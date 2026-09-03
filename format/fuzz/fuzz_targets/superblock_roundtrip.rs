//! Aus dem Fuzz-Input einen *gueltigen* Superblock bauen, kodieren, dekodieren,
//! auf Gleichheit pruefen.
//!
//! Der Unterschied zu `superblock_decode`: Dort kommen die Bytes von der
//! Platte, hier kommen sie aus dem Programm. Wenn `validate()` eine Eingabe
//! durchlaesst, `encode()` sie schreibt und `decode()` etwas anderes
//! zurueckgibt, ist ein Feld nicht verlustfrei darstellbar — und das faellt
//! spaeter als stille Aenderung an den Metadaten auf.

#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use ferrite_format::superblock::{MemberState, Role, Superblock, MAX_DATA_SLOTS};
use ferrite_format::Uuid;
use libfuzzer_sys::fuzz_target;

const LABEL_LEN: usize = 32;
/// Kleinster erlaubter `payload_offset`: hinter dem primaeren Superblock.
const MIN_PAYLOAD_OFFSET_BLOCKS: u64 = 17;

fn build(u: &mut Unstructured) -> arbitrary::Result<Superblock> {
    let role = match u8::arbitrary(u)? % 4 {
        0 => Role::Data,
        1 => Role::ParityP,
        2 => Role::ParityQ,
        _ => Role::Log,
    };
    let parity_block_size_log2 = 12 + u8::arbitrary(u)? % 13;
    let data_slot_count = 1 + u32::arbitrary(u)? % MAX_DATA_SLOTS;

    let payload_size = if role == Role::Log {
        // Ringpuffer: nur 4096-aligned, Laenge null ist erlaubt.
        (u64::arbitrary(u)? % 4096) * 4096
    } else {
        (1 + u64::arbitrary(u)? % 4096) << parity_block_size_log2
    };

    let mut superblock = Superblock::new(
        Uuid::from_random_bytes(<[u8; 16]>::arbitrary(u)?),
        Uuid::from_random_bytes(<[u8; 16]>::arbitrary(u)?),
        role,
        data_slot_count,
        payload_size,
    );
    superblock.parity_block_size_log2 = parity_block_size_log2;
    superblock.payload_offset = (MIN_PAYLOAD_OFFSET_BLOCKS + u64::arbitrary(u)? % 1024) * 4096;
    superblock.slot_index = if role == Role::Data {
        (u32::arbitrary(u)? % data_slot_count) as u16
    } else {
        u16::arbitrary(u)?
    };
    // Abschnitt 4.2. Ein Log-Member muss Clean bleiben, und ein Fortschritt gibt
    // es nur bei Rebuilding — auf einer Parity-Block-Grenze und innerhalb der
    // Payload.
    superblock.member_state = match u8::arbitrary(u)? % 3 {
        1 if role != Role::Log => MemberState::Rebuilding,
        2 if role != Role::Log => MemberState::Stale,
        _ => MemberState::Clean,
    };
    if superblock.member_state == MemberState::Rebuilding {
        let blocks = payload_size >> parity_block_size_log2;
        superblock.rebuild_progress = (u64::arbitrary(u)? % (blocks + 1)) << parity_block_size_log2;
    }

    superblock.version_minor = u16::arbitrary(u)?;
    superblock.generation = u64::arbitrary(u)?;
    superblock.created_unix = u64::arbitrary(u)?;
    superblock.feature_compat = u64::arbitrary(u)?;
    superblock.feature_incompat = u64::arbitrary(u)?;
    superblock.feature_ro_compat = u64::arbitrary(u)?;

    // Beliebiges UTF-8, auf 32 Bytes an einer Zeichengrenze gekuerzt. Nullbytes
    // fallen raus: Abschnitt 4 laesst sie im Label nicht zu, weil das Feld mit
    // ihnen aufgefuellt wird. Dass diese Regel durchgesetzt wird, prueft
    // `superblock_rejects_label_with_interior_nul` in den Roundtrip-Tests —
    // hier geht es um alles andere, was ein Label sein kann.
    let mut label: String = String::arbitrary(u)?
        .chars()
        .filter(|&c| c != '\0')
        .collect();
    while label.len() > LABEL_LEN {
        label.pop();
    }
    superblock.label = label;

    Ok(superblock)
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let Ok(superblock) = build(&mut u) else {
        return;
    };

    // `build` erzeugt ausschliesslich Werte, die die Regeln aus Abschnitt 4
    // erfuellen. Wird hier abgelehnt, widersprechen sich Konstruktion und
    // `validate()`.
    let encoded = superblock
        .encode()
        .expect("nach den Regeln gebauter Superblock muss kodieren");
    let decoded = Superblock::decode(&encoded).expect("eigenes Encoding muss lesbar sein");
    assert_eq!(decoded, superblock, "Roundtrip veraendert den Superblock");

    // Zweite Runde: stabil, nicht nur einmal richtig.
    assert_eq!(decoded.encode().expect("stabil"), encoded);
});
