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
    member_for, ArrayWriter, DeviceLog, DiskRebuild, EngineError, Member, MemberDevice,
};
use ferrite_format::superblock::{MemberState, Role, Superblock, DEFAULT_PAYLOAD_OFFSET};
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
    // fortgeschrieben. Solange der Inhalt des gemeldeten Members noch dasteht,
    // muss dabei dasselbe herauskommen — dieser Test haelt das fest.
    //
    // Der Zustand kommt aus dem Superblock und nicht aus einem Schalter am
    // Schreibpfad: Genau so erfaehrt ihn die Engine nach einem Neustart.
    let mut scratch = Scratch::new("degradiert");
    let mut writer = build(&mut scratch, &[BLOCK; 5], true);

    writer.write(0, 0, &pattern(1, 2048)).unwrap();
    writer
        .mark_member(4, MemberState::Stale, 0)
        .expect("Member als unbrauchbar melden");

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
    //
    // Bei drei Slots mit Q ist Neurechnen guenstiger und wird gewaehlt; ist ein
    // Member unbrauchbar gemeldet, bleibt nur das Fortschreiben. Beide Arrays
    // bekommen dieselben Daten in dieselben Slots.
    let mut recompute = Scratch::new("neurechnen");
    let mut incremental = Scratch::new("fortschreiben");
    let mut a = build(&mut recompute, &[BLOCK; 3], true);
    let mut b = build(&mut incremental, &[BLOCK; 3], true);
    b.mark_member(2, MemberState::Stale, 0).unwrap();

    for slot in 0..2u16 {
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

// --- Rekonstruktion und Rebuild -------------------------------------------

#[test]
fn a_read_from_an_unusable_member_is_reconstructed() {
    // Die Redundanz von aussen betrachtet: Der Aufrufer merkt nicht, dass der
    // Member nichts mehr traegt. Genau dafuer gibt es sie.
    let mut scratch = Scratch::new("rekonstruktion");
    let mut writer = build(&mut scratch, &[BLOCK; 4], false);

    let expected = pattern(0x9C, 4096);
    writer.write(1, 0, &expected).unwrap();
    writer.write(0, 0, &pattern(0x1A, 4096)).unwrap();
    writer.write(2, 0, &pattern(0x2B, 4096)).unwrap();

    // Den Member fuer unbrauchbar erklaeren und seinen Inhalt zerstoeren.
    writer.mark_member(1, MemberState::Stale, 0).unwrap();
    let payload_offset = writer.member(1).unwrap().superblock().payload_offset;
    writer
        .member(1)
        .unwrap()
        .device()
        .write_at(payload_offset, &[0xFFu8; 4096])
        .unwrap();

    let mut read_back = vec![0u8; 4096];
    writer.read(1, 0, &mut read_back).unwrap();
    assert_eq!(read_back, expected, "nicht aus der Paritaet rekonstruiert");
}

#[test]
fn a_rebuild_restores_the_member_from_parity() {
    let mut scratch = Scratch::new("rebuild");
    let mut writer = build(&mut scratch, &[4 * BLOCK; 3], true);

    // Etwas hineinschreiben, das nachher wieder dastehen muss.
    let mut expected = Vec::new();
    for block in 0..4u64 {
        let data = pattern(block as u8 + 0x40, 8192);
        writer.write(1, block * BLOCK, &data).unwrap();
        expected.push(data);
    }

    // Platte tauschen: Member als `Stale` melden und den Inhalt loeschen.
    writer.mark_member(1, MemberState::Stale, 0).unwrap();
    let payload_offset = writer.member(1).unwrap().superblock().payload_offset;
    writer
        .member(1)
        .unwrap()
        .device()
        .write_at(payload_offset, &vec![0xEEu8; (4 * BLOCK) as usize])
        .unwrap();

    let mut rebuild = DiskRebuild::resume(&writer, 1).expect("Rebuild aufsetzen");
    assert_eq!(rebuild.remaining_blocks(), 4);
    rebuild.run(&mut writer, 2).expect("Rebuild durchfuehren");
    assert!(rebuild.is_complete());

    // Der Member ist wieder `Clean` und traegt seinen Inhalt.
    assert_eq!(
        writer.member(1).unwrap().superblock().member_state,
        MemberState::Clean
    );
    assert_eq!(writer.member(1).unwrap().superblock().rebuild_progress, 0);
    for (block, data) in expected.iter().enumerate() {
        let mut read_back = vec![0u8; data.len()];
        writer
            .read(1, block as u64 * BLOCK, &mut read_back)
            .unwrap();
        assert_eq!(&read_back, data, "Block {block} nicht wiederhergestellt");
    }
    assert!(writer.verify_parity(0, (4 * BLOCK) as usize).unwrap());
}

#[test]
fn a_rebuild_resumes_from_the_superblock_after_a_crash() {
    // Der Fall, um den es geht: Der Rebuild bricht mittendrin ab, und was
    // danach passiert, haengt allein an dem, was auf der Platte steht.
    // `integration/tests/rebuild_resume.rs` spielt das im Speicher durch —
    // hier muss dasselbe herauskommen.
    let mut scratch = Scratch::new("wiederaufnahme");
    let mut writer = build(&mut scratch, &[6 * BLOCK; 3], false);

    for block in 0..6u64 {
        writer
            .write(2, block * BLOCK, &pattern(block as u8, 4096))
            .unwrap();
    }
    writer.mark_member(2, MemberState::Stale, 0).unwrap();
    let payload_offset = writer.member(2).unwrap().superblock().payload_offset;
    writer
        .member(2)
        .unwrap()
        .device()
        .write_at(payload_offset, &vec![0x77u8; (6 * BLOCK) as usize])
        .unwrap();

    // Zwei Stapel zu zwei Bloecken, dann Abbruch. Der Plan verlaesst diesen
    // Block und ist damit weg — genau wie nach einem Absturz.
    {
        let mut rebuild = DiskRebuild::resume(&writer, 2).unwrap();
        assert!(rebuild.step(&mut writer, 2).unwrap());
        assert!(rebuild.step(&mut writer, 2).unwrap());
    }

    // Der Fortschritt steht auf der Platte, nicht im Arbeitsspeicher.
    let superblock = writer.member(2).unwrap().superblock().clone();
    assert_eq!(superblock.member_state, MemberState::Rebuilding);
    assert_eq!(superblock.rebuild_progress, 4 * BLOCK);

    // Ein frischer Plan aus genau diesem Superblock macht dort weiter.
    let mut resumed = DiskRebuild::resume(&writer, 2).unwrap();
    assert_eq!(resumed.next_block(), 4);
    assert_eq!(resumed.remaining_blocks(), 2);
    resumed.run(&mut writer, 8).unwrap();

    assert_eq!(
        writer.member(2).unwrap().superblock().member_state,
        MemberState::Clean
    );
    for block in 0..6u64 {
        let mut read_back = vec![0u8; 4096];
        writer.read(2, block * BLOCK, &mut read_back).unwrap();
        assert_eq!(
            read_back,
            pattern(block as u8, 4096),
            "Block {block} nach der Wiederaufnahme falsch"
        );
    }
}

#[test]
fn the_progress_never_runs_ahead_of_the_blocks() {
    // Die Zusage aus dem Kickoff: erst die Bloecke durable, dann der
    // Fortschritt. Nach jedem Stapel muss alles unterhalb des Fortschritts
    // wirklich dastehen — nicht nur als Zahl im Superblock.
    let mut scratch = Scratch::new("reihenfolge");
    let mut writer = build(&mut scratch, &[8 * BLOCK; 3], false);

    for block in 0..8u64 {
        writer
            .write(0, block * BLOCK, &pattern(block as u8 + 3, 2048))
            .unwrap();
    }
    writer.mark_member(0, MemberState::Stale, 0).unwrap();
    let payload_offset = writer.member(0).unwrap().superblock().payload_offset;
    writer
        .member(0)
        .unwrap()
        .device()
        .write_at(payload_offset, &vec![0u8; (8 * BLOCK) as usize])
        .unwrap();

    let mut rebuild = DiskRebuild::resume(&writer, 0).unwrap();
    while rebuild.step(&mut writer, 3).unwrap() {
        let member = writer.member(0).unwrap();
        let done = if member.superblock().member_state == MemberState::Clean {
            8
        } else {
            member.superblock().rebuild_progress / BLOCK
        };
        // Alles unterhalb des Fortschritts liegt roh auf der Platte — ohne
        // Rekonstruktion, sonst prueft der Test sich selbst.
        for block in 0..done {
            let mut raw = vec![0u8; 2048];
            member
                .device()
                .read_at(member.superblock().payload_offset + block * BLOCK, &mut raw)
                .unwrap();
            assert_eq!(
                raw,
                pattern(block as u8 + 3, 2048),
                "Block {block} gilt als fertig, steht aber nicht auf der Platte"
            );
        }
    }
}

#[test]
fn a_clean_member_has_nothing_to_rebuild() {
    let mut scratch = Scratch::new("nichts-zu-tun");
    let writer = build(&mut scratch, &[BLOCK; 2], false);
    let rebuild = DiskRebuild::resume(&writer, 0).unwrap();
    assert!(rebuild.is_complete());
    assert_eq!(rebuild.remaining_blocks(), 0);
}

#[test]
fn two_unusable_members_stop_the_reconstruction() {
    // Mit P allein laesst sich genau ein fehlender Slot rekonstruieren. Bei
    // zweien braeuchte es Q — das kann `parity/`, aber die Buchfuehrung
    // darueber fehlt hier noch. Gemeldet statt geraten.
    let mut scratch = Scratch::new("zwei-fehlen");
    let mut writer = build(&mut scratch, &[BLOCK; 4], true);
    writer.write(0, 0, &pattern(1, 1024)).unwrap();

    writer.mark_member(1, MemberState::Stale, 0).unwrap();
    writer.mark_member(2, MemberState::Stale, 0).unwrap();

    assert!(matches!(
        writer.read(1, 0, &mut [0u8; 1024]),
        Err(EngineError::CannotRebuild { .. })
    ));
}

#[test]
fn a_write_to_a_not_yet_rebuilt_block_keeps_the_parity_right() {
    // Der Fall, den ein Rebuild im laufenden Betrieb erzeugt: Ein Write geht
    // auf einen Block, den der Member noch nicht zurueckbekommen hat. Sein
    // alter Inhalt kommt dann aus der Paritaet — und muss so stimmig sein,
    // dass eine spaetere Rekonstruktion wieder den neuen Inhalt liefert.
    let mut scratch = Scratch::new("write-waehrend-rebuild");
    let mut writer = build(&mut scratch, &[4 * BLOCK; 3], true);

    for block in 0..4u64 {
        writer
            .write(1, block * BLOCK, &pattern(block as u8, 4096))
            .unwrap();
    }
    // Halber Rebuild: die ersten zwei Bloecke gelten als fertig.
    writer
        .mark_member(1, MemberState::Rebuilding, 2 * BLOCK)
        .unwrap();

    // Ein Write auf Block 3 — jenseits des Fortschritts.
    let fresh = pattern(0xD4, 4096);
    writer.write(1, 3 * BLOCK, &fresh).unwrap();

    // Der Rebuild holt den Block aus der Paritaet und muss den neuen Inhalt
    // liefern, nicht den alten.
    let mut rebuild = DiskRebuild::resume(&writer, 1).unwrap();
    rebuild.run(&mut writer, 1).unwrap();

    let mut read_back = vec![0u8; 4096];
    writer.read(1, 3 * BLOCK, &mut read_back).unwrap();
    assert_eq!(read_back, fresh, "der Write ging beim Rebuild verloren");
    assert!(writer.verify_parity(0, (4 * BLOCK) as usize).unwrap());
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
