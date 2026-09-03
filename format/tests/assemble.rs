//! Tests gegen Abschnitt 2.1 von `docs/FORMAT.md`.
//!
//! Zu jeder der sechs Assemble-Regeln gehoert hier ein Test, der ihre
//! Verletzung nachweist — nicht nur der Happy Path. Ein Array, das mit einem
//! fehlenden Slot oder einem zu kurzen Parity-Member zusammengesetzt wird,
//! rechnet Paritaet, aus der sich nichts rekonstruieren laesst.

use ferrite_format::assemble::{assemble, MAX_MEMBERS};
use ferrite_format::error::FormatError;
use ferrite_format::superblock::{Role, Superblock};
use ferrite_format::Uuid;

/// Parity-Block-Groesse, die `Superblock::new` vorgibt: 64 KiB.
const BLOCK: u64 = 64 * 1024;
const DATA_SLOTS: u32 = 4;

fn array_uuid() -> Uuid {
    Uuid::from_random_bytes([0xA1; 16])
}

/// Ein Member. `seed` unterscheidet die `member_uuid`, `blocks` die Laenge.
fn member(role: Role, slot_index: u16, blocks: u64, seed: u8) -> Superblock {
    let mut superblock = Superblock::new(
        array_uuid(),
        Uuid::from_random_bytes([seed; 16]),
        role,
        DATA_SLOTS,
        blocks * BLOCK,
    );
    superblock.slot_index = slot_index;
    superblock
}

/// Vier gleich lange Data-Members, ein ParityP, ein ParityQ, ein Log.
fn healthy() -> Vec<Superblock> {
    let mut members: Vec<Superblock> = (0..DATA_SLOTS)
        .map(|slot| member(Role::Data, slot as u16, 16, 0x10 + slot as u8))
        .collect();
    members.push(member(Role::ParityP, 0, 16, 0x40));
    members.push(member(Role::ParityQ, 0, 16, 0x50));
    members.push(member(Role::Log, 0, 4, 0x60));
    members
}

#[test]
fn assembles_a_healthy_array() {
    let members = healthy();
    let layout = assemble(&members).expect("gesundes Array muss zusammengehen");

    assert_eq!(layout.array_uuid(), array_uuid());
    assert_eq!(layout.data_slot_count(), DATA_SLOTS);
    assert_eq!(layout.parity_block_size(), BLOCK);

    for slot in 0..DATA_SLOTS as u16 {
        assert_eq!(layout.data_position(slot), Some(usize::from(slot)));
    }
    assert_eq!(layout.data_position(DATA_SLOTS as u16), None);

    assert_eq!(layout.parity_p_position(), 4);
    assert_eq!(layout.parity_q_position(), Some(5));
    assert_eq!(layout.log_position(), Some(6));
    assert!(layout.has_parity_q());
    assert!(layout.has_log());
}

#[test]
fn parity_q_and_log_are_optional() {
    // Regel 4 verlangt genau einen ParityP, die anderen beiden Rollen nur
    // hoechstens einmal. Ein Array mit P allein toleriert einen Ausfall.
    let mut members: Vec<Superblock> = (0..DATA_SLOTS)
        .map(|slot| member(Role::Data, slot as u16, 8, 0x10 + slot as u8))
        .collect();
    members.push(member(Role::ParityP, 0, 8, 0x40));

    let layout = assemble(&members).unwrap();
    assert!(!layout.has_parity_q());
    assert!(!layout.has_log());
    assert_eq!(layout.parity_q_position(), None);
    assert_eq!(layout.log_position(), None);
}

#[test]
fn the_order_of_the_members_does_not_matter() {
    // Members kommen von der Platte in der Reihenfolge, in der sie gefunden
    // wurden. Das Layout darf davon nicht abhaengen.
    let mut shuffled = healthy();
    shuffled.reverse();

    let layout = assemble(&shuffled).unwrap();
    for slot in 0..DATA_SLOTS as u16 {
        let position = layout.data_position(slot).unwrap();
        assert_eq!(shuffled[position].slot_index, slot);
        assert_eq!(shuffled[position].role, Role::Data);
    }
    assert_eq!(shuffled[layout.parity_p_position()].role, Role::ParityP);
}

#[test]
fn data_positions_are_ordered_by_slot_index_not_by_position() {
    // Fuer die Q-Rechnung zaehlt der Slot-Index. Wer hier die Reihenfolge des
    // Slices durchreicht, bekommt die falschen Koeffizienten.
    let mut shuffled = healthy();
    shuffled.reverse();

    let layout = assemble(&shuffled).unwrap();
    let slots: Vec<u16> = layout
        .data_positions()
        .map(|position| shuffled[position].slot_index)
        .collect();
    assert_eq!(slots, vec![0, 1, 2, 3]);
}

// --- Regel 1 -------------------------------------------------------------

#[test]
fn rejects_an_empty_member_list() {
    assert_eq!(assemble(&[]), Err(FormatError::NoMembers));
}

#[test]
fn rejects_a_member_that_is_invalid_on_its_own() {
    let mut members = healthy();
    members[2].slot_index = 99; // Regel aus Abschnitt 4
    assert!(matches!(
        assemble(&members),
        Err(FormatError::InvalidField {
            field: "slot_index",
            ..
        })
    ));
}

#[test]
fn rejects_more_members_than_the_format_allows() {
    // Aus Regel 4 und 5 folgt die Obergrenze. Sie muss vor allem anderen
    // greifen, weil die Positionen sonst nicht mehr in die Slot-Tabelle passen.
    let members: Vec<Superblock> = (0..MAX_MEMBERS + 1)
        .map(|index| member(Role::Data, 0, 1, index as u8))
        .collect();
    assert_eq!(
        assemble(&members),
        Err(FormatError::TooManyMembers {
            max: MAX_MEMBERS,
            got: MAX_MEMBERS + 1
        })
    );
}

// --- Regel 2 -------------------------------------------------------------

#[test]
fn rejects_a_foreign_array_uuid() {
    // Der Fall, der ohne diese Pruefung am teuersten waere: eine Platte aus
    // einem anderen Array, die zufaellig einen freien Slot besetzt.
    let mut members = healthy();
    members[2].array_uuid = Uuid::from_random_bytes([0xEE; 16]);
    assert_eq!(
        assemble(&members),
        Err(FormatError::MismatchedArrayParameter {
            field: "array_uuid",
            member: 2
        })
    );
}

#[test]
fn rejects_a_differing_parity_block_size() {
    let mut members = healthy();
    members[3].parity_block_size_log2 = 20;
    // payload_size muss zur neuen Blockgroesse passen, sonst schlaegt schon
    // Abschnitt 4 zu und die Array-Regel kaeme nie dran.
    members[3].payload_size = 16 * (1 << 20);
    assert_eq!(
        assemble(&members),
        Err(FormatError::MismatchedArrayParameter {
            field: "parity_block_size_log2",
            member: 3
        })
    );
}

#[test]
fn rejects_a_differing_data_slot_count() {
    let mut members = healthy();
    members[1].data_slot_count = DATA_SLOTS + 1;
    assert_eq!(
        assemble(&members),
        Err(FormatError::MismatchedArrayParameter {
            field: "data_slot_count",
            member: 1
        })
    );
}

// --- Regel 3 -------------------------------------------------------------

#[test]
fn rejects_the_same_member_twice() {
    // Nach einer Kopie mit `dd` liegen zwei Platten mit derselben member_uuid
    // im System. Wer beide aufnimmt, rechnet einen Slot doppelt in die
    // Paritaet.
    let mut members = healthy();
    members[3].member_uuid = members[1].member_uuid;
    assert_eq!(
        assemble(&members),
        Err(FormatError::DuplicateMemberUuid {
            first: 1,
            second: 3
        })
    );
}

// --- Regel 4 -------------------------------------------------------------

#[test]
fn rejects_a_missing_parity_p() {
    let members: Vec<Superblock> = (0..DATA_SLOTS)
        .map(|slot| member(Role::Data, slot as u16, 8, 0x10 + slot as u8))
        .collect();
    assert_eq!(assemble(&members), Err(FormatError::MissingParityP));
}

#[test]
fn rejects_two_parity_p_members() {
    let mut members = healthy();
    members.push(member(Role::ParityP, 0, 16, 0x70));
    assert_eq!(
        assemble(&members),
        Err(FormatError::DuplicateRole {
            role: Role::ParityP,
            first: 4,
            second: 7
        })
    );
}

#[test]
fn rejects_two_parity_q_members() {
    let mut members = healthy();
    members.push(member(Role::ParityQ, 0, 16, 0x71));
    assert_eq!(
        assemble(&members),
        Err(FormatError::DuplicateRole {
            role: Role::ParityQ,
            first: 5,
            second: 7
        })
    );
}

#[test]
fn rejects_two_log_members() {
    let mut members = healthy();
    members.push(member(Role::Log, 0, 4, 0x72));
    assert_eq!(
        assemble(&members),
        Err(FormatError::DuplicateRole {
            role: Role::Log,
            first: 6,
            second: 7
        })
    );
}

// --- Regel 5 -------------------------------------------------------------

#[test]
fn rejects_two_members_in_the_same_slot() {
    let mut members = healthy();
    members[3].slot_index = 1;
    assert_eq!(
        assemble(&members),
        Err(FormatError::DuplicateDataSlot {
            slot_index: 1,
            first: 1,
            second: 3
        })
    );
}

#[test]
fn rejects_a_missing_slot() {
    // Der gefaehrlichste Fall: Es sieht aus wie ein Array, nur ein Slot fehlt.
    // Ohne diese Pruefung liefe die Paritaetsrechnung ueber drei statt vier
    // Members durch und ergaebe eine Paritaet, die zu nichts passt.
    let mut members = healthy();
    members.remove(2);
    assert_eq!(
        assemble(&members),
        Err(FormatError::MissingDataSlot { slot_index: 2 })
    );
}

// --- Regel 6 -------------------------------------------------------------

#[test]
fn data_members_may_have_different_sizes() {
    // Der eigentliche Punkt des Layouts. Solange die Paritaet den laengsten
    // Data-Member abdeckt, ist jede Mischung erlaubt.
    let mut members = vec![
        member(Role::Data, 0, 1, 0x10),
        member(Role::Data, 1, 40, 0x11),
        member(Role::Data, 2, 7, 0x12),
        member(Role::Data, 3, 40, 0x13),
    ];
    members.push(member(Role::ParityP, 0, 40, 0x40));
    members.push(member(Role::ParityQ, 0, 64, 0x50)); // laenger als noetig

    assert!(assemble(&members).is_ok());
}

#[test]
fn rejects_a_parity_p_shorter_than_the_longest_data_member() {
    let mut members = healthy();
    members[2].payload_size = 32 * BLOCK; // Slot 2 waechst ueber die Paritaet
    assert_eq!(
        assemble(&members),
        Err(FormatError::ParityTooShort {
            role: Role::ParityP,
            parity_size: 16 * BLOCK,
            data_size: 32 * BLOCK,
            slot_index: 2
        })
    );
}

#[test]
fn rejects_a_parity_q_shorter_than_the_longest_data_member() {
    // Abschnitt 2.1 Regel 6 nennt ParityQ ausdruecklich mit. Ein zu kurzer Q
    // liesse die Zwei-Slot-Rekonstruktion still auf halber Strecke enden.
    let mut members = healthy();
    members[5].payload_size = 8 * BLOCK;
    assert_eq!(
        assemble(&members),
        Err(FormatError::ParityTooShort {
            role: Role::ParityQ,
            parity_size: 8 * BLOCK,
            data_size: 16 * BLOCK,
            slot_index: 0
        })
    );
}

#[test]
fn the_log_member_has_no_length_requirement() {
    // Die Log-Region ist ein Ringpuffer und hat mit der Paritaetslaenge nichts
    // zu tun. Ein winziges Log-Geraet ist erlaubt.
    let mut members = healthy();
    members[6].payload_size = 4096;
    assert!(assemble(&members).is_ok());
}
