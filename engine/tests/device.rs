// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Tests des Geraetezugriffs, `docs/FORMAT.md` Abschnitt 3.
//!
//! Als Unterbau dient eine gewoehnliche Datei. Das ist keine Attrappe: Der
//! Code sieht dieselbe Schnittstelle wie bei einem Blockgeraet, und die
//! Unterschiede — Sektorgroesse, `O_DIRECT`, ehrliches Flush — betreffen ihn
//! an keiner Stelle, die hier geprueft wird. Was nur mit einem echten
//! Blockgeraet geht, steht in `loop_device.rs` und braucht Root.

use std::fs::File;
use std::path::{Path, PathBuf};

use ferrite_engine::{read_superblock, write_superblock, EngineError, MemberDevice};
use ferrite_format::superblock::{
    MemberState, Role, Superblock, MIN_DEVICE_SIZE, SUPERBLOCK_PRIMARY_OFFSET, SUPERBLOCK_SIZE,
};
use ferrite_format::{FormatError, Uuid};

/// Eine Datei, die sich nach dem Test selbst wegraeumt.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str, size: u64) -> Self {
        let path = std::env::temp_dir().join(format!("ferrite-{name}-{}.img", std::process::id()));
        let file = File::create(&path).expect("Datei anlegen");
        file.set_len(size).expect("Groesse setzen");
        Scratch(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn open(&self) -> MemberDevice {
        MemberDevice::open(&self.0).expect("Member oeffnen")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

const PAYLOAD_BLOCKS: u64 = 8;
const BLOCK: u64 = 64 * 1024;
/// Kleinstes Geraet, auf das `sample()` passt.
const DEVICE_SIZE: u64 = 1_048_576 + PAYLOAD_BLOCKS * BLOCK + 65_536;

fn sample(role: Role, slot_index: u16) -> Superblock {
    let mut superblock = Superblock::new(
        Uuid::from_random_bytes([0xD1; 16]),
        Uuid::from_random_bytes([0xE2; 16]),
        role,
        4,
        PAYLOAD_BLOCKS * BLOCK,
    );
    superblock.slot_index = slot_index;
    superblock.label = "geraet".to_string();
    superblock
}

// --- Groesse und Bereichspruefung ----------------------------------------

#[test]
fn the_size_comes_from_the_device_itself() {
    let scratch = Scratch::new("size", DEVICE_SIZE);
    assert_eq!(scratch.open().size(), DEVICE_SIZE);
}

#[test]
fn a_read_past_the_end_is_refused() {
    // Kein kurzer Read, kein Teilerfolg. Wer die Haelfte eines Superblocks
    // bekommt und weiterrechnet, prueft eine Pruefsumme ueber halben Muell.
    let scratch = Scratch::new("past-end", MIN_DEVICE_SIZE);
    let device = scratch.open();
    let mut buffer = [0u8; 4096];

    assert_eq!(
        device.read_at(MIN_DEVICE_SIZE - 1, &mut buffer),
        Err(EngineError::BeyondDevice {
            offset: MIN_DEVICE_SIZE - 1,
            len: 4096,
            size: MIN_DEVICE_SIZE
        })
    );
    // Genau bis ans Ende ist erlaubt.
    device.read_at(MIN_DEVICE_SIZE - 4096, &mut buffer).unwrap();
}

#[test]
fn an_offset_that_overflows_is_refused() {
    let scratch = Scratch::new("overflow", MIN_DEVICE_SIZE);
    let device = scratch.open();
    let mut buffer = [0u8; 16];
    assert!(matches!(
        device.read_at(u64::MAX - 4, &mut buffer),
        Err(EngineError::OffsetOverflow { .. })
    ));
}

#[test]
fn a_read_only_member_refuses_writes() {
    let scratch = Scratch::new("readonly", MIN_DEVICE_SIZE);
    let device = MemberDevice::open_read_only(scratch.path()).unwrap();
    assert!(!device.is_writable());
    assert_eq!(
        device.write_at(0, &[0u8; 16]),
        Err(EngineError::NotWritable)
    );
}

#[test]
fn opening_something_that_is_not_there_reports_the_reason() {
    let missing = std::env::temp_dir().join("ferrite-gibt-es-nicht.img");
    assert!(matches!(
        MemberDevice::open(&missing),
        Err(EngineError::Io {
            what: "Member oeffnen",
            kind: std::io::ErrorKind::NotFound,
            ..
        })
    ));
}

// --- Lesen und Schreiben --------------------------------------------------

#[test]
fn what_was_written_comes_back() {
    let scratch = Scratch::new("roundtrip", DEVICE_SIZE);
    let device = scratch.open();

    let pattern: Vec<u8> = (0u8..=255).cycle().take(8192).collect();
    device.write_at(1_048_576, &pattern).unwrap();
    device.flush().unwrap();

    let mut read_back = vec![0u8; 8192];
    device.read_at(1_048_576, &mut read_back).unwrap();
    assert_eq!(read_back, pattern);

    // Und daneben steht weiterhin nichts.
    let mut untouched = vec![0xFFu8; 4096];
    device.read_at(1_048_576 + 8192, &mut untouched).unwrap();
    assert!(untouched.iter().all(|&byte| byte == 0));
}

// --- Superbloecke ---------------------------------------------------------

#[test]
fn both_superblocks_are_written_and_read_back() {
    let scratch = Scratch::new("superblock", DEVICE_SIZE);
    let device = scratch.open();
    let superblock = sample(Role::Data, 2);

    write_superblock(&device, &superblock).unwrap();
    assert_eq!(read_superblock(&device).unwrap(), superblock);

    // Beide Kopien tragen wirklich dasselbe — nicht nur die, die `select`
    // zufaellig zuerst ansieht.
    let mut primary = [0u8; SUPERBLOCK_SIZE];
    let mut backup = [0u8; SUPERBLOCK_SIZE];
    device
        .read_at(SUPERBLOCK_PRIMARY_OFFSET, &mut primary)
        .unwrap();
    device.read_at(DEVICE_SIZE - 65_536, &mut backup).unwrap();
    assert_eq!(primary, backup);
    assert_eq!(&primary[..8], b"FERRITE1");
}

#[test]
fn a_destroyed_primary_is_survived_by_the_backup() {
    // Abschnitt 3: Beim Lesen gilt der Superblock mit gueltiger Pruefsumme.
    // Genau dafuer liegt er zweimal auf dem Geraet.
    let scratch = Scratch::new("torn-primary", DEVICE_SIZE);
    let device = scratch.open();
    let superblock = sample(Role::ParityP, 0);
    write_superblock(&device, &superblock).unwrap();

    device
        .write_at(SUPERBLOCK_PRIMARY_OFFSET, &[0xFFu8; 512])
        .unwrap();
    device.flush().unwrap();

    assert_eq!(read_superblock(&device).unwrap(), superblock);
}

#[test]
fn a_destroyed_backup_is_survived_by_the_primary() {
    let scratch = Scratch::new("torn-backup", DEVICE_SIZE);
    let device = scratch.open();
    let superblock = sample(Role::Data, 0);
    write_superblock(&device, &superblock).unwrap();

    device.write_at(DEVICE_SIZE - 65_536, &[0u8; 512]).unwrap();
    device.flush().unwrap();

    assert_eq!(read_superblock(&device).unwrap(), superblock);
}

#[test]
fn a_device_without_any_superblock_reports_bad_magic() {
    // Eine fremde oder frische Platte. `BadMagic` und nicht
    // `ChecksumMismatch` — die Diagnose soll sagen, was los ist.
    let scratch = Scratch::new("fremd", DEVICE_SIZE);
    assert!(matches!(
        read_superblock(&scratch.open()),
        Err(EngineError::Format(FormatError::BadMagic { .. }))
    ));
}

#[test]
fn the_newer_generation_wins() {
    let scratch = Scratch::new("generation", DEVICE_SIZE);
    let device = scratch.open();

    let mut old = sample(Role::Data, 1);
    old.generation = 7;
    write_superblock(&device, &old).unwrap();

    // Nur den primaeren aktualisieren, so wie es aussieht, wenn der Strom
    // zwischen den beiden Schreibvorgaengen ausfaellt.
    let mut new = old.clone();
    new.generation = 8;
    device
        .write_at(SUPERBLOCK_PRIMARY_OFFSET, &new.encode().unwrap())
        .unwrap();
    device.flush().unwrap();

    assert_eq!(read_superblock(&device).unwrap().generation, 8);
}

#[test]
fn a_superblock_that_does_not_fit_is_refused_before_anything_is_written() {
    // Bedingung 2 aus Abschnitt 3. Der Fehler muss kommen, *bevor* etwas auf
    // dem Geraet steht — sonst laege dort ein Superblock, der ueber seinen
    // eigenen Backup hinausreicht.
    let scratch = Scratch::new("zu-gross", MIN_DEVICE_SIZE + 65_536);
    let device = scratch.open();
    let superblock = sample(Role::Data, 0);

    assert!(matches!(
        write_superblock(&device, &superblock),
        Err(EngineError::Format(FormatError::InvalidField {
            field: "payload_size",
            ..
        }))
    ));

    let mut untouched = [0xAAu8; 64];
    device
        .read_at(SUPERBLOCK_PRIMARY_OFFSET, &mut untouched)
        .unwrap();
    assert!(
        untouched.iter().all(|&byte| byte == 0),
        "es wurde nichts geschrieben"
    );
}

#[test]
fn the_member_state_survives_the_round_trip_to_the_device() {
    // Die Felder aus Abschnitt 4.2, diesmal ueber ein Geraet statt nur ueber
    // einen Puffer.
    let scratch = Scratch::new("member-state", DEVICE_SIZE);
    let device = scratch.open();

    let mut superblock = sample(Role::Data, 3);
    superblock.member_state = MemberState::Rebuilding;
    superblock.rebuild_progress = 3 * BLOCK;
    write_superblock(&device, &superblock).unwrap();

    let back = read_superblock(&device).unwrap();
    assert_eq!(back.member_state, MemberState::Rebuilding);
    assert_eq!(back.rebuild_progress, 3 * BLOCK);
}
