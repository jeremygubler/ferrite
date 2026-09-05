// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Das Crash-Harness, Meilenstein 3.
//!
//! # Was hier bewiesen wird
//!
//! Ein Speicherprojekt gewinnt Vertrauen nicht ueber Funktionsumfang, sondern
//! darueber, dass es beim Stromausfall nichts verliert. Dieses Crate bricht den
//! Schreibpfad an **jedem einzelnen** I/O-Punkt ab und prueft danach drei
//! Zusagen:
//!
//! 1. **Das Array laesst sich oeffnen.** Superbloecke gueltig, `assemble` geht
//!    durch. Ein Array, das nach einem Absturz nicht mehr zusammenfindet, ist
//!    verloren, egal wie gut die Daten darauf noch waeren.
//! 2. **Nach dem Recovery passt die Paritaet zum Inhalt der Data-Members.**
//!    Stimmt sie nicht, rekonstruiert der naechste Plattenausfall Muell — und
//!    zwar lautlos.
//! 3. **Kein bestaetigter Write geht verloren.** Was der Schreibpfad
//!    zurueckgemeldet hat, steht danach noch da.
//!
//! # Warum durchgezaehlt statt gewuerfelt
//!
//! Das Harness zaehlt in einem Vorlauf, wieviele I/O-Operationen der Ablauf
//! braucht, und bricht dann bei 1, 2, 3, … ab. Damit ist jeder Punkt abgedeckt
//! statt einer Stichprobe, und ein Fehlschlag laesst sich mit derselben Zahl
//! exakt wiederholen. Ein zufaelliger Abbruch findet denselben Fehler
//! vielleicht — nachstellen kann ihn danach niemand.
//!
//! # Die Grenze, die dieses Harness hat
//!
//! Der Abbruch faellt zwischen zwei I/O-Operationen, nie mitten in eine. Ein
//! echter Stromausfall kann einen Sektor halb geschrieben zuruecklassen. Diese
//! Luecke deckt `dm-flakey` mit `drop_writes` ab; sie steht hier, damit niemand
//! den Nachweis fuer vollstaendiger haelt, als er ist.

use std::path::{Path, PathBuf};

use ferrite_engine::{
    member_for, write_superblock, ArrayWriter, DeviceLog, EngineError, Member, MemberDevice, Result,
};
use ferrite_format::superblock::{Role, Superblock, DEFAULT_PAYLOAD_OFFSET};
use ferrite_format::Uuid;

/// Blockgroesse des Arrays. Klein genug, dass ein Lauf schnell ist, und ein
/// Vielfaches der Sektorgroesse des Logs.
pub const BLOCK: u64 = 64 * 1024;

/// Payload je Data- und Parity-Member.
pub const PAYLOAD: u64 = 8 * BLOCK;

/// Payload des Log-Members.
pub const LOG_PAYLOAD: u64 = 4 * BLOCK;

/// Zahl der Data-Slots.
pub const SLOTS: u16 = 3;

/// Groesse einer Geraetedatei fuer eine gegebene Payload.
pub fn device_size(payload: u64) -> u64 {
    DEFAULT_PAYLOAD_OFFSET + payload + 65_536
}

/// Die Dateinamen der Members in fester Reihenfolge: Data 0..n, P, Q, Log.
pub fn member_files(directory: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = (0..SLOTS)
        .map(|slot| directory.join(format!("data{slot}.img")))
        .collect();
    files.push(directory.join("parity-p.img"));
    files.push(directory.join("parity-q.img"));
    files.push(directory.join("log.img"));
    files
}

fn superblock(role: Role, slot_index: u16, payload: u64) -> Superblock {
    let mut superblock = Superblock::new(
        Uuid::from_random_bytes([0xC5; 16]),
        // Die Member-UUID muss je Member verschieden sein (Regel 3 aus
        // Abschnitt 2.1). Rolle und Slot zusammen sind eindeutig.
        Uuid::from_random_bytes([role as u8 * 16 + slot_index as u8 + 1; 16]),
        role,
        SLOTS as u32,
        payload,
    );
    superblock.slot_index = slot_index;
    superblock
}

/// Legt die Geraetedateien an und initialisiert das Array.
///
/// Danach steht ein leeres, gueltiges Array auf der Platte: Superbloecke
/// geschrieben, Log-Region genullt, Paritaet passend (alles null).
/// Legt ein Array auf den angegebenen Geraeten an.
///
/// Die Reihenfolge ist die von [`member_files`]: Data 0..n, P, Q, Log. Die
/// Geraete muessen bereits die noetige Groesse haben — hier wird nichts mehr
/// angelegt, denn ein `/dev/mapper`-Eintrag laesst sich nicht `set_len`.
pub fn create_on(paths: &[PathBuf]) -> Result<()> {
    for (index, path) in paths.iter().enumerate() {
        let device = MemberDevice::open(path)?;
        let superblock = match index {
            index if index < usize::from(SLOTS) => superblock(Role::Data, index as u16, PAYLOAD),
            index if index == usize::from(SLOTS) => superblock(Role::ParityP, 0, PAYLOAD),
            index if index == usize::from(SLOTS) + 1 => superblock(Role::ParityQ, 0, PAYLOAD),
            _ => superblock(Role::Log, 0, LOG_PAYLOAD),
        };
        write_superblock(&device, &superblock)?;
    }

    // Die Log-Region nullen, sonst faende der erste Scan, was das Geraet
    // vorher trug.
    let log_device = MemberDevice::open(&paths[usize::from(SLOTS) + 2])?;
    DeviceLog::initialize(log_device, &superblock(Role::Log, 0, LOG_PAYLOAD))?;
    Ok(())
}

/// Legt die Geraetedateien in einem Verzeichnis an und initialisiert das Array.
pub fn create(directory: &Path) -> Result<()> {
    std::fs::create_dir_all(directory).map_err(|error| EngineError::Io {
        what: "Verzeichnis anlegen",
        kind: error.kind(),
        raw_os_error: error.raw_os_error(),
    })?;

    for (index, path) in member_files(directory).iter().enumerate() {
        let payload = if index == usize::from(SLOTS) + 2 {
            LOG_PAYLOAD
        } else {
            PAYLOAD
        };
        let file = std::fs::File::create(path).map_err(|error| EngineError::Io {
            what: "Geraetedatei anlegen",
            kind: error.kind(),
            raw_os_error: error.raw_os_error(),
        })?;
        file.set_len(device_size(payload))
            .map_err(|error| EngineError::Io {
                what: "Geraetegroesse setzen",
                kind: error.kind(),
                raw_os_error: error.raw_os_error(),
            })?;
    }

    create_on(&member_files(directory))
}

/// Oeffnet das Array und spielt das Log zurueck.
///
/// Gibt den Schreibpfad und die Zahl der beim Recovery angewendeten Writes
/// zurueck. Die Superbloecke kommen von der Platte — nach einem Absturz ist
/// das das Einzige, was noch da ist.
pub fn open(directory: &Path) -> Result<(ArrayWriter, u64)> {
    open_on(&member_files(directory))
}

/// Oeffnet ein Array auf den angegebenen Geraeten und spielt das Log zurueck.
pub fn open_on(files: &[PathBuf]) -> Result<(ArrayWriter, u64)> {
    let log_device = MemberDevice::open(&files[usize::from(SLOTS) + 2])?;
    let log_superblock = ferrite_engine::read_superblock(&log_device)?;
    let (log, recovery) = DeviceLog::open(log_device, &log_superblock)?;

    let data: Result<Vec<Member>> = (0..SLOTS)
        .map(|slot| {
            let device = MemberDevice::open(&files[usize::from(slot)])?;
            let superblock = ferrite_engine::read_superblock(&device)?;
            member_for(device, &superblock, Role::Data)
        })
        .collect();

    let p_device = MemberDevice::open(&files[usize::from(SLOTS)])?;
    let p_superblock = ferrite_engine::read_superblock(&p_device)?;
    let parity_p = member_for(p_device, &p_superblock, Role::ParityP)?;

    let q_device = MemberDevice::open(&files[usize::from(SLOTS) + 1])?;
    let q_superblock = ferrite_engine::read_superblock(&q_device)?;
    let parity_q = member_for(q_device, &q_superblock, Role::ParityQ)?;

    let mut writer = ArrayWriter::new(log, data?, parity_p, Some(parity_q))?;
    let applied = writer.recover(&recovery)?;
    Ok((writer, applied))
}

/// Ein Write, den der Arbeiter ausfuehrt.
///
/// Deterministisch aus seiner Nummer gebildet: Wer die Nummer kennt, kennt
/// Slot, Offset und Inhalt. Damit laesst sich nach einem Absturz pruefen, was
/// dastehen muss, ohne dass der Pruefer den Ablauf noch einmal nachspielt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedWrite {
    pub slot_index: u16,
    pub offset: u64,
    pub data: Vec<u8>,
}

/// Der Ablauf, den der Arbeiter abarbeitet.
///
/// Die Writes ueberlappen sich absichtlich: Zwei Writes auf denselben Bereich
/// zwingen den Schreibpfad zum Fortschreiben mit gelesenem Vorzustand, und das
/// ist der Teil, der nach einem Absturz falsch werden kann.
pub fn plan(count: u64) -> Vec<PlannedWrite> {
    (0..count)
        .map(|nth| {
            let slot_index = (nth % u64::from(SLOTS)) as u16;
            // Zwei Runden ueber dieselben Offsets, damit jeder Bereich einmal
            // beschrieben und einmal ueberschrieben wird.
            let offset = (nth % 4) * BLOCK + (nth % 2) * 4096;
            let len = 4096 + (nth as usize % 3) * 1024;
            let data = (0..len)
                .map(|index| (index as u8).wrapping_mul(37) ^ (nth as u8).wrapping_add(1))
                .collect();
            PlannedWrite {
                slot_index,
                offset,
                data,
            }
        })
        .collect()
}
