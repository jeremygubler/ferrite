// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Zugriff auf einen Member, `docs/FORMAT.md` Abschnitt 3.
//!
//! Die erste Stelle im Projekt, die etwas oeffnet. Sie kommt mit `std` aus:
//! positioniertes Lesen und Schreiben gibt es dort, und die Groesse eines
//! Blockgeraets liefert ein `seek` ans Ende — dafuer braucht es keinen ioctl
//! und damit keine Dependency.
//!
//! **Warum das hier nicht hinter `#[cfg(target_os = "linux")]` steht**, obwohl
//! der Kickoff das vorsah: Der Grund fuer die Trennung war, dass `cargo test`
//! ueberall laufen soll. Genau das ist erfuellt — eine Datei verhaelt sich fuer
//! diesen Code wie ein Blockgeraet, und beide Wege sind auf jeder Plattform
//! getestet. Linux-spezifisch wird erst das ublk-Target; die Grenze gehoert
//! dorthin und nicht schon hierher.
//!
//! Kein `O_DIRECT`. Es verlangt ausgerichtete Puffer im Speicher, und die ohne
//! `libc` selbst zu verwalten waere Aufwand fuer eine Optimierung, die noch
//! niemand gemessen hat. Erst korrekt, dann schnell — und dann mit Benchmark.

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};

use ferrite_format::superblock::{Superblock, SUPERBLOCK_PRIMARY_OFFSET, SUPERBLOCK_SIZE};

use crate::error::{io_error, EngineError, Result};

/// Ein geoeffneter Member: Blockgeraet oder Datei, die eines nachbildet.
#[derive(Debug)]
pub struct MemberDevice {
    file: File,
    size: u64,
    path: PathBuf,
    writable: bool,
}

impl MemberDevice {
    /// Oeffnet zum Lesen und Schreiben.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(path, true)
    }

    /// Oeffnet nur zum Lesen — fuer alles, was ein fremdes oder als
    /// unbrauchbar gemeldetes Geraet nur ansehen will.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(path, false)
    }

    fn open_with(path: impl AsRef<Path>, writable: bool) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut file = OpenOptions::new()
            .read(true)
            .write(writable)
            .open(&path)
            .map_err(io_error("Member oeffnen"))?;

        // Bei einer Datei stimmt die Metadaten-Laenge, bei einem Blockgeraet
        // ist sie null — `seek` ans Ende liefert in beiden Faellen die
        // nutzbare Groesse, ohne ioctl und ohne Dependency.
        let size = file
            .seek(SeekFrom::End(0))
            .map_err(io_error("Geraetegroesse ermitteln"))?;

        Ok(MemberDevice {
            file,
            size,
            path,
            writable,
        })
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_writable(&self) -> bool {
        self.writable
    }

    /// Liest genau `buffer.len()` Bytes ab `offset`.
    ///
    /// Ein kurzer Read ist ein Fehler und kein Teilerfolg: Wer die Haelfte
    /// eines Superblocks bekommt und weiterrechnet, prueft eine Pruefsumme
    /// ueber halben Muell.
    pub fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        self.check_range(offset, buffer.len())?;
        read_exact_at(&self.file, offset, buffer).map_err(io_error("Member lesen"))
    }

    /// Schreibt genau `data.len()` Bytes ab `offset`.
    pub fn write_at(&self, offset: u64, data: &[u8]) -> Result<()> {
        if !self.writable {
            return Err(EngineError::NotWritable);
        }
        self.check_range(offset, data.len())?;
        crash_point();
        write_all_at(&self.file, offset, data).map_err(io_error("Member schreiben"))
    }

    /// Sorgt dafuer, dass alles Geschriebene die Platte erreicht hat.
    ///
    /// `sync_data` und nicht `sync_all`: Die Metadaten der Datei interessieren
    /// nicht, ihre Groesse aendert sich nie. Ob das Geraet den Flush ehrlich
    /// beantwortet, ist eine andere Frage — die stellt Abschnitt 5.3.
    pub fn flush(&self) -> Result<()> {
        crash_point();
        self.file.sync_data().map_err(io_error("Member flushen"))
    }

    fn check_range(&self, offset: u64, len: usize) -> Result<()> {
        let end = offset
            .checked_add(len as u64)
            .ok_or(EngineError::OffsetOverflow {
                offset,
                len: len as u64,
            })?;
        if end > self.size {
            return Err(EngineError::BeyondDevice {
                offset,
                len: len as u64,
                size: self.size,
            });
        }
        Ok(())
    }
}

/// Offset des Backup-Superblocks auf einem Geraet dieser Groesse.
fn backup_offset(size: u64) -> Result<u64> {
    Superblock::backup_offset(size).ok_or(EngineError::Format(
        ferrite_format::FormatError::InvalidField {
            field: "device_size",
            reason: "zu klein fuer den Backup-Superblock",
        },
    ))
}

/// Liest den Superblock so, wie Abschnitt 3 es vorschreibt.
///
/// Beide Kopien werden gelesen, es gewinnt die mit gueltiger Pruefsumme und
/// hoeherer `generation`. Ist nur eine lesbar, gilt sie — genau dafuer gibt es
/// die zweite.
pub fn read_superblock(device: &MemberDevice) -> Result<Superblock> {
    let mut primary = [0u8; SUPERBLOCK_SIZE];
    let mut backup = [0u8; SUPERBLOCK_SIZE];

    // Ein Lesefehler auf einer der beiden Kopien ist noch kein Grund
    // aufzugeben: Genau dafuer liegt sie doppelt. Erst wenn beide unbrauchbar
    // sind, meldet `select` das.
    let _ = device.read_at(SUPERBLOCK_PRIMARY_OFFSET, &mut primary);
    if let Ok(offset) = backup_offset(device.size()) {
        let _ = device.read_at(offset, &mut backup);
    }

    Superblock::select(&primary, &backup).map_err(EngineError::Format)
}

/// Schreibt beide Superbloecke, **primaer zuerst, Backup nach einem Flush**.
///
/// Die Reihenfolge ist der Grund, warum es zwei Kopien gibt. Wer sie umdreht
/// oder ohne Flush dazwischen schreibt, kann bei einem Stromausfall beide auf
/// einmal verlieren — und dann ist der Member nicht mehr zuzuordnen.
pub fn write_superblock(device: &MemberDevice, superblock: &Superblock) -> Result<()> {
    superblock
        .fits_on_device(device.size())
        .map_err(EngineError::Format)?;
    let encoded = superblock.encode().map_err(EngineError::Format)?;

    device.write_at(SUPERBLOCK_PRIMARY_OFFSET, &encoded)?;
    device.flush()?;

    device.write_at(backup_offset(device.size())?, &encoded)?;
    device.flush()?;
    Ok(())
}

// --- Positioniertes Lesen und Schreiben ----------------------------------
//
// `pread`/`pwrite` statt `seek` plus `read`: Der Offset gehoert zur Operation
// und nicht zu einem Zustand, den sich zwei Aufrufer teilen. Das ist auch die
// Form, die das ublk-Target spaeter braucht.

#[cfg(unix)]
pub(crate) fn read_exact_at(file: &File, offset: u64, buffer: &mut [u8]) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buffer, offset)
}

#[cfg(unix)]
pub(crate) fn write_all_at(file: &File, offset: u64, data: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(data, offset)
}

#[cfg(windows)]
pub(crate) fn read_exact_at(file: &File, offset: u64, buffer: &mut [u8]) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut done = 0usize;
    while done < buffer.len() {
        match file.seek_read(&mut buffer[done..], offset + done as u64) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Geraet endet vor dem angeforderten Bereich",
                ))
            }
            Ok(n) => done += n,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn write_all_at(file: &File, offset: u64, data: &[u8]) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut done = 0usize;
    while done < data.len() {
        match file.seek_write(&data[done..], offset + done as u64) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "Geraet nimmt keine Bytes mehr an",
                ))
            }
            Ok(n) => done += n,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

// --- Abbruchpunkt fuer das Crash-Harness ---------------------------------
//
// Hier steht der einzige Ort, an dem dieses Crate schreibt und flusht — und
// damit der einzige, an dem ein Stromausfall etwas anrichten kann. Ohne das
// Feature `crash-points` ist die Funktion leer und verschwindet im Optimierer.

#[cfg(all(target_os = "linux", feature = "crash-points"))]
#[inline]
fn crash_point() {
    crate::crash::before_io();
}

#[cfg(not(all(target_os = "linux", feature = "crash-points")))]
#[inline(always)]
fn crash_point() {}
