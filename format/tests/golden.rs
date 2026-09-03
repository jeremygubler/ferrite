//! Golden Vectors: das eingefrorene Byte-Layout, von Hand festgeschrieben.
//!
//! Jeder andere Test in diesem Crate laeuft `encode` → `decode` durch denselben
//! Code. Wer zwei Feldoffsets vertauscht, bleibt dort gruen — die Rechnung geht
//! ja in beide Richtungen gleich falsch auf. Hier liegen die Bytes als
//! Literale, gegen `docs/FORMAT.md` Abschnitt 4 und 5.1 von Hand geprueft.
//!
//! **Diese Datei aendert sich nicht mehr.** Ab Version 1.0 sagt das Format zu,
//! dass jede spaetere `1.y`-Implementierung jedes `1.x`-Array liest. Schlaegt
//! einer dieser Tests fehl, ist genau diese Zusage gebrochen — und die richtige
//! Reaktion ist, den Code zurueckzunehmen, nicht die Erwartung anzupassen.

use ferrite_format::log::{LogRecordHeader, RecordType, LOG_HEADER_SIZE};
use ferrite_format::superblock::{MemberState, Role, Superblock, SUPERBLOCK_SIZE};
use ferrite_format::Uuid;

/// Die ersten 160 Bytes eines Superblocks. Dahinter bis zur Pruefsumme nur
/// Nullbytes.
#[rustfmt::skip]
const GOLDEN_SUPERBLOCK_HEAD: [u8; 160] = [
    // 0: magic "FERRITE1"
    0x46, 0x45, 0x52, 0x52, 0x49, 0x54, 0x45, 0x31,
    // 8: version_major = 1, version_minor = 0
    0x01, 0x00, 0x00, 0x00,
    // 12: header_size = 4096
    0x00, 0x10, 0x00, 0x00,
    // 16: array_uuid
    0x0F, 0x1E, 0x2D, 0x3C, 0x4B, 0x5A, 0x46, 0x78,
    0x89, 0x9A, 0xAB, 0xBC, 0xCD, 0xDE, 0xEF, 0xF0,
    // 32: member_uuid
    0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x47, 0x88,
    0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00,
    // 48: role = Data, 49: parity_block_size_log2 = 16
    0x00, 0x10,
    // 50: slot_index = 3
    0x03, 0x00,
    // 52: data_slot_count = 6
    0x06, 0x00, 0x00, 0x00,
    // 56: payload_offset = 1048576
    0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 64: payload_size = 536870912
    0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00, 0x00,
    // 72: generation = 0x0102030405060708
    0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
    // 80: created_unix = 1756000000
    0x00, 0x6F, 0xAA, 0x68, 0x00, 0x00, 0x00, 0x00,
    // 88: feature_compat
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 96: feature_incompat
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 104: feature_ro_compat
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 112: label "ferrite-golden", mit Nullbytes aufgefuellt
    0x66, 0x65, 0x72, 0x72, 0x69, 0x74, 0x65, 0x2D,
    0x67, 0x6F, 0x6C, 0x64, 0x65, 0x6E, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 144: member_state = Rebuilding
    0x01,
    // 145: reserviert
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 152: rebuild_progress = 67108864
    0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00,
];

/// CRC-32C ueber die Bytes `0..4092` des Superblocks oben.
const GOLDEN_SUPERBLOCK_CRC: u32 = 0xAA3F_756E;

#[rustfmt::skip]
const GOLDEN_LOG_HEADER: [u8; LOG_HEADER_SIZE] = [
    // 0: magic "FLOG"
    0x46, 0x4C, 0x4F, 0x47,
    // 4: record_type = Write
    0x01, 0x00,
    // 6: header_size = 64
    0x40, 0x00,
    // 8: seq = 0x0A0B0C0D0E0F1011
    0x11, 0x10, 0x0F, 0x0E, 0x0D, 0x0C, 0x0B, 0x0A,
    // 16: target_offset = 2097152
    0x00, 0x00, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 24: payload_len = 600
    0x58, 0x02, 0x00, 0x00,
    // 28: slot_index = 3
    0x03, 0x00,
    // 30: reserviert
    0x00, 0x00,
    // 32: generation = 0x0102030405060708
    0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
    // 40: commit_unix = 1756000123
    0x7B, 0x6F, 0xAA, 0x68, 0x00, 0x00, 0x00, 0x00,
    // 48: payload_crc32c
    0x25, 0xC5, 0x77, 0x62,
    // 52: reserviert
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 60: header_crc32c ueber 0..60
    0xA0, 0x88, 0xD1, 0x63,
];

/// Der Superblock, den die Bytes oben beschreiben.
fn golden_superblock() -> Superblock {
    let mut superblock = Superblock::new(
        Uuid::from_bytes([
            0x0F, 0x1E, 0x2D, 0x3C, 0x4B, 0x5A, 0x46, 0x78, 0x89, 0x9A, 0xAB, 0xBC, 0xCD, 0xDE,
            0xEF, 0xF0,
        ]),
        Uuid::from_bytes([
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x47, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE,
            0xFF, 0x00,
        ]),
        Role::Data,
        6,
        512 * 1024 * 1024,
    );
    superblock.parity_block_size_log2 = 16;
    superblock.slot_index = 3;
    superblock.generation = 0x0102_0304_0506_0708;
    superblock.created_unix = 1_756_000_000;
    superblock.label = "ferrite-golden".to_string();
    superblock.member_state = MemberState::Rebuilding;
    superblock.rebuild_progress = 64 * 1024 * 1024;
    superblock
}

/// Die Nutzdaten, ueber die `payload_crc32c` im Golden-Header gebildet ist.
fn golden_payload() -> Vec<u8> {
    (0u8..=255).cycle().take(600).collect()
}

fn golden_superblock_bytes() -> [u8; SUPERBLOCK_SIZE] {
    let mut block = [0u8; SUPERBLOCK_SIZE];
    block[..GOLDEN_SUPERBLOCK_HEAD.len()].copy_from_slice(&GOLDEN_SUPERBLOCK_HEAD);
    block[4092..].copy_from_slice(&GOLDEN_SUPERBLOCK_CRC.to_le_bytes());
    block
}

#[test]
fn the_superblock_encodes_to_exactly_these_bytes() {
    // Die Richtung, die zaehlt: Was dieser Code schreibt, muessen alle
    // spaeteren Versionen lesen koennen.
    let encoded = golden_superblock().encode().unwrap();
    assert_eq!(
        &encoded[..160],
        &GOLDEN_SUPERBLOCK_HEAD,
        "Feld-Layout im Superblock hat sich geaendert"
    );
    assert!(
        encoded[160..4092].iter().all(|&b| b == 0),
        "reservierter Bereich ist nicht null"
    );
    assert_eq!(
        u32::from_le_bytes([encoded[4092], encoded[4093], encoded[4094], encoded[4095]]),
        GOLDEN_SUPERBLOCK_CRC,
        "Pruefsumme oder Pruefbereich hat sich geaendert"
    );
}

#[test]
fn these_bytes_decode_to_exactly_this_superblock() {
    // Die Gegenrichtung: fremde Bytes, unsere Interpretation.
    let decoded = Superblock::decode(&golden_superblock_bytes()).unwrap();
    assert_eq!(decoded, golden_superblock());

    // Einzeln nachgezogen, damit ein Fehlschlag sagt, welches Feld verrutscht
    // ist, statt nur "die Structs sind verschieden".
    assert_eq!(decoded.version_major, 1);
    assert_eq!(decoded.version_minor, 0);
    assert_eq!(decoded.role, Role::Data);
    assert_eq!(decoded.parity_block_size_log2, 16);
    assert_eq!(decoded.slot_index, 3);
    assert_eq!(decoded.data_slot_count, 6);
    assert_eq!(decoded.payload_offset, 1_048_576);
    assert_eq!(decoded.payload_size, 536_870_912);
    assert_eq!(decoded.generation, 0x0102_0304_0506_0708);
    assert_eq!(decoded.created_unix, 1_756_000_000);
    assert_eq!(decoded.label, "ferrite-golden");
    assert_eq!(decoded.member_state, MemberState::Rebuilding);
    assert_eq!(decoded.rebuild_progress, 67_108_864);
}

#[test]
fn the_log_header_encodes_to_exactly_these_bytes() {
    let payload = golden_payload();
    let mut header = LogRecordHeader::write(0x0A0B_0C0D_0E0F_1011, 3, 0x0020_0000, &payload);
    header.generation = 0x0102_0304_0506_0708;
    header.commit_unix = 1_756_000_123;

    assert_eq!(
        header.encode(),
        GOLDEN_LOG_HEADER,
        "Feld-Layout im Log-Record-Header hat sich geaendert"
    );
}

#[test]
fn these_bytes_decode_to_exactly_this_log_header() {
    let decoded = LogRecordHeader::decode(&GOLDEN_LOG_HEADER).unwrap();

    assert_eq!(decoded.record_type, RecordType::Write);
    assert_eq!(decoded.seq, 0x0A0B_0C0D_0E0F_1011);
    assert_eq!(decoded.target_offset, 0x0020_0000);
    assert_eq!(decoded.payload_len, 600);
    assert_eq!(decoded.slot_index, 3);
    assert_eq!(decoded.generation, 0x0102_0304_0506_0708);
    assert_eq!(decoded.commit_unix, 1_756_000_123);
    assert_eq!(decoded.payload_crc32c, 0x6277_C525);

    // Und die Nutzdaten passen dazu — die Pruefsumme im Header ist keine
    // beliebige Zahl, sondern gegen echte Bytes gerechnet.
    decoded.verify_payload(&golden_payload()).unwrap();
    assert_eq!(decoded.on_disk_len(), 4096);
}

#[test]
fn the_checksums_cover_the_documented_ranges() {
    // Abschnitt 4: crc32c ueber Bytes 0..4092. Abschnitt 5.1: header_crc32c
    // ueber Bytes 0..60. Ein gekipptes Bit irgendwo darin muss auffallen —
    // sonst schuetzt die Pruefsumme einen Teil der Struktur nicht.
    let block = golden_superblock_bytes();
    for index in 0..4092 {
        let mut corrupted = block;
        corrupted[index] ^= 0x01;
        assert!(
            Superblock::decode(&corrupted).is_err(),
            "Superblock-Bitflip an Offset {index} unentdeckt"
        );
    }

    for index in 0..60 {
        let mut corrupted = GOLDEN_LOG_HEADER;
        corrupted[index] ^= 0x01;
        assert!(
            LogRecordHeader::decode(&corrupted).is_err(),
            "Log-Header-Bitflip an Offset {index} unentdeckt"
        );
    }
}
