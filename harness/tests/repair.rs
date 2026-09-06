// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Der Repair-Broker an einem echten Array, Meilenstein 4.
//!
//! Hier laeuft die ganze Kette einmal durch: eine Kernelzeile, wie btrfs sie
//! nach einem Scrub hinterlaesst, hinein — und ein wiederhergestellter Bereich
//! auf der Platte heraus.
//!
//! # Was dieser Test nicht ist
//!
//! Er startet kein btrfs. Die Zeilen sind nachgebildet, und das ist der eine
//! Punkt, an dem hier nicht die Wirklichkeit geprueft wird. Der Nachweis, dass
//! btrfs sie wirklich so schreibt, gehoert in einen Lauf mit ublk und btrfs
//! und braucht einen Kernel mit `ublk_drv`; hier steht der Teil, der ueberall
//! laeuft. Alles ab der Zeile — Zuordnung, Rekonstruktion, Gegenprobe,
//! Rueckschreiben — ist echt: echte Platten (Dateien), echte Paritaet, echter
//! Rost.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ferrite_broker::{Classified, DamageReport, RepairBroker, SlotMap};
use ferrite_engine::{ArrayWriter, EngineError, MemberDevice, Repair};
use ferrite_format::superblock::DEFAULT_PAYLOAD_OFFSET;

/// Ein Arbeitsverzeichnis, das sich selbst wegraeumt.
struct Workspace(PathBuf);

impl Workspace {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("ferrite-repair-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("Arbeitsverzeichnis anlegen");
        Workspace(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn pattern(marker: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| (index as u8).wrapping_mul(29) ^ marker)
        .collect()
}

/// Legt ein Array an, schreibt auf jeden Slot und gibt den geteilten
/// Schreibpfad zurueck.
///
/// Geteilt, weil der Broker im Betrieb neben dem ublk-Target arbeitet und
/// nicht an seiner Stelle.
fn seeded(workspace: &Workspace) -> Arc<Mutex<ArrayWriter>> {
    ferrite_harness::create(workspace.path()).expect("Array anlegen");
    let (mut writer, _) = ferrite_harness::open(workspace.path()).expect("Array oeffnen");
    for slot in 0..ferrite_harness::SLOTS {
        writer
            .write(slot, 0, &pattern(slot as u8 + 1, 16384))
            .expect("Grundinhalt schreiben");
    }
    Arc::new(Mutex::new(writer))
}

/// Kippt Bytes direkt auf der Platte, am Schreibpfad vorbei.
fn rot(path: &Path, offset: u64, len: usize) {
    let device = MemberDevice::open(path).expect("Geraet oeffnen");
    let at = DEFAULT_PAYLOAD_OFFSET + offset;
    let mut bytes = vec![0u8; len];
    device.read_at(at, &mut bytes).expect("lesen");
    for byte in bytes.iter_mut() {
        *byte ^= 0x5A;
    }
    device.write_at(at, &bytes).expect("schreiben");
    device.flush().expect("flushen");
}

/// Die Zuordnung, die im Betrieb aus der Konfiguration kaeme.
fn slot_map() -> SlotMap {
    let mut slots = SlotMap::new();
    for slot in 0..ferrite_harness::SLOTS {
        slots.insert(format!("/dev/ublkb{slot}"), slot);
    }
    slots
}

/// Eine Zeile, wie `scrub_print_warning` sie hinterlaesst.
fn scrub_line(device: u16, physical: u64, length: u32) -> String {
    format!(
        "BTRFS warning (device ublkb{device}): checksum error at logical {physical} on dev \
         /dev/ublkb{device}, physical {physical}, root 5, inode 257, offset 0, length {length}, \
         links 1 (path: datei)"
    )
}

#[test]
fn a_kernel_line_becomes_a_repaired_range() {
    let workspace = Workspace::new("kette");
    let writer = seeded(&workspace);
    let expected = pattern(2, 16384);

    rot(
        &ferrite_harness::member_files(workspace.path())[1],
        4096,
        4096,
    );

    let mut broker = RepairBroker::new(Arc::clone(&writer), slot_map(), 4096);
    let classified = broker.classify(&scrub_line(1, 4096, 4096));
    assert_eq!(
        classified,
        Classified::Damage(DamageReport {
            slot_index: 1,
            offset: 4096,
            len: 4096,
        }),
        "der physische Offset ist zugleich der Offset in der Payload-Region"
    );

    let Classified::Damage(report) = classified else {
        unreachable!()
    };
    assert_eq!(
        broker.repair(report).expect("reparieren"),
        Repair::Written { len: 4096 }
    );

    let mut found = vec![0u8; 16384];
    let guard = writer.lock().expect("Sperre");
    guard.read(1, 0, &mut found).expect("lesen");
    assert_eq!(found, expected, "der Inhalt ist nicht zurueck");
    assert!(
        guard.verify_parity(0, 16384).expect("Paritaet pruefen"),
        "die Reparatur hat die Paritaet verstellt"
    );

    let stats = broker.stats();
    assert_eq!(stats.seen, 1);
    assert_eq!(stats.repaired, 1);
    assert_eq!(stats.bytes_repaired, 4096);
    assert!(!stats.needs_attention());
}

#[test]
fn a_message_about_a_foreign_device_is_counted_not_swallowed() {
    let workspace = Workspace::new("fremd");
    let writer = seeded(&workspace);
    let mut broker = RepairBroker::new(writer, slot_map(), 4096);

    let line = "BTRFS warning (device sda1): checksum error at logical 4096 on dev /dev/sda1, \
                physical 4096, root 5, inode 257, offset 0, length 4096, links 1 (path: x)";
    assert_eq!(broker.classify(line), Classified::ForeignDevice);

    // Gesehen, aber nicht unsers — und beides steht in der Rechnung. Wer die
    // Zuordnung falsch konfiguriert, sieht es hier und nicht am ausbleibenden
    // Erfolg.
    assert_eq!(broker.stats().seen, 1);
    assert_eq!(broker.stats().foreign, 1);
}

#[test]
fn a_line_without_a_device_offset_is_not_even_seen() {
    let workspace = Workspace::new("keine-meldung");
    let writer = seeded(&workspace);
    let mut broker = RepairBroker::new(writer, slot_map(), 4096);

    assert_eq!(
        broker.classify("BTRFS info (device ublkb0): scrub: started on devid 1"),
        Classified::NotOurs
    );
    assert_eq!(broker.stats().seen, 0);
}

#[test]
fn a_second_damaged_source_is_refused_and_marked_for_a_human() {
    let workspace = Workspace::new("zwei-schaeden");
    let writer = seeded(&workspace);
    let files = ferrite_harness::member_files(workspace.path());
    let expected = pattern(1, 16384);

    // Diesmal ist der gemeldete Slot heil und die Paritaet angefressen. Eine
    // Reparatur aus P allein schriebe Muell ueber gute Daten.
    rot(&files[usize::from(ferrite_harness::SLOTS)], 4096, 4096);

    let mut broker = RepairBroker::new(Arc::clone(&writer), slot_map(), 4096);
    let Classified::Damage(report) = broker.classify(&scrub_line(0, 4096, 4096)) else {
        panic!("die Meldung gehoert zu Slot 0");
    };

    let error = broker.repair(report).expect_err("darf nicht raten");
    assert!(
        matches!(
            error,
            ferrite_broker::BrokerError::Engine(EngineError::AmbiguousReconstruction { .. })
        ),
        "unerwartet: {error}"
    );

    let mut found = vec![0u8; 16384];
    writer
        .lock()
        .expect("Sperre")
        .read(0, 0, &mut found)
        .expect("lesen");
    assert_eq!(found, expected, "die guten Daten wurden ueberschrieben");

    assert_eq!(broker.stats().refused, 1);
    assert!(
        broker.stats().needs_attention(),
        "zwei Schaeden gleichzeitig sind ein Fall fuer einen Menschen"
    );
}

#[test]
fn a_scrub_that_reports_every_sector_costs_one_repair() {
    let workspace = Workspace::new("zusammenfassen");
    let writer = seeded(&workspace);
    let expected = pattern(1, 16384);

    rot(
        &ferrite_harness::member_files(workspace.path())[0],
        0,
        16384,
    );

    // So meldet ein Scrub: ein Befund je Sektor.
    let mut broker = RepairBroker::new(Arc::clone(&writer), slot_map(), 4096);
    let mut reports: Vec<DamageReport> = (0..4)
        .filter_map(
            |sector| match broker.classify(&scrub_line(0, sector * 4096, 4096)) {
                Classified::Damage(report) => Some(report),
                _ => None,
            },
        )
        .collect();
    assert_eq!(reports.len(), 4);

    let outcomes = broker.repair_all(&mut reports);
    assert_eq!(
        outcomes.len(),
        1,
        "vier angrenzende Sektoren sind ein Bereich"
    );
    assert_eq!(
        outcomes[0].1.as_ref().expect("reparieren"),
        &Repair::Written { len: 16384 }
    );

    let mut found = vec![0u8; 16384];
    let guard = writer.lock().expect("Sperre");
    guard.read(0, 0, &mut found).expect("lesen");
    assert_eq!(found, expected);
    assert!(guard.verify_parity(0, 16384).expect("Paritaet pruefen"));
}

#[test]
fn a_forged_message_about_an_intact_range_writes_nothing() {
    let workspace = Workspace::new("erfunden");
    let writer = seeded(&workspace);
    let mut broker = RepairBroker::new(Arc::clone(&writer), slot_map(), 4096);

    // `/dev/kmsg` ist beschreibbar. Eine erfundene Meldung kostet Lesearbeit
    // und richtet nichts an — das folgt daraus, dass die Rekonstruktion
    // gegengeprueft wird, statt ihr zu glauben.
    let Classified::Damage(report) = broker.classify(&scrub_line(2, 8192, 4096)) else {
        panic!("die Meldung gehoert zu Slot 2");
    };
    assert_eq!(
        broker.repair(report).expect("reparieren"),
        Repair::AlreadyIntact
    );
    assert_eq!(broker.stats().repaired, 0);
    assert_eq!(broker.stats().already_intact, 1);
}

#[test]
fn a_message_beyond_the_member_is_refused_instead_of_wrapped() {
    let workspace = Workspace::new("jenseits");
    let writer = seeded(&workspace);
    let mut broker = RepairBroker::new(Arc::clone(&writer), slot_map(), 4096);

    let beyond = ferrite_harness::PAYLOAD + 4096;
    let Classified::Damage(report) = broker.classify(&scrub_line(0, beyond, 4096)) else {
        panic!("die Meldung gehoert zu Slot 0");
    };

    assert!(
        matches!(
            broker.repair(report),
            Err(ferrite_broker::BrokerError::Engine(
                EngineError::BeyondDevice { .. }
            ))
        ),
        "ein Offset aus einer Kernelzeile ist ungeprueft"
    );
}
