// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Der Schreibpfad auf echte Platten, `docs/FORMAT.md` Abschnitt 5.
//!
//! Die tragende Frage ist immer dieselbe: **Stimmt die Paritaet danach?**
//! `verify_parity` rechnet sie aus den Data-Members neu und vergleicht — es
//! prueft also gegen die Definition und nicht gegen den Weg, auf dem der
//! Schreibpfad hingekommen ist. Ein Test, der beide Male dieselbe Rechnung
//! benutzt, bliebe gruen, wenn die Rechnung falsch waere.

use std::fs::File;
use std::path::PathBuf;

use ferrite_engine::{
    member_for, ArrayWriter, DeviceLog, EngineError, Member, MemberDevice, SourceState,
};
use ferrite_format::superblock::{Role, Superblock, DEFAULT_PAYLOAD_OFFSET};
use ferrite_format::{LogRing, Uuid};
use ferrite_parity::gf;

const BLOCK: u64 = 64 * 1024;
const LOG_REGION: u64 = 256 * 1024;

/// Geraetedateien, die sich nach dem Test selbst wegraeumen.
struct Scratch {
    paths: Vec<PathBuf>,
    counter: usize,
    name: String,
}

impl Scratch {
    fn new(name: &str) -> Self {
        Scratch {
            paths: Vec::new(),
            counter: 0,
            name: name.to_string(),
        }
    }

    /// Legt ein Geraet an, auf das `payload` Bytes Nutzdaten passen.
    fn device(&mut self, payload: u64) -> MemberDevice {
        let path = std::env::temp_dir().join(format!(
            "ferrite-wt-{}-{}-{}.img",
            self.name,
            std::process::id(),
            self.counter
        ));
        self.counter += 1;
        let file = File::create(&path).expect("Datei anlegen");
        file.set_len(DEFAULT_PAYLOAD_OFFSET + payload + 65_536)
            .expect("Groesse setzen");
        drop(file);
        let device = MemberDevice::open(&path).expect("oeffnen");
        self.paths.push(path);
        device
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn superblock(role: Role, slot_index: u16, payload: u64, data_slot_count: u32) -> Superblock {
    let mut superblock = Superblock::new(
        Uuid::from_random_bytes([0x33; 16]),
        Uuid::from_random_bytes([slot_index as u8 + 1; 16]),
        role,
        data_slot_count,
        payload,
    );
    superblock.slot_index = slot_index;
    superblock
}

/// Ein Array mit den angegebenen Data-Groessen, ParityP und optional ParityQ.
fn build(scratch: &mut Scratch, data_sizes: &[u64], with_q: bool) -> ArrayWriter {
    let count = data_sizes.len() as u32;
    let longest = data_sizes.iter().copied().max().unwrap_or(0);

    let log_superblock = superblock(Role::Log, 0, LOG_REGION, count);
    let log_device = scratch.device(LOG_REGION);
    let log = DeviceLog::initialize(log_device, &log_superblock).expect("Log anlegen");

    let data: Vec<Member> = data_sizes
        .iter()
        .enumerate()
        .map(|(slot, &size)| {
            let sb = superblock(Role::Data, slot as u16, size, count);
            member_for(scratch.device(size), &sb, Role::Data).expect("Data-Member")
        })
        .collect();

    let p_sb = superblock(Role::ParityP, 0, longest, count);
    let parity_p = member_for(scratch.device(longest), &p_sb, Role::ParityP).expect("ParityP");

    let parity_q = with_q.then(|| {
        let q_sb = superblock(Role::ParityQ, 0, longest, count);
        member_for(scratch.device(longest), &q_sb, Role::ParityQ).expect("ParityQ")
    });

    ArrayWriter::new(log, data, parity_p, parity_q).expect("ArrayWriter")
}

fn pattern(marker: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| (index as u8).wrapping_mul(31) ^ marker)
        .collect()
}

// --- Der Grundfall --------------------------------------------------------

#[test]
fn a_write_lands_on_the_member_and_can_be_read_back() {
    let mut scratch = Scratch::new("grund");
    let mut writer = build(&mut scratch, &[BLOCK; 3], false);

    let data = pattern(1, 4096);
    writer.write(1, 8192, &data).unwrap();

    let mut read_back = vec![0u8; data.len()];
    writer.read(1, 8192, &mut read_back).unwrap();
    assert_eq!(read_back, data);
}

#[test]
fn the_parity_is_correct_after_every_write() {
    // Nach jedem einzelnen Write, nicht erst am Ende: Ein Pfad, der die
    // Paritaet zwischendurch falsch stehen laesst und sie erst spaeter wieder
    // einrenkt, verliert Daten bei einem Ausfall dazwischen.
    let mut scratch = Scratch::new("paritaet");
    let mut writer = build(&mut scratch, &[BLOCK; 4], true);

    for round in 0..6u8 {
        let slot = u16::from(round % 4);
        let offset = u64::from(round) * 2048;
        writer.write(slot, offset, &pattern(round, 3000)).unwrap();
        assert!(
            writer.verify_parity(0, BLOCK as usize).unwrap(),
            "Paritaet nach Runde {round} falsch"
        );
    }
}

#[test]
fn overwriting_the_same_place_keeps_the_parity_right() {
    // Der Fall, bei dem Fortschreiben schiefgeht, wenn der alte Inhalt nicht
    // *vor* dem Ueberschreiben gelesen wird.
    let mut scratch = Scratch::new("ueberschreiben");
    let mut writer = build(&mut scratch, &[BLOCK; 3], true);

    for round in 0..5u8 {
        writer.write(0, 4096, &pattern(round, 2048)).unwrap();
        assert!(writer.verify_parity(0, BLOCK as usize).unwrap());
    }

    let mut read_back = vec![0u8; 2048];
    writer.read(0, 4096, &mut read_back).unwrap();
    assert_eq!(read_back, pattern(4, 2048), "der letzte Write gewinnt");
}

#[test]
fn parity_p_is_the_xor_of_all_data_members() {
    // Gegen die Definition aus der Kerninvariante, nicht gegen den Code:
    // P[i] = D0[i] ^ D1[i] ^ D2[i], Byte fuer Byte.
    let mut scratch = Scratch::new("definition-p");
    let mut writer = build(&mut scratch, &[BLOCK; 3], false);

    writer.write(0, 0, &pattern(0xA0, 1024)).unwrap();
    writer.write(1, 0, &pattern(0xB1, 1024)).unwrap();
    writer.write(2, 0, &pattern(0xC2, 1024)).unwrap();

    let mut slots = Vec::new();
    for slot in 0..3u16 {
        let mut buffer = vec![0u8; 1024];
        writer.read(slot, 0, &mut buffer).unwrap();
        slots.push(buffer);
    }
    let expected: Vec<u8> = (0..1024)
        .map(|index| slots[0][index] ^ slots[1][index] ^ slots[2][index])
        .collect();

    assert!(writer.verify_parity(0, 1024).unwrap());
    // Und noch einmal von Hand, damit `verify_parity` nicht seine eigene
    // Rechnung bestaetigt.
    let mut found = vec![0u8; 1024];
    writer.read_parity_p(0, &mut found).unwrap();
    assert_eq!(found, expected);
}

#[test]
fn parity_q_uses_the_slot_index_as_exponent() {
    // Q[i] = ⊕ⱼ g^j · Dⱼ[i], und `j` ist der slot_index. Wer stattdessen die
    // Position in einer Liste nimmt, bekommt eine Paritaet, die sich nicht
    // rekonstruieren laesst — und merkt es erst im Ernstfall.
    let mut scratch = Scratch::new("definition-q");
    let mut writer = build(&mut scratch, &[BLOCK; 3], true);

    writer.write(0, 0, &pattern(0x11, 512)).unwrap();
    writer.write(1, 0, &pattern(0x22, 512)).unwrap();
    writer.write(2, 0, &pattern(0x33, 512)).unwrap();

    let mut slots = Vec::new();
    for slot in 0..3u16 {
        let mut buffer = vec![0u8; 512];
        writer.read(slot, 0, &mut buffer).unwrap();
        slots.push(buffer);
    }

    let expected: Vec<u8> = (0..512)
        .map(|index| {
            (0..3u8).fold(0u8, |acc, slot| {
                acc ^ gf::mul(gf::g_pow(slot), slots[usize::from(slot)][index])
            })
        })
        .collect();

    let mut found = vec![0u8; 512];
    writer.read_parity_q(0, &mut found).unwrap();
    assert_eq!(found, expected);
}

// --- Gemischte Groessen ---------------------------------------------------

#[test]
fn a_short_member_contributes_zeros_beyond_its_end() {
    // Die Kerninvariante. Slot 1 endet nach einem Block; jenseits davon muss
    // die Paritaet so aussehen, als stuenden dort Nullen — nicht abbrechen.
    let mut scratch = Scratch::new("kurz");
    let mut writer = build(&mut scratch, &[3 * BLOCK, BLOCK, 2 * BLOCK], true);

    writer.write(0, 0, &pattern(7, 4096)).unwrap();
    writer.write(1, 0, &pattern(8, 4096)).unwrap();
    writer.write(2, 0, &pattern(9, 4096)).unwrap();
    assert!(writer.verify_parity(0, 4096).unwrap());

    // Hinter dem Ende von Slot 1: nur Slot 0 und 2 tragen bei.
    writer.write(0, 2 * BLOCK, &pattern(10, 4096)).unwrap();
    assert!(
        writer.verify_parity(2 * BLOCK, 4096).unwrap(),
        "Paritaet jenseits des kurzen Members falsch"
    );

    // Und ganz hinten traegt nur noch Slot 0 bei.
    writer
        .write(0, 2 * BLOCK + 8192, &pattern(11, 1024))
        .unwrap();
    assert!(writer.verify_parity(2 * BLOCK + 8192, 1024).unwrap());
}

#[test]
fn a_write_beyond_a_short_member_is_refused() {
    let mut scratch = Scratch::new("zu-weit");
    let mut writer = build(&mut scratch, &[2 * BLOCK, BLOCK], false);

    assert!(matches!(
        writer.write(1, BLOCK - 512, &pattern(0, 1024)),
        Err(EngineError::BeyondDevice { .. })
    ));
    // Und der Rand genau passt.
    writer.write(1, BLOCK - 1024, &pattern(0, 1024)).unwrap();
}

// --- Das Log --------------------------------------------------------------

#[test]
fn every_write_leaves_a_record_and_a_checkpoint() {
    // Abschnitt 5: erst der Record, zuletzt der Checkpoint. Der Checkpoint
    // gibt Log-Platz frei — er darf erst kommen, wenn die Paritaet steht.
    let mut scratch = Scratch::new("log");
    let mut writer = build(&mut scratch, &[BLOCK; 2], false);

    let before = writer.log().next_seq();
    writer.write(0, 0, &pattern(1, 1024)).unwrap();
    // Ein Write-Record und ein Checkpoint.
    assert_eq!(writer.log().next_seq(), before + 2);

    let region = writer.log().read_region().unwrap();
    let ring = LogRing::new(&region).unwrap();
    let checkpoint = ring.newest_checkpoint().expect("kein Checkpoint");
    assert_eq!(
        checkpoint.1.seq,
        before + 1,
        "der Checkpoint deckt den Write davor"
    );
}

#[test]
fn the_record_carries_slot_and_offset() {
    let mut scratch = Scratch::new("record");
    let mut writer = build(&mut scratch, &[BLOCK; 3], false);
    let data = pattern(0x5E, 700);
    writer.write(2, 12288, &data).unwrap();

    let region = writer.log().read_region().unwrap();
    let ring = LogRing::new(&region).unwrap();
    let written: Vec<_> = ring
        .scan()
        .filter(|(_, header)| header.record_type == ferrite_format::log::RecordType::Write)
        .collect();
    assert_eq!(written.len(), 1);
    assert_eq!(written[0].1.slot_index, 2);
    assert_eq!(written[0].1.target_offset, 12288);
    assert_eq!(written[0].1.payload_len as usize, data.len());
}

// --- Verfahren ------------------------------------------------------------

#[test]
fn the_degraded_case_keeps_the_parity_right() {
    // Im degradierten Betrieb ist Neurechnen unmoeglich, also wird
    // fortgeschrieben. Solange alle Members noch lesbar sind, muss dabei
    // dasselbe herauskommen — dieser Test haelt das fest.
    let mut scratch = Scratch::new("degradiert");
    let mut writer = build(&mut scratch, &[BLOCK; 5], true);

    writer.write(0, 0, &pattern(1, 2048)).unwrap();
    writer.set_sources(SourceState::Degraded);

    for round in 0..4u8 {
        writer
            .write(
                u16::from(round),
                u64::from(round) * 1024,
                &pattern(round, 2048),
            )
            .unwrap();
        assert!(
            writer.verify_parity(0, BLOCK as usize).unwrap(),
            "Paritaet nach Runde {round} im degradierten Betrieb falsch"
        );
    }
}

#[test]
fn both_methods_produce_the_same_parity() {
    // Fortschreiben und Neurechnen sind zwei Wege zum selben Ergebnis. Weichen
    // sie ab, ist einer von beiden falsch — und welcher, sagt kein Test, der
    // nur einen von ihnen benutzt.
    let mut incremental = Scratch::new("fortschreiben");
    let mut recompute = Scratch::new("neurechnen");
    let mut a = build(&mut incremental, &[BLOCK; 6], true);
    let mut b = build(&mut recompute, &[BLOCK; 6], true);

    // `Degraded` erzwingt Fortschreiben, `AllValid` bei sechs Slots und einem
    // geschriebenen waehlt ebenfalls Fortschreiben — deshalb wird bei `b`
    // ueber alle Slots geschrieben, was das Neurechnen guenstiger macht.
    a.set_sources(SourceState::Degraded);

    for slot in 0..6u16 {
        let data = pattern(slot as u8, 1500);
        a.write(slot, 2048, &data).unwrap();
        b.write(slot, 2048, &data).unwrap();
    }

    let mut from_a = vec![0u8; 1500];
    let mut from_b = vec![0u8; 1500];
    a.read_parity_p(2048, &mut from_a).unwrap();
    b.read_parity_p(2048, &mut from_b).unwrap();
    assert_eq!(from_a, from_b, "P weicht zwischen den Verfahren ab");

    a.read_parity_q(2048, &mut from_a).unwrap();
    b.read_parity_q(2048, &mut from_b).unwrap();
    assert_eq!(from_a, from_b, "Q weicht zwischen den Verfahren ab");
}

// --- Grenzen --------------------------------------------------------------

#[test]
fn an_unknown_slot_is_refused() {
    let mut scratch = Scratch::new("unbekannt");
    let mut writer = build(&mut scratch, &[BLOCK; 2], false);
    assert!(writer.write(5, 0, &pattern(0, 512)).is_err());
    assert!(writer.read(5, 0, &mut [0u8; 512]).is_err());
}

#[test]
fn a_parity_member_shorter_than_the_longest_data_member_is_refused() {
    // Regel 6 aus Abschnitt 2.1. Hinter dem Ende der Paritaet gaebe es keine
    // Redundanz mehr, und niemand wuerde es merken.
    let mut scratch = Scratch::new("kurze-paritaet");
    let log_sb = superblock(Role::Log, 0, LOG_REGION, 1);
    let log = DeviceLog::initialize(scratch.device(LOG_REGION), &log_sb).unwrap();

    let data_sb = superblock(Role::Data, 0, 2 * BLOCK, 1);
    let data = member_for(scratch.device(2 * BLOCK), &data_sb, Role::Data).unwrap();

    let p_sb = superblock(Role::ParityP, 0, BLOCK, 1);
    let parity_p = member_for(scratch.device(BLOCK), &p_sb, Role::ParityP).unwrap();

    assert!(ArrayWriter::new(log, vec![data], parity_p, None).is_err());
}

#[test]
fn an_empty_write_changes_nothing() {
    let mut scratch = Scratch::new("leer");
    let mut writer = build(&mut scratch, &[BLOCK; 2], false);
    let before = writer.log().next_seq();
    writer.write(0, 0, &[]).unwrap();
    assert_eq!(writer.log().next_seq(), before, "kein Record fuer nichts");
}
