//! Tests gegen `docs/FORMAT.md`.
//!
//! Der Zufall hier laeuft ueber einen festen LCG statt ueber eine
//! Fuzzing-Bibliothek: reproduzierbar, ohne Dependency, und in CI in
//! Millisekunden durch. Die richtige Fuzzing-Runde (`cargo-fuzz` auf
//! `Superblock::decode` und `LogRecordHeader::decode`) kommt zusaetzlich, nicht
//! stattdessen.

use ferrite_format::error::FormatError;
use ferrite_format::log::{ChainBreak, ChainValidator, ChainVerdict, LogRecordHeader, RecordType};
use ferrite_format::superblock::{
    AccessMode, MemberState, Role, Superblock, MIN_DEVICE_SIZE, SUPERBLOCK_SIZE,
};
use ferrite_format::Uuid;

struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed)
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 1
    }
    fn below(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound
    }
    fn bytes16(&mut self) -> [u8; 16] {
        let mut out = [0u8; 16];
        for chunk in out.chunks_mut(8) {
            chunk.copy_from_slice(&self.next_u64().to_le_bytes());
        }
        out
    }
}

fn sample_superblock() -> Superblock {
    let mut superblock = Superblock::new(
        Uuid::from_random_bytes([0xA1; 16]),
        Uuid::from_random_bytes([0xB2; 16]),
        Role::Data,
        6,
        512 * 1024 * 1024,
    );
    superblock.slot_index = 3;
    superblock.generation = 42;
    superblock.created_unix = 1_756_000_000;
    superblock.label = "gg-tank".to_string();
    superblock
}

#[test]
fn superblock_roundtrip() {
    let original = sample_superblock();
    let encoded = original.encode().unwrap();
    assert_eq!(encoded.len(), SUPERBLOCK_SIZE);
    assert_eq!(&encoded[..8], b"FERRITE1");
    assert_eq!(Superblock::decode(&encoded).unwrap(), original);
}

#[test]
fn superblock_reserved_area_is_zero() {
    // Reservierte Bytes muessen null sein, sonst kann kein spaeteres Feature
    // sie belegen, ohne alte Arrays zu brechen. Genau so sind in Version 0.2
    // `member_state` und `rebuild_progress` entstanden.
    let encoded = sample_superblock().encode().unwrap();
    assert!(encoded[145..152].iter().all(|&b| b == 0), "Luecke 145..152");
    assert!(encoded[160..4092].iter().all(|&b| b == 0), "Rest ab 160");
}

#[test]
fn superblock_detects_single_bit_flip_anywhere() {
    let encoded = sample_superblock().encode().unwrap();
    // Jedes Byte des pruefsummengeschuetzten Bereichs einmal kippen.
    for index in 0..4092 {
        let mut corrupted = encoded;
        corrupted[index] ^= 0x01;
        let result = Superblock::decode(&corrupted);
        assert!(
            result.is_err(),
            "Bitflip an Offset {index} wurde nicht erkannt"
        );
    }
}

#[test]
fn superblock_rejects_foreign_device() {
    let mut buffer = [0u8; SUPERBLOCK_SIZE];
    buffer[..8].copy_from_slice(b"NOTOURS!");
    assert!(matches!(
        Superblock::decode(&buffer),
        Err(FormatError::BadMagic { .. })
    ));
}

#[test]
fn superblock_rejects_truncated_buffer() {
    let encoded = sample_superblock().encode().unwrap();
    assert!(matches!(
        Superblock::decode(&encoded[..100]),
        Err(FormatError::BufferTooSmall {
            need: 4096,
            got: 100
        })
    ));
}

#[test]
fn superblock_rejects_slot_index_out_of_range() {
    let mut superblock = sample_superblock();
    superblock.slot_index = 6; // data_slot_count ist 6
    assert!(matches!(
        superblock.encode(),
        Err(FormatError::InvalidField {
            field: "slot_index",
            ..
        })
    ));
}

#[test]
fn superblock_rejects_unaligned_payload_size() {
    let mut superblock = sample_superblock();
    superblock.payload_size += 4096; // kein Vielfaches von 64 KiB
    assert!(matches!(
        superblock.encode(),
        Err(FormatError::InvalidField {
            field: "payload_size",
            ..
        })
    ));
}

#[test]
fn parity_member_may_skip_slot_index_rule() {
    let mut superblock = sample_superblock();
    superblock.role = Role::ParityP;
    superblock.slot_index = 999;
    let encoded = superblock.encode().unwrap();
    assert_eq!(Superblock::decode(&encoded).unwrap().role, Role::ParityP);
}

#[test]
fn feature_flags_gate_access() {
    let mut superblock = sample_superblock();
    assert_eq!(superblock.access_mode().unwrap(), AccessMode::ReadWrite);

    superblock.feature_ro_compat = 1 << 3;
    assert_eq!(superblock.access_mode().unwrap(), AccessMode::ReadOnly);

    superblock.feature_incompat = 1 << 7;
    assert!(matches!(
        superblock.access_mode(),
        Err(FormatError::IncompatibleFeatures { .. })
    ));
}

#[test]
fn select_prefers_higher_generation() {
    let mut old = sample_superblock();
    old.generation = 10;
    let mut new = sample_superblock();
    new.generation = 11;

    let old_block = old.encode().unwrap();
    let new_block = new.encode().unwrap();

    assert_eq!(Superblock::select(&old_block, &new_block).unwrap(), new);
    assert_eq!(Superblock::select(&new_block, &old_block).unwrap(), new);
}

#[test]
fn select_survives_one_corrupt_copy() {
    let good = sample_superblock();
    let good_block = good.encode().unwrap();
    let torn = [0u8; SUPERBLOCK_SIZE];

    assert_eq!(Superblock::select(&torn, &good_block).unwrap(), good);
    assert_eq!(Superblock::select(&good_block, &torn).unwrap(), good);
    assert!(Superblock::select(&torn, &torn).is_err());
}

#[test]
fn superblock_randomized_roundtrip() {
    let mut rng = Lcg::new(0xFE4417E);
    for _ in 0..2000 {
        let slot_count = 1 + rng.below(64) as u32;
        let block_log2 = 12 + rng.below(13) as u8;
        let blocks = 1 + rng.below(4096);

        let mut superblock = Superblock::new(
            Uuid::from_random_bytes(rng.bytes16()),
            Uuid::from_random_bytes(rng.bytes16()),
            Role::Data,
            slot_count,
            blocks << block_log2,
        );
        superblock.parity_block_size_log2 = block_log2;
        superblock.slot_index = rng.below(slot_count as u64) as u16;
        superblock.generation = rng.next_u64();
        superblock.created_unix = rng.next_u64();
        superblock.feature_compat = rng.next_u64();

        let encoded = superblock.encode().expect("gueltige Eingabe muss kodieren");
        assert_eq!(Superblock::decode(&encoded).unwrap(), superblock);
    }
}

#[test]
fn decode_never_panics_on_garbage() {
    let mut rng = Lcg::new(0xC0FFEE);
    for _ in 0..5000 {
        let mut buffer = [0u8; SUPERBLOCK_SIZE];
        for chunk in buffer.chunks_mut(8) {
            chunk.copy_from_slice(&rng.next_u64().to_le_bytes());
        }
        // Magic gueltig setzen, damit der Parser tatsaechlich weiterlaeuft.
        buffer[..8].copy_from_slice(b"FERRITE1");
        let _ = Superblock::decode(&buffer);

        let mut header = [0u8; 64];
        for chunk in header.chunks_mut(8) {
            chunk.copy_from_slice(&rng.next_u64().to_le_bytes());
        }
        header[..4].copy_from_slice(b"FLOG");
        let _ = LogRecordHeader::decode(&header);
    }
}

#[test]
fn log_header_roundtrip() {
    let payload = vec![0x7Eu8; 8192];
    let mut header = LogRecordHeader::write(17, 2, 4096 * 900, &payload);
    header.generation = 5;
    header.commit_unix = 1_756_000_123;

    let encoded = header.encode();
    let decoded = LogRecordHeader::decode(&encoded).unwrap();
    assert_eq!(decoded, header);
    assert_eq!(decoded.record_type, RecordType::Write);
    decoded.verify_payload(&payload).unwrap();
}

#[test]
fn log_header_on_disk_len_is_sector_aligned() {
    for length in [0usize, 1, 4095, 4096, 4097, 65536] {
        let payload = vec![0u8; length];
        let header = LogRecordHeader::write(1, 0, 0, &payload);
        let on_disk = header.on_disk_len();
        assert_eq!(on_disk % 4096, 0);
        assert!(on_disk >= 64 + length);
    }
}

#[test]
fn log_header_detects_corrupt_payload() {
    let mut payload = vec![0x11u8; 4096];
    let header = LogRecordHeader::write(1, 0, 0, &payload);
    payload[2048] ^= 0x80;
    assert!(matches!(
        header.verify_payload(&payload),
        Err(FormatError::ChecksumMismatch { .. })
    ));
}

#[test]
fn log_header_detects_wrong_length() {
    let payload = vec![0x11u8; 4096];
    let header = LogRecordHeader::write(1, 0, 0, &payload);
    assert!(header.verify_payload(&payload[..2048]).is_err());
}

fn record(seq: u64, generation: u64, payload: &[u8]) -> LogRecordHeader {
    // `target_offset` spielt fuer die Kettenregel keine Rolle, es soll nur pro
    // Record verschieden sein — deshalb wrappend, damit auch `seq == u64::MAX`
    // als Eingabe taugt.
    let mut header = LogRecordHeader::write(seq, 0, seq.wrapping_mul(4096), payload);
    header.generation = generation;
    header
}

#[test]
fn chain_accepts_contiguous_run() {
    let payload = vec![0xABu8; 4096];
    let mut chain = ChainValidator::new(9, 100);
    for seq in 100..110 {
        assert_eq!(
            chain.offer(&record(seq, 9, &payload), &payload),
            ChainVerdict::Accept
        );
    }
    assert_eq!(chain.accepted_count(), 10);
    assert_eq!(chain.last_accepted_seq(), Some(109));
    assert!(!chain.is_broken());
}

#[test]
fn chain_stops_at_gap_and_stays_stopped() {
    // Der eigentliche Test: Nach der Luecke folgt ein in sich vollkommen
    // gueltiger Record. Er darf trotzdem nicht mehr angewendet werden, sonst
    // landen alte Daten aus einer frueheren Runde des Ringpuffers ueber neuen.
    let payload = vec![0xCDu8; 4096];
    let mut chain = ChainValidator::new(3, 500);

    assert_eq!(
        chain.offer(&record(500, 3, &payload), &payload),
        ChainVerdict::Accept
    );
    assert_eq!(
        chain.offer(&record(502, 3, &payload), &payload),
        ChainVerdict::StopReplay(ChainBreak::SequenceGap {
            expected: 501,
            found: 502
        })
    );
    assert_eq!(
        chain.offer(&record(501, 3, &payload), &payload),
        ChainVerdict::StopReplay(ChainBreak::AlreadyBroken)
    );
    assert_eq!(chain.accepted_count(), 1);
}

#[test]
fn chain_rejects_stale_generation() {
    let payload = vec![0u8; 4096];
    let mut chain = ChainValidator::new(7, 1);
    assert_eq!(
        chain.offer(&record(1, 6, &payload), &payload),
        ChainVerdict::StopReplay(ChainBreak::GenerationMismatch {
            expected: 7,
            found: 6
        })
    );
}

#[test]
fn chain_stops_on_corrupt_payload() {
    let payload = vec![0x5Au8; 4096];
    let header = record(1, 1, &payload);
    let mut damaged = payload.clone();
    damaged[0] ^= 0xFF;

    let mut chain = ChainValidator::new(1, 1);
    assert_eq!(
        chain.offer(&header, &damaged),
        ChainVerdict::StopReplay(ChainBreak::CorruptPayload)
    );
    assert!(chain.is_broken());
}

// ---------------------------------------------------------------------------
// Regressionen aus `format/fuzz`. Jeder Eintrag hier hat einmal rot gezeigt.
// ---------------------------------------------------------------------------

/// `superblock_roundtrip`, Fund 1.
///
/// Ein Label mit einem Nullbyte darin kam durch `validate()`, wurde geschrieben
/// und las sich am Nullbyte abgeschnitten zurueck. Abschnitt 4 fuellt das Feld
/// mit Nullbytes auf — das Nullbyte ist damit das Ende des Labels und kann
/// nicht Teil davon sein. Ohne die Regel aendert ein Schreib-Lese-Zyklus still
/// die Metadaten.
#[test]
fn superblock_rejects_label_with_interior_nul() {
    let mut superblock = sample_superblock();
    superblock.label = "tank\0extra".to_string();
    assert!(matches!(
        superblock.encode(),
        Err(FormatError::InvalidField { field: "label", .. })
    ));
}

/// `superblock_roundtrip`, Fund 1, minimaler Fall aus dem Fuzzer.
#[test]
fn superblock_rejects_label_that_is_only_a_nul() {
    let mut superblock = sample_superblock();
    superblock.label = "\0".to_string();
    assert!(matches!(
        superblock.encode(),
        Err(FormatError::InvalidField { field: "label", .. })
    ));
}

/// Die Gegenprobe: Ein Label, das die volle Feldbreite ausnutzt, muss weiterhin
/// verlustfrei durchlaufen. Hier gibt es kein terminierendes Nullbyte.
#[test]
fn superblock_label_may_fill_the_whole_field() {
    let mut superblock = sample_superblock();
    superblock.label = "x".repeat(32);
    let encoded = superblock.encode().unwrap();
    assert_eq!(Superblock::decode(&encoded).unwrap(), superblock);
}

/// `chain_replay`, Fund 2.
///
/// `seq == u64::MAX` liess den Zaehler ueberlaufen. Im Debug-Build war das eine
/// Panik im Recovery-Pfad, im Release-Build das Schlimmere: Der Zaehler lief
/// auf 0 zurueck, und der naechste Record mit `seq == 0` wurde akzeptiert. Genau
/// das ist der stille Datenverlust, den Abschnitt 5.2 Schritt 4 verhindern
/// soll — ein uralter Record aus der ersten Runde des Ringpuffers landet ueber
/// neuen Daten.
#[test]
fn chain_stops_after_the_last_representable_sequence_number() {
    let payload = vec![0x42u8; 4096];
    let mut chain = ChainValidator::new(4, u64::MAX);

    assert_eq!(
        chain.offer(&record(u64::MAX, 4, &payload), &payload),
        ChainVerdict::Accept
    );
    assert_eq!(chain.last_accepted_seq(), Some(u64::MAX));
    assert!(
        chain.is_broken(),
        "nach der letzten Nummer ist die Kette zu Ende"
    );

    assert_eq!(
        chain.offer(&record(0, 4, &payload), &payload),
        ChainVerdict::StopReplay(ChainBreak::AlreadyBroken)
    );
    assert_eq!(chain.accepted_count(), 1);
}

/// Die Gegenprobe zur Regression oben: Bis kurz vor die Grenze laeuft die Kette
/// normal weiter.
#[test]
fn chain_runs_up_to_the_last_representable_sequence_number() {
    let payload = vec![0x42u8; 512];
    let mut chain = ChainValidator::new(4, u64::MAX - 2);

    for seq in [u64::MAX - 2, u64::MAX - 1, u64::MAX] {
        assert_eq!(
            chain.offer(&record(seq, 4, &payload), &payload),
            ChainVerdict::Accept,
            "seq {seq}"
        );
    }
    assert_eq!(chain.accepted_count(), 3);
    assert_eq!(chain.last_accepted_seq(), Some(u64::MAX));
}

// ---------------------------------------------------------------------------
// Abschnitt 3: die Bedingungen, die die Geraetegroesse brauchen.
// ---------------------------------------------------------------------------

/// Kleinste Geraetegroesse, auf der `sample_superblock()` Platz hat:
/// payload_end plus der reservierte Bereich am Ende.
const SAMPLE_MIN_DEVICE: u64 = 1_048_576 + 512 * 1024 * 1024 + 65_536;

#[test]
fn superblock_fits_on_a_device_with_room_to_spare() {
    let superblock = sample_superblock();
    superblock
        .fits_on_device(SAMPLE_MIN_DEVICE + 1_000_000)
        .unwrap();
}

#[test]
fn payload_may_end_exactly_at_the_backup_superblock() {
    // Der Grenzfall in die erlaubte Richtung: Die Payload endet genau dort, wo
    // der reservierte Bereich am Ende beginnt.
    let superblock = sample_superblock();
    assert_eq!(
        superblock.payload_end().unwrap(),
        Superblock::backup_offset(SAMPLE_MIN_DEVICE).unwrap()
    );
    superblock.fits_on_device(SAMPLE_MIN_DEVICE).unwrap();
}

#[test]
fn payload_must_not_reach_into_the_backup_superblock() {
    // Ein einziges Byte zu wenig. Ohne diese Pruefung ueberschriebe der erste
    // Write auf den letzten Parity-Block den Backup-Superblock — und zwar
    // unbemerkt, weil beim Schreiben nichts auffaellt.
    let superblock = sample_superblock();
    assert!(matches!(
        superblock.fits_on_device(SAMPLE_MIN_DEVICE - 1),
        Err(FormatError::InvalidField {
            field: "payload_size",
            reason: "reicht in den Backup-Superblock"
        })
    ));
}

#[test]
fn a_device_too_small_for_both_superblocks_is_refused() {
    let superblock = sample_superblock();
    assert!(matches!(
        superblock.fits_on_device(MIN_DEVICE_SIZE - 1),
        Err(FormatError::InvalidField {
            field: "device_size",
            ..
        })
    ));
    // Bei genau `MIN_DEVICE_SIZE` liegen die Superbloecke gerade noch
    // nebeneinander — die Payload passt dann natuerlich trotzdem nicht.
    assert!(matches!(
        superblock.fits_on_device(MIN_DEVICE_SIZE),
        Err(FormatError::InvalidField {
            field: "payload_size",
            ..
        })
    ));
}

#[test]
fn a_payload_that_overflows_the_address_space_is_refused() {
    // `payload_offset` ist 4096-aligned und gueltig, die Summe laeuft trotzdem
    // ueber. Ein u64-Ueberlauf im Groessenvergleich waere hier eine Panik im
    // Debug-Build und eine falsche Zusage im Release-Build.
    let mut superblock = sample_superblock();
    superblock.payload_offset = u64::MAX - 4095;
    assert_eq!(superblock.payload_end(), None);
    assert!(matches!(
        superblock.fits_on_device(u64::MAX),
        Err(FormatError::InvalidField {
            field: "payload_size",
            reason: "payload_offset + payload_size laeuft ueber"
        })
    ));
}

#[test]
fn fits_on_device_also_enforces_the_field_rules() {
    // Wer nur diese Funktion aufruft, darf kein falsches Ja bekommen.
    let mut superblock = sample_superblock();
    superblock.slot_index = 99;
    assert!(matches!(
        superblock.fits_on_device(SAMPLE_MIN_DEVICE),
        Err(FormatError::InvalidField {
            field: "slot_index",
            ..
        })
    ));
}

/// Abschnitt 5.1: Der Wert 3 ist reserviert und MUSS abgelehnt werden.
///
/// Vorher stand dort `Barrier` — ein Record-Typ, den das Dokument nirgends
/// erklaerte und den niemand schreiben oder lesen konnte. Ein reservierter Wert
/// kostet nichts, eine geratene Semantik kostet eine Formatversion.
#[test]
fn record_type_three_is_reserved_and_refused() {
    let mut header = LogRecordHeader::checkpoint(1).encode();
    // record_type steht auf Offset 4, danach die Header-Pruefsumme neu bilden.
    header[4..6].copy_from_slice(&3u16.to_le_bytes());
    let checksum = ferrite_format::checksum(&header[..60]);
    header[60..].copy_from_slice(&checksum.to_le_bytes());

    assert!(matches!(
        LogRecordHeader::decode(&header),
        Err(FormatError::UnknownRecordType(3))
    ));
}

#[test]
fn the_defined_record_types_still_decode() {
    for record_type in [
        RecordType::Write,
        RecordType::Checkpoint,
        RecordType::Padding,
    ] {
        let mut header = LogRecordHeader::checkpoint(1);
        header.record_type = record_type;
        assert_eq!(
            LogRecordHeader::decode(&header.encode())
                .unwrap()
                .record_type,
            record_type
        );
    }
}

// ---------------------------------------------------------------------------
// Abschnitt 4.2: Member-Zustand.
// ---------------------------------------------------------------------------

#[test]
fn member_state_and_rebuild_progress_round_trip() {
    let mut superblock = sample_superblock();
    superblock.member_state = MemberState::Rebuilding;
    superblock.rebuild_progress = 64 * 1024 * 1024; // Vielfaches von 64 KiB

    let encoded = superblock.encode().unwrap();
    assert_eq!(encoded[144], 1);
    assert_eq!(Superblock::decode(&encoded).unwrap(), superblock);
}

#[test]
fn all_zero_state_bytes_mean_clean() {
    // Der Grund, warum `Clean` den Wert 0 hat: Die beiden Felder kamen aus dem
    // reservierten Bereich, der als Null geschrieben wird. Ein spaeteres Feld
    // aus demselben Vorrat muss dieselbe Eigenschaft haben — sein Nullwert
    // muss das bisherige Verhalten bedeuten.
    let encoded = sample_superblock().encode().unwrap();
    assert_eq!(encoded[144], 0);
    assert!(encoded[152..160].iter().all(|&b| b == 0));

    let decoded = Superblock::decode(&encoded).unwrap();
    assert_eq!(decoded.member_state, MemberState::Clean);
    assert_eq!(decoded.rebuild_progress, 0);
}

#[test]
fn a_superblock_from_a_draft_version_is_refused() {
    // Ab 1.0: `version_major` MUSS 1 sein. Die `0.x`-Entwuerfe durften brechen,
    // und es gibt keine Arrays daraus — bis zum Freeze hat kein Code auf eine
    // echte Platte geschrieben.
    let mut encoded = sample_superblock().encode().unwrap();
    encoded[8..10].copy_from_slice(&0u16.to_le_bytes());
    let checksum = ferrite_format::checksum(&encoded[..4092]);
    encoded[4092..].copy_from_slice(&checksum.to_le_bytes());

    assert!(matches!(
        Superblock::decode(&encoded),
        Err(FormatError::UnsupportedVersion { major: 0, .. })
    ));
}

#[test]
fn an_unknown_member_state_is_refused() {
    let mut encoded = sample_superblock().encode().unwrap();
    encoded[144] = 3;
    let checksum = ferrite_format::checksum(&encoded[..4092]);
    encoded[4092..].copy_from_slice(&checksum.to_le_bytes());

    assert!(matches!(
        Superblock::decode(&encoded),
        Err(FormatError::UnknownMemberState(3))
    ));
}

#[test]
fn rebuild_progress_must_be_zero_unless_rebuilding() {
    for state in [MemberState::Clean, MemberState::Stale] {
        let mut superblock = sample_superblock();
        superblock.member_state = state;
        superblock.rebuild_progress = 65_536;
        assert!(
            matches!(
                superblock.encode(),
                Err(FormatError::InvalidField {
                    field: "rebuild_progress",
                    ..
                })
            ),
            "{state:?} darf keinen Fortschritt tragen"
        );
    }
}

#[test]
fn rebuild_progress_must_not_exceed_the_payload() {
    let mut superblock = sample_superblock();
    superblock.member_state = MemberState::Rebuilding;
    superblock.rebuild_progress = superblock.payload_size + superblock.parity_block_size();
    assert!(matches!(
        superblock.encode(),
        Err(FormatError::InvalidField {
            field: "rebuild_progress",
            reason: "groesser als payload_size"
        })
    ));
}

#[test]
fn rebuild_progress_must_sit_on_a_parity_block_boundary() {
    // Rekonstruiert wird blockweise. Ein Fortschritt mitten in einem Block
    // waere nach einem Absturz nicht wiederaufsetzbar — man wuesste nicht, ob
    // der angefangene Block schon stimmt.
    let mut superblock = sample_superblock();
    superblock.member_state = MemberState::Rebuilding;
    superblock.rebuild_progress = 4096; // Parity-Block ist 64 KiB
    assert!(matches!(
        superblock.encode(),
        Err(FormatError::InvalidField {
            field: "rebuild_progress",
            reason: "kein Vielfaches der Parity-Block-Groesse"
        })
    ));
}

#[test]
fn a_finished_rebuild_may_report_the_full_payload() {
    // Der Moment zwischen dem letzten Block und dem Umschalten auf Clean.
    let mut superblock = sample_superblock();
    superblock.member_state = MemberState::Rebuilding;
    superblock.rebuild_progress = superblock.payload_size;
    let encoded = superblock.encode().unwrap();
    assert_eq!(Superblock::decode(&encoded).unwrap(), superblock);
}

#[test]
fn a_log_member_must_be_clean() {
    // Die Log-Region ist von keiner Paritaet gedeckt. Ein leeres Log ist immer
    // zulaessig, es gibt daran nichts zu rekonstruieren.
    for state in [MemberState::Rebuilding, MemberState::Stale] {
        let mut superblock = sample_superblock();
        superblock.role = Role::Log;
        superblock.member_state = state;
        assert!(
            matches!(
                superblock.encode(),
                Err(FormatError::InvalidField {
                    field: "member_state",
                    ..
                })
            ),
            "Log darf nicht {state:?} sein"
        );
    }
}

#[test]
fn a_parity_member_may_rebuild() {
    // Eine getauschte Paritaetsplatte wird genauso wiederhergestellt wie eine
    // Datenplatte.
    let mut superblock = sample_superblock();
    superblock.role = Role::ParityP;
    superblock.member_state = MemberState::Rebuilding;
    superblock.rebuild_progress = 128 * 1024 * 1024;
    let encoded = superblock.encode().unwrap();
    assert_eq!(
        Superblock::decode(&encoded).unwrap().member_state,
        MemberState::Rebuilding
    );
}
