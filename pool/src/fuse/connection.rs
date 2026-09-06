// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Die Verbindung zum Kernel: `/dev/fuse` und `mount(2)`.
//!
//! # Warum ohne `fusermount3`
//!
//! Der uebliche Weg laeuft ueber das setuid-Programm `fusermount3` aus
//! libfuse: Es haengt ein und reicht den Dateideskriptor ueber einen
//! Unix-Socket zurueck. Das ist da richtig, wo ein unprivilegierter Nutzer
//! einhaengen soll.
//!
//! Ferrite haengt seinen Pool als Systemdienst ein, also als Root. Dann ist
//! `mount(2)` der direkte Weg: eine Abhaengigkeit weniger, ein setuid-Programm
//! weniger im Pfad, und der Fehlerfall ist ein `errno` statt der
//! Standardfehlerausgabe eines Kindprozesses.
//!
//! # Die Eigenheiten von `/dev/fuse`
//!
//! * **Ein `read` liefert genau eine Anfrage**, nie einen Teil und nie zwei.
//!   Der Puffer muss deshalb gross genug fuer die groesste sein — sonst
//!   antwortet der Kernel mit `EINVAL` und die Anfrage ist verloren.
//! * **`ENODEV` heisst ausgehaengt.** Das ist das Ende der Schleife und kein
//!   Fehler.
//! * **`ENOENT` heisst abgebrochen.** Die Anfrage wurde zurueckgezogen,
//!   waehrend wir sie holten. Weitermachen.
//! * **`EINTR` heisst nichts.** Ein Signal kam dazwischen.

use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::error::{PoolError, Result};
use crate::fuse::abi::{FUSE_MIN_READ_BUFFER, OUT_HEADER_SIZE};

/// Groesse des Lesepuffers.
///
/// Ein Write darf so gross sein wie `max_write`; dazu kommen Kopf und
/// Argumente. `FUSE_MIN_READ_BUFFER` ist die Untergrenze des Kernels.
pub const READ_BUFFER: usize = MAX_WRITE as usize + 4096;

/// Groesster Write, den dieser Server annimmt.
pub const MAX_WRITE: u32 = 1 << 20;

const _: () = assert!(READ_BUFFER >= FUSE_MIN_READ_BUFFER);

/// Optionen fuer den Mount.
#[derive(Debug, Clone)]
pub struct MountOptions {
    /// Duerfen andere Nutzer als der Einhaengende den Pool sehen?
    ///
    /// Fuer ein NAS ja — Samba und NFS laufen unter eigenen Nutzern. Als
    /// Schalter und nicht fest, weil es die Sichtbarkeit erweitert und das
    /// eine Entscheidung des Betreibers ist.
    pub allow_other: bool,
    /// Name, unter dem der Mount in `/proc/mounts` erscheint.
    pub source: String,
}

impl Default for MountOptions {
    fn default() -> Self {
        MountOptions {
            allow_other: true,
            source: "ferrite-pool".to_string(),
        }
    }
}

/// Eine offene FUSE-Verbindung samt ihrem Einhaengepunkt.
#[derive(Debug)]
pub struct Connection {
    device: OwnedFd,
    mountpoint: CString,
    mounted: bool,
}

impl Connection {
    /// Oeffnet `/dev/fuse` und haengt den Pool ein.
    pub fn mount(mountpoint: &Path, options: &MountOptions) -> Result<Self> {
        // Ab hier ist dieser Prozess ein Dateisystem, und ein Dateisystem hat
        // keine eigene `umask`. Der Kernel hat die des Aufrufers auf den Modus
        // schon angewandt, bevor er ihn schickt; wuerde sie beim `open` oder
        // `mkdir` ein zweites Mal wirken, bekaeme jede neue Datei weniger
        // Rechte als bestellt. libfuse macht an derselben Stelle dasselbe.
        unsafe { libc::umask(0) };

        let device = open_device()?;
        let target = c_path(mountpoint)?;

        // `default_permissions` laesst den Kernel die Rechte anhand der
        // Attribute pruefen, die dieser Server liefert. Ohne das muesste jede
        // Operation die Rechte selbst pruefen — und eine vergessene Pruefung
        // waere ein Loch, das niemand sieht.
        let mut data = format!(
            "fd={},rootmode=40000,user_id={},group_id={},default_permissions",
            device.as_raw_fd(),
            unsafe { libc::getuid() },
            unsafe { libc::getgid() },
        );
        if options.allow_other {
            data.push_str(",allow_other");
        }

        let source = CString::new(options.source.as_str()).map_err(|_| PoolError::InvalidPath {
            reason: "Nullbyte im Quellnamen",
        })?;
        let fstype = CString::new("fuse.ferrite").expect("konstant und ohne Nullbyte");
        let data = CString::new(data).expect("aus Zahlen gebaut, ohne Nullbyte");

        // MS_NOSUID und MS_NODEV: Auf einem Pool, in den jeder Nutzer
        // schreiben darf, haette eine setuid-Datei oder ein Geraeteknoten
        // nichts zu suchen.
        let flags = libc::MS_NOSUID | libc::MS_NODEV;
        let result = unsafe {
            libc::mount(
                source.as_ptr(),
                target.as_ptr(),
                fstype.as_ptr(),
                flags,
                data.as_ptr().cast(),
            )
        };
        if result != 0 {
            return Err(last_error("Pool einhaengen"));
        }

        Ok(Connection {
            device,
            mountpoint: target,
            mounted: true,
        })
    }

    pub fn fd(&self) -> RawFd {
        self.device.as_raw_fd()
    }

    /// Holt die naechste Anfrage.
    ///
    /// `Ok(None)` heisst: ausgehaengt, die Schleife ist zu Ende.
    pub fn next_request(&self, buffer: &mut [u8]) -> Result<Option<usize>> {
        loop {
            let read = unsafe {
                libc::read(
                    self.device.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            if read >= 0 {
                return Ok(Some(read as usize));
            }
            match errno() {
                libc::ENODEV => return Ok(None),
                // Die Anfrage wurde zurueckgezogen, waehrend wir sie holten.
                libc::ENOENT | libc::EINTR | libc::EAGAIN => continue,
                _ => return Err(last_error("Anfrage lesen")),
            }
        }
    }

    /// Antwortet auf eine Anfrage.
    ///
    /// `error` ist **negativ**, so wie der Kernel es erwartet: `-libc::ENOENT`
    /// und nicht `libc::ENOENT`. Ein Vorzeichen, das mal so und mal anders
    /// herum gilt, ist eine Fehlerquelle fuer sich — deshalb steht es hier in
    /// der Signatur und wird nirgends unterwegs gedreht.
    pub fn reply(&self, unique: u64, error: i32, payload: &[u8]) -> Result<()> {
        debug_assert!(error <= 0, "der Kernel erwartet Fehler negativ");
        let len = OUT_HEADER_SIZE + payload.len();
        let mut head = Vec::with_capacity(len);
        head.extend_from_slice(&(len as u32).to_ne_bytes());
        head.extend_from_slice(&error.to_ne_bytes());
        head.extend_from_slice(&unique.to_ne_bytes());
        head.extend_from_slice(payload);

        let written =
            unsafe { libc::write(self.device.as_raw_fd(), head.as_ptr().cast(), head.len()) };
        if written < 0 {
            // Der Kernel wirft eine Antwort weg, deren Anfrage abgebrochen
            // wurde. Das ist normal und kein Grund, die Schleife zu beenden.
            return match errno() {
                libc::ENOENT | libc::ENODEV => Ok(()),
                _ => Err(last_error("Antwort schreiben")),
            };
        }
        Ok(())
    }

    /// Haengt aus.
    ///
    /// `MNT_DETACH`: Ein Pool, den noch jemand offen hat, laesst sich sonst
    /// nicht aushaengen, und ein Dienst, der sich wegen eines fremden
    /// Arbeitsverzeichnisses nicht beenden laesst, ist schlimmer als ein
    /// spaeter geloester Mount.
    pub fn unmount(&mut self) -> Result<()> {
        if !self.mounted {
            return Ok(());
        }
        self.mounted = false;
        let result = unsafe { libc::umount2(self.mountpoint.as_ptr(), libc::MNT_DETACH) };
        if result != 0 {
            return Err(last_error("Pool aushaengen"));
        }
        Ok(())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // Ohne das bliebe ein Einhaengepunkt stehen, dessen Server nicht mehr
        // lebt — jeder Zugriff darauf haengt dann bis zum naechsten Neustart.
        let _ = self.unmount();
    }
}

fn open_device() -> Result<OwnedFd> {
    let path = CString::new("/dev/fuse").expect("konstant und ohne Nullbyte");
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(last_error("/dev/fuse oeffnen"));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn c_path(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| PoolError::InvalidPath {
        reason: "Nullbyte im Einhaengepunkt",
    })
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn last_error(what: &'static str) -> PoolError {
    let error = std::io::Error::last_os_error();
    PoolError::Io {
        what,
        kind: error.kind(),
        raw_os_error: error.raw_os_error(),
    }
}

// --- Passthrough ----------------------------------------------------------

/// Hinterlegt einen Dateideskriptor beim Kernel und liefert seine
/// `backing_id`.
///
/// Danach darf eine Antwort auf `OPEN` mit [`FOPEN_PASSTHROUGH`] auf ihn
/// zeigen, und der Kernel bedient Lesen und Schreiben unmittelbar aus dieser
/// Datei — dieser Server sieht sie nicht mehr.
///
/// # Warum ein Fehlschlag keiner ist
///
/// Auf einem Kernel vor 6.9 gibt es dieses `ioctl` nicht, und auch sonst kann
/// es scheitern — etwa wenn zu viele Dateien gleichzeitig hinterlegt sind.
/// Der Aufrufer faellt dann auf den gewoehnlichen Weg zurueck: langsamer,
/// aber richtig. Deshalb `Option` und kein `Result`; ein Fehler ist hier eine
/// Auskunft und kein Abbruch.
pub fn backing_open(connection: &Connection, backing: RawFd) -> Option<i32> {
    let map = crate::fuse::abi::backing_map(backing).into_bytes();
    // SAFETY: `map` ist `BACKING_MAP_SIZE` gross, genau die Groesse, die in
    // der Ioctl-Nummer steht — der Kernel liest nicht darueber hinaus.
    let id = unsafe {
        libc::ioctl(
            connection.fd(),
            crate::fuse::abi::FUSE_DEV_IOC_BACKING_OPEN as libc::Ioctl,
            map.as_ptr(),
        )
    };
    (id > 0).then_some(id)
}

/// Gibt eine `backing_id` wieder frei.
///
/// **Muss zu jedem [`backing_open`] kommen.** Der Kernel haelt sonst einen
/// Verweis auf die Datei, bis der Pool ausgehaengt wird — bei einem Server,
/// der Monate laeuft, ist das ein Leck, das erst beim Aufraeumen auffaellt.
/// Der Rueckgabewert sagt, ob der Kernel die `backing_id` kannte. `false`
/// heisst: Sie war schon zu oder hat nie existiert — ein Buchhaltungsfehler
/// hier, kein Fehler des Aufrufers.
pub fn backing_close(connection: &Connection, id: i32) -> bool {
    // SAFETY: `id` ist ein `u32`, und die Ioctl-Nummer sagt genau vier Bytes.
    let result = unsafe {
        libc::ioctl(
            connection.fd(),
            crate::fuse::abi::FUSE_DEV_IOC_BACKING_CLOSE as libc::Ioctl,
            &id as *const i32,
        )
    };
    result == 0
}
