// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Das Write-Log auf einem Geraet, `docs/FORMAT.md` Abschnitt 5.
//!
//! Die Tests hier pruefen, was `format/` nicht pruefen kann: dass zwischen
//! Ringpuffer-Logik und Platte nichts verlorengeht. Der wichtigste ist
//! `a_stale_checkpoint_in_the_padding_area_is_erased` — ohne ihn faellt der
//! stille Datenverlust erst nach einem Absturz auf.

use std::fs::File;
use std::path::PathBuf;

use ferrite_engine::{DeviceLog, EngineError, MemberDevice};
use ferrite_format::log::{LogRecordHeader, RecordType, LOG_SECTOR_SIZE};
use ferrite_format::superblock::{Role, Superblock, DEFAULT_PAYLOAD_OFFSET};
use ferrite_format::{FormatError, LogRing, Uuid};

/// 16 Sektoren — klein genug, dass der Ringpuffer in wenigen Records umlaeuft.
const REGION: u64 = 65_536;
const SECTORS: usize = (REGION / LOG_SECTOR_SIZE as u64) as usize;
const DEVICE_SIZE: u64 = DEFAULT_PAYLOAD_OFFSET + REGION + 65_536;

/// Eine Datei, die sich nach dem Test selbst wegraeumt.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("ferrite-log-{name}-{}.img", std::process::id()));
        let file = File::create(&path).expect("Datei anlegen");
        file.set_len(DEVICE_SIZE).expect("Groesse setzen");
        Scratch(path)
    }

    fn open(&self) -> MemberDevice {
        MemberDevice::open(&self.0).expect("oeffnen")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn log_superblock() -> Superblock {
    Superblock::new(
        Uuid::from_random_bytes([0x21; 16]),
        Uuid::from_random_bytes([0x22; 16]),
        Role::Log,
        4,
        REGION,
    )
}

/// Nutzdaten, die sich von Nullen und voneinander unterscheiden.
fn payload(marker: u8, len: usize) -> Vec<u8> {
    (0..len).map(|index| (index as u8) ^ marker).collect()
}

// --- Anlegen --------------------------------------------------------------

#[test]
fn initialising_zeroes_the_whole_region() {
    // Ohne das Nullen stuende dort, was die Platte vorher trug, und der Scan
    // aus Abschnitt 5.2 faende Header, die zu keinem Array gehoeren.
    let scratch = Scratch::new("init");
    let device = scratch.open();
    device
        .write_at(DEFAULT_PAYLOAD_OFFSET, &[0xA5u8; 4096])
        .unwrap();
    device
        .write_at(DEFAULT_PAYLOAD_OFFSET + REGION - 4096, &[0xA5u8; 4096])
        .unwrap();

    let log = DeviceLog::initialize(scratch.open(), &log_superblock()).unwrap();
    assert!(log.read_region().unwrap().iter().all(|&byte| byte == 0));
    assert_eq!(log.head(), 0);
    assert_eq!(log.next_seq(), 1);
}

#[test]
fn a_member_that_is_not_the_log_is_refused() {
    let scratch = Scratch::new("falsche-rolle");
    let mut superblock = log_superblock();
    superblock.role = Role::Data;

    assert!(matches!(
        DeviceLog::initialize(scratch.open(), &superblock),
        Err(EngineError::Format(FormatError::InvalidField {
            field: "role",
            ..
        }))
    ));
}

// --- Schreiben und Zuruecklesen -------------------------------------------

#[test]
fn a_record_written_to_the_device_is_found_again() {
    let scratch = Scratch::new("record");
    let mut log = DeviceLog::initialize(scratch.open(), &log_superblock()).unwrap();

    let data = payload(0x3C, 1000);
    let offset = log.append_write(2, 8192, &data).unwrap();
    assert_eq!(offset, 0);
    assert_eq!(log.head(), LOG_SECTOR_SIZE);
    assert_eq!(log.next_seq(), 2);

    // Ueber `format` gelesen, nicht ueber die eigene Buchhaltung.
    let region = log.read_region().unwrap();
    let ring = LogRing::new(&region).unwrap();
    let found: Vec<_> = ring.replay(1).collect();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].header.seq, 1);
    assert_eq!(found[0].header.slot_index, 2);
    assert_eq!(found[0].header.target_offset, 8192);
    assert_eq!(found[0].payload, &data[..]);
}

#[test]
fn the_slack_of_the_last_sector_is_zero() {
    // Abschnitt 5.1: Der Rest des letzten belegten Sektors MUSS als Null
    // geschrieben werden. Sonst liegt dort der Rest einer frueheren Runde.
    let scratch = Scratch::new("slack");

    // Erst die Region vollschreiben, dann neu anlegen und einen kurzen
    // Record hineinsetzen.
    let mut log = DeviceLog::initialize(scratch.open(), &log_superblock()).unwrap();
    log.append_write(0, 0, &payload(0xFF, 4000)).unwrap();

    let mut log = DeviceLog::initialize(scratch.open(), &log_superblock()).unwrap();
    log.append_write(0, 0, &payload(0x11, 8)).unwrap();

    let region = log.read_region().unwrap();
    let header_len = LogRecordHeader::checkpoint(1).encode().len();
    assert!(
        region[header_len + 8..LOG_SECTOR_SIZE]
            .iter()
            .all(|&byte| byte == 0),
        "hinter den Nutzdaten steht etwas"
    );
}

#[test]
fn several_records_form_a_chain() {
    let scratch = Scratch::new("kette");
    let mut log = DeviceLog::initialize(scratch.open(), &log_superblock()).unwrap();

    for nth in 0..5u8 {
        log.append_write(nth as u16 % 4, nth as u64 * 4096, &payload(nth, 512))
            .unwrap();
    }
    assert_eq!(log.next_seq(), 6);

    let region = log.read_region().unwrap();
    let found: Vec<_> = LogRing::new(&region).unwrap().replay(1).collect();
    assert_eq!(found.len(), 5);
    for (nth, record) in found.iter().enumerate() {
        assert_eq!(record.header.seq, nth as u64 + 1);
        assert_eq!(record.payload, &payload(nth as u8, 512)[..]);
    }
}

#[test]
fn a_checkpoint_moves_the_starting_point_of_the_replay() {
    // Ein Checkpoint sagt: Alles bis zu seiner `seq` liegt bereits auf den
    // Data-Members und in der Paritaet. Der Replay beginnt deshalb *hinter*
    // ihm — was davor steht, noch einmal anzuwenden waere Arbeit ohne Wirkung.
    let scratch = Scratch::new("checkpoint");
    let mut log = DeviceLog::initialize(scratch.open(), &log_superblock()).unwrap();

    log.append_write(0, 0, &payload(1, 64)).unwrap();
    log.append_checkpoint().unwrap();
    log.append_write(1, 4096, &payload(2, 64)).unwrap();

    let region = log.read_region().unwrap();
    let ring = LogRing::new(&region).unwrap();
    assert_eq!(ring.newest_checkpoint().unwrap().1.seq, 2);

    let found: Vec<_> = ring.replay(1).collect();
    assert_eq!(found.len(), 1, "nur was nach dem Checkpoint kam");
    assert_eq!(found[0].header.seq, 3);
    assert_eq!(found[0].header.record_type, RecordType::Write);
}

// --- Nach einem Neustart --------------------------------------------------

#[test]
fn reopening_finds_the_head_and_the_next_sequence_number() {
    let scratch = Scratch::new("neustart");
    {
        let mut log = DeviceLog::initialize(scratch.open(), &log_superblock()).unwrap();
        for nth in 0..3u8 {
            log.append_write(0, nth as u64 * 4096, &payload(nth, 100))
                .unwrap();
        }
    }

    // Neues Geraet, neues Log — alles kommt von der Platte.
    let (log, recovery) = DeviceLog::open(scratch.open(), &log_superblock()).unwrap();
    assert_eq!(recovery.accepted, 3);
    assert_eq!(recovery.next_seq, 4);
    assert_eq!(log.next_seq(), 4);
    assert_eq!(log.head(), 3 * LOG_SECTOR_SIZE);

    let seqs: Vec<u64> = recovery
        .records()
        .unwrap()
        .map(|record| record.header.seq)
        .collect();
    assert_eq!(seqs, vec![1, 2, 3]);
}

#[test]
fn writing_continues_where_the_replay_stopped() {
    let scratch = Scratch::new("fortsetzen");
    {
        let mut log = DeviceLog::initialize(scratch.open(), &log_superblock()).unwrap();
        log.append_write(0, 0, &payload(1, 100)).unwrap();
        log.append_write(0, 4096, &payload(2, 100)).unwrap();
    }

    let (mut log, _) = DeviceLog::open(scratch.open(), &log_superblock()).unwrap();
    log.append_write(0, 8192, &payload(3, 100)).unwrap();

    let region = log.read_region().unwrap();
    let found: Vec<_> = LogRing::new(&region).unwrap().replay(1).collect();
    assert_eq!(found.len(), 3);
    assert_eq!(found[2].header.seq, 3);
    assert_eq!(found[2].payload, &payload(3, 100)[..]);
}

#[test]
fn an_empty_log_reopens_as_empty() {
    let scratch = Scratch::new("leer");
    DeviceLog::initialize(scratch.open(), &log_superblock()).unwrap();

    let (log, recovery) = DeviceLog::open(scratch.open(), &log_superblock()).unwrap();
    assert_eq!(recovery.accepted, 0);
    assert_eq!(log.head(), 0);
    assert_eq!(log.next_seq(), 1);
}

#[test]
fn a_torn_record_stops_the_replay() {
    // Ein Absturz mitten im Schreiben sieht so aus. Was danach kommt, wird
    // verworfen — auch wenn es in sich gueltig ist (Abschnitt 5.2, Schritt 4).
    let scratch = Scratch::new("torn");
    let device = scratch.open();
    let mut log = DeviceLog::initialize(scratch.open(), &log_superblock()).unwrap();
    for nth in 0..4u8 {
        log.append_write(0, nth as u64 * 4096, &payload(nth, 100))
            .unwrap();
    }

    // Den zweiten Record zerstoeren.
    device
        .write_at(
            DEFAULT_PAYLOAD_OFFSET + LOG_SECTOR_SIZE as u64,
            &[0xFFu8; 64],
        )
        .unwrap();
    device.flush().unwrap();

    let (log, recovery) = DeviceLog::open(scratch.open(), &log_superblock()).unwrap();
    assert_eq!(recovery.accepted, 1, "nur der erste Record zaehlt");
    assert_eq!(log.next_seq(), 2);
    assert!(recovery.stop.is_some());
}

// --- Umlauf des Ringpuffers ----------------------------------------------

#[test]
fn the_ring_wraps_with_a_padding_record() {
    let scratch = Scratch::new("umlauf");
    let mut log = DeviceLog::initialize(scratch.open(), &log_superblock()).unwrap();

    // Bis auf einen Sektor vollschreiben, dann einen Record, der zwei
    // braucht — er passt nicht mehr und erzwingt ein Padding.
    for nth in 0..SECTORS - 1 {
        log.append_write(0, nth as u64 * 4096, &payload(nth as u8, 64))
            .unwrap();
    }
    assert_eq!(log.head(), (SECTORS - 1) * LOG_SECTOR_SIZE);

    let offset = log.append_write(0, 0, &payload(0xEE, 5000)).unwrap();
    assert_eq!(offset, 0, "der Record beginnt wieder bei null");

    let region = log.read_region().unwrap();
    let padding = LogRecordHeader::decode(
        &region[(SECTORS - 1) * LOG_SECTOR_SIZE..][..LogRecordHeader::checkpoint(0).encode().len()],
    )
    .unwrap();
    assert_eq!(padding.record_type, RecordType::Padding);
}

#[test]
fn a_stale_checkpoint_in_the_padding_area_is_erased() {
    // Der Test, um den es geht. Bleibt im Bereich, den ein Padding abdeckt,
    // ein intakter Checkpoint aus einer frueheren Runde stehen, findet ihn
    // Schritt 2 des Recovery — und der Replay beginnt an der falschen Stelle.
    let scratch = Scratch::new("alter-checkpoint");
    let mut log = DeviceLog::initialize(scratch.open(), &log_superblock()).unwrap();

    // Erste Runde: die Region bis kurz vor das Ende fuellen, unterwegs ein
    // Checkpoint mit hoher Sequenznummer.
    for nth in 0..SECTORS - 4 {
        if nth == SECTORS - 6 {
            log.append_checkpoint().unwrap();
        } else {
            log.append_write(0, nth as u64 * 4096, &payload(nth as u8, 64))
                .unwrap();
        }
    }
    let stale_seq = log.next_seq() - 1;

    // Ein Record, der nicht mehr passt: Das Padding deckt die letzten vier
    // Sektoren ab, darunter den Checkpoint.
    log.append_write(0, 0, &payload(0xAB, 64)).unwrap();

    let region = log.read_region().unwrap();
    // Kein Sektor hinter dem Padding-Header traegt noch einen Header.
    let stale: Vec<usize> = ((SECTORS - 3)..SECTORS)
        .filter(|sector| LogRecordHeader::decode(&region[sector * LOG_SECTOR_SIZE..][..64]).is_ok())
        .collect();
    assert!(
        stale.is_empty(),
        "Sektoren {stale:?} tragen noch alte Header (seq bis {stale_seq})"
    );
}

#[test]
fn the_chain_survives_the_wrap() {
    let scratch = Scratch::new("umlauf-kette");
    let mut log = DeviceLog::initialize(scratch.open(), &log_superblock()).unwrap();

    // Einmal ganz herum und ein Stueck weiter, mit einem Checkpoint kurz vor
    // Schluss — sonst beginnt der Replay bei der niedrigsten Sequenznummer,
    // und die liegt hinter dem Kopf.
    for nth in 0..SECTORS + 3 {
        log.append_write(0, nth as u64 * 4096, &payload(nth as u8, 64))
            .unwrap();
    }
    log.append_checkpoint().unwrap();
    let after_checkpoint = log.next_seq();
    log.append_write(0, 0, &payload(0x7F, 64)).unwrap();

    let (reopened, recovery) = DeviceLog::open(scratch.open(), &log_superblock()).unwrap();
    // Nach dem Umlauf steht der Checkpoint irgendwo mitten in der Region und
    // der Record danach am Anfang oder dahinter — der Replay findet ihn
    // trotzdem, und zwar genau ihn.
    let seqs: Vec<u64> = recovery
        .records()
        .unwrap()
        .map(|record| record.header.seq)
        .collect();
    assert_eq!(seqs, vec![after_checkpoint]);
    assert_eq!(reopened.next_seq(), after_checkpoint + 1);
}

// --- Grenzen --------------------------------------------------------------

#[test]
fn a_record_larger_than_the_region_is_refused() {
    let scratch = Scratch::new("zu-gross");
    let mut log = DeviceLog::initialize(scratch.open(), &log_superblock()).unwrap();

    assert!(matches!(
        log.append_write(0, 0, &payload(0, REGION as usize + 1)),
        Err(EngineError::Format(FormatError::InvalidField {
            field: "payload_len",
            ..
        }))
    ));
}

#[test]
fn a_payload_that_does_not_match_the_header_is_refused() {
    let scratch = Scratch::new("laenge");
    let mut log = DeviceLog::initialize(scratch.open(), &log_superblock()).unwrap();

    let header = LogRecordHeader::write(1, 0, 0, &payload(0, 100));
    assert!(matches!(
        log.append(&header, &payload(0, 99)),
        Err(EngineError::Format(FormatError::InvalidField {
            field: "payload_len",
            ..
        }))
    ));
}

#[test]
fn a_read_only_log_refuses_to_be_written() {
    let scratch = Scratch::new("nur-lesend");
    DeviceLog::initialize(scratch.open(), &log_superblock()).unwrap();

    let read_only = MemberDevice::open_read_only(&scratch.0).unwrap();
    let (mut log, _) = DeviceLog::open(read_only, &log_superblock()).unwrap();
    assert_eq!(
        log.append_write(0, 0, &payload(0, 64)),
        Err(EngineError::NotWritable)
    );
}

// --- Die Sequenzkette ueber Checkpoints hinweg ---------------------------

#[test]
fn the_sequence_keeps_running_across_a_checkpoint() {
    // Abschnitt 5.1: `seq` ist streng monoton steigend **ueber die Lebensdauer
    // des Arrays**. Ein Checkpoint deckt alles vor sich ab, aber er setzt den
    // Zaehler nicht zurueck.
    //
    // Faellt dieser Test, vergibt das Log nach jedem Neustart wieder kleine
    // Sequenznummern. Der Replay findet die neuen Records dann nicht als
    // Nachfolger des Checkpoints — und wendet nach einem Absturz nichts an.
    let scratch = Scratch::new("kette-ueber-checkpoint");
    let mut log = DeviceLog::initialize(scratch.open(), &log_superblock()).unwrap();
    log.append_write(0, 0, &payload(1, 64)).unwrap();
    log.append_checkpoint().unwrap();
    let after = log.next_seq();
    assert_eq!(after, 3, "zwei Records vergeben seq 1 und 2");

    let (reopened, recovery) = DeviceLog::open(scratch.open(), &log_superblock()).unwrap();
    assert_eq!(
        recovery.accepted, 0,
        "der Checkpoint deckt den Write, es gibt nichts anzuwenden"
    );
    assert_eq!(
        reopened.next_seq(),
        after,
        "die Sequenznummer ist nach dem Neustart zurueckgesprungen"
    );
    assert_eq!(
        reopened.head(),
        2 * LOG_SECTOR_SIZE,
        "der Kopf liegt nicht hinter dem zuletzt geschriebenen Record"
    );
}

#[test]
fn a_record_written_after_a_checkpoint_is_replayed() {
    // Der Fall, um den es wirklich geht: Nach einem Checkpoint wird ein Write
    // geloggt, und dann faellt der Strom aus, bevor er angewendet ist. Beim
    // naechsten Oeffnen muss der Replay ihn finden.
    let scratch = Scratch::new("nach-checkpoint");
    {
        let mut log = DeviceLog::initialize(scratch.open(), &log_superblock()).unwrap();
        log.append_write(0, 0, &payload(1, 64)).unwrap();
        log.append_checkpoint().unwrap();
    }
    {
        // Neustart, dann ein weiterer Write — so wie im laufenden Betrieb.
        let (mut log, _) = DeviceLog::open(scratch.open(), &log_superblock()).unwrap();
        log.append_write(1, 4096, &payload(2, 128)).unwrap();
    }

    let (_, recovery) = DeviceLog::open(scratch.open(), &log_superblock()).unwrap();
    let seqs: Vec<u64> = recovery
        .records()
        .unwrap()
        .map(|record| record.header.seq)
        .collect();
    assert_eq!(
        seqs,
        vec![3],
        "der Record nach dem Checkpoint wurde nicht wiedergefunden"
    );
}
