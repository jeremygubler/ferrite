// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Tests der Planungslogik.
//!
//! Kein Geraet, kein Zufall. Geprueft wird, dass kein Parity-Block unter den
//! Tisch faellt und dass ein Rebuild nur dann Fortschritt meldet, wenn die
//! Bloecke wirklich drankamen.

use ferrite_engine::{
    data_is_valid_at, dirty_blocks, total_blocks, BlockGeometry, EngineError, RebuildPlan,
    WriteTarget,
};
use ferrite_format::log::LogRecordHeader;
use ferrite_format::superblock::{MemberState, Role, Superblock};
use ferrite_format::{FormatError, Uuid};

/// 4-KiB-Bloecke, das kleinste vom Format erlaubte Mass.
const LOG2: u8 = 12;
const BLOCK: u64 = 1 << LOG2;

fn geometry(blocks: u64) -> BlockGeometry {
    BlockGeometry::new(LOG2, blocks)
}

fn member(role: Role, blocks: u64) -> Superblock {
    let mut superblock = Superblock::new(
        Uuid::from_random_bytes([0xA1; 16]),
        Uuid::from_random_bytes([0xB2; 16]),
        role,
        4,
        blocks * BLOCK,
    );
    superblock.parity_block_size_log2 = LOG2;
    superblock
}

fn write(offset: u64, len: u64) -> WriteTarget {
    WriteTarget {
        slot_index: 0,
        offset,
        len,
    }
}

// --- Geometrie -----------------------------------------------------------

#[test]
fn a_write_inside_one_block_touches_one_block() {
    let geometry = geometry(16);
    assert_eq!(geometry.blocks_touched(0, 1).unwrap(), 0..1);
    assert_eq!(geometry.blocks_touched(BLOCK - 1, 1).unwrap(), 0..1);
    assert_eq!(geometry.blocks_touched(BLOCK, 1).unwrap(), 1..2);
}

#[test]
fn a_write_across_a_boundary_touches_both_blocks() {
    // Der Fall, den man vergisst: Ein Write, der genau auf der Grenze sitzt,
    // macht zwei Bloecke dreckig. Wer nur den ersten neu rechnet, laesst die
    // Paritaet des zweiten veraltet zurueck.
    let geometry = geometry(16);
    assert_eq!(geometry.blocks_touched(BLOCK - 1, 2).unwrap(), 0..2);
    assert_eq!(geometry.blocks_touched(BLOCK - 1, BLOCK + 2).unwrap(), 0..3);
}

#[test]
fn a_write_of_length_zero_touches_nothing() {
    let geometry = geometry(16);
    assert!(geometry.blocks_touched(BLOCK * 3, 0).unwrap().is_empty());
}

#[test]
fn a_write_that_ends_exactly_at_a_boundary_stays_in_one_block() {
    let geometry = geometry(16);
    assert_eq!(geometry.blocks_touched(0, BLOCK).unwrap(), 0..1);
    assert_eq!(geometry.blocks_touched(0, BLOCK + 1).unwrap(), 0..2);
}

#[test]
fn a_write_past_the_last_block_is_refused() {
    let geometry = geometry(4);
    assert_eq!(
        geometry.blocks_touched(4 * BLOCK, 1),
        Err(EngineError::BeyondArray {
            block: 4,
            block_count: 4
        })
    );
    // Genau bis ans Ende ist erlaubt.
    assert_eq!(geometry.blocks_touched(4 * BLOCK - 1, 1).unwrap(), 3..4);
}

#[test]
fn an_offset_that_overflows_is_refused() {
    // `target_offset` und `payload_len` kommen ungeprueft von der Platte.
    let geometry = geometry(16);
    assert_eq!(
        geometry.blocks_touched(u64::MAX, 2),
        Err(EngineError::OffsetOverflow {
            offset: u64::MAX,
            len: 2
        })
    );
}

#[test]
fn the_byte_range_of_a_block_matches_the_block_size() {
    let geometry = geometry(16);
    assert_eq!(geometry.byte_range(0), 0..BLOCK);
    assert_eq!(geometry.byte_range(5), 5 * BLOCK..6 * BLOCK);
    assert_eq!(geometry.block_size(), BLOCK);
}

// --- Dreckige Bloecke ----------------------------------------------------

#[test]
fn the_slot_does_not_change_which_blocks_are_dirty() {
    // Paritaet wird ueber gleiche Offsets gebildet, nicht ueber Streifen.
    // Zwei Writes auf denselben Offset in verschiedenen Slots machen denselben
    // Block dreckig, und zwar genau einmal.
    let geometry = geometry(16);
    let writes = [
        WriteTarget {
            slot_index: 0,
            offset: 0,
            len: 512,
        },
        WriteTarget {
            slot_index: 3,
            offset: 512,
            len: 512,
        },
    ];
    assert_eq!(dirty_blocks(&geometry, &writes).unwrap(), vec![0..1]);
}

#[test]
fn overlapping_and_adjacent_ranges_are_merged() {
    let geometry = geometry(32);
    let writes = [
        write(0, BLOCK),             // 0..1
        write(BLOCK, BLOCK),         // 1..2, grenzt an
        write(5 * BLOCK, 2 * BLOCK), // 5..7
        write(6 * BLOCK, BLOCK),     // 6..7, ueberlappt
        write(20 * BLOCK, 1),        // 20..21
    ];
    assert_eq!(
        dirty_blocks(&geometry, &writes).unwrap(),
        vec![0..2, 5..7, 20..21]
    );
}

#[test]
fn unsorted_input_produces_sorted_output() {
    // Der Replay liefert Records in Sequenzreihenfolge, nicht nach Offset.
    let geometry = geometry(32);
    let writes = [write(20 * BLOCK, 1), write(0, 1), write(10 * BLOCK, 1)];
    let dirty = dirty_blocks(&geometry, &writes).unwrap();
    assert_eq!(dirty, vec![0..1, 10..11, 20..21]);
    assert_eq!(total_blocks(&dirty), 3);
}

#[test]
fn an_empty_batch_makes_nothing_dirty() {
    assert!(dirty_blocks(&geometry(16), &[]).unwrap().is_empty());
}

#[test]
fn a_write_target_comes_from_a_log_record() {
    let payload = vec![0u8; 600];
    let header = LogRecordHeader::write(7, 3, 8192, &payload);
    let target = WriteTarget::from_record(&header);
    assert_eq!(target.slot_index, 3);
    assert_eq!(target.offset, 8192);
    assert_eq!(target.len, 600);
}

// --- Rebuild -------------------------------------------------------------

#[test]
fn a_clean_member_has_nothing_to_rebuild() {
    let plan = RebuildPlan::resume(&member(Role::Data, 8), LOG2).unwrap();
    assert!(plan.is_complete());
    assert_eq!(plan.remaining_blocks(), 0);
    assert_eq!(plan.next_batch(4), None);
}

#[test]
fn a_stale_member_is_rebuilt_from_the_start() {
    let mut superblock = member(Role::Data, 8);
    superblock.member_state = MemberState::Stale;
    let plan = RebuildPlan::resume(&superblock, LOG2).unwrap();
    assert_eq!(plan.next_block(), 0);
    assert_eq!(plan.remaining_blocks(), 8);
}

#[test]
fn a_rebuild_resumes_where_it_stopped() {
    // Der Punkt der ganzen Uebung: Nach einem Absturz mitten im Rebuild wird
    // nicht von vorn angefangen.
    let mut superblock = member(Role::Data, 10);
    superblock.member_state = MemberState::Rebuilding;
    superblock.rebuild_progress = 6 * BLOCK;

    let plan = RebuildPlan::resume(&superblock, LOG2).unwrap();
    assert_eq!(plan.next_block(), 6);
    assert_eq!(plan.remaining_blocks(), 4);
    assert_eq!(plan.next_batch(3), Some(6..9));
}

#[test]
fn batches_walk_the_member_to_the_end() {
    let mut plan = RebuildPlan::from_scratch(LOG2, 10);
    let mut seen = Vec::new();
    while let Some(batch) = plan.next_batch(4) {
        seen.push(batch.clone());
        plan.complete_batch(batch).unwrap();
    }
    assert_eq!(seen, vec![0..4, 4..8, 8..10]);
    assert!(plan.is_complete());
    assert_eq!(plan.progress_bytes(), 10 * BLOCK);
}

#[test]
fn next_batch_does_not_move_the_plan() {
    // Ein Absturz zwischen Ausgabe und Abschluss darf nur kosten, dass der
    // Stapel noch einmal laeuft.
    let plan = RebuildPlan::from_scratch(LOG2, 10);
    assert_eq!(plan.next_batch(4), Some(0..4));
    assert_eq!(plan.next_batch(4), Some(0..4));
    assert_eq!(plan.next_block(), 0);
}

#[test]
fn a_skipped_batch_is_refused() {
    // Der gefaehrliche Aufruferfehler: Fortschritt melden fuer Bloecke, die
    // niemand rekonstruiert hat. Danach liest der Member Nullen als Nutzdaten.
    let mut plan = RebuildPlan::from_scratch(LOG2, 10);
    plan.complete_batch(0..4).unwrap();
    assert_eq!(
        plan.complete_batch(6..8),
        Err(EngineError::BatchOutOfOrder {
            expected: 4,
            got: 6
        })
    );
    assert_eq!(plan.next_block(), 4, "der Plan bleibt stehen");
}

#[test]
fn a_batch_past_the_end_is_refused() {
    let mut plan = RebuildPlan::from_scratch(LOG2, 10);
    assert_eq!(
        plan.complete_batch(0..11),
        Err(EngineError::BatchPastEnd { end: 11, limit: 10 })
    );
}

#[test]
fn the_progress_lands_in_the_superblock() {
    let mut superblock = member(Role::Data, 10);
    superblock.member_state = MemberState::Stale;

    let mut plan = RebuildPlan::resume(&superblock, LOG2).unwrap();
    plan.complete_batch(0..4).unwrap();
    plan.apply_to(&mut superblock).unwrap();
    assert_eq!(superblock.member_state, MemberState::Rebuilding);
    assert_eq!(superblock.rebuild_progress, 4 * BLOCK);

    // Und der Superblock ist danach schreibbar — die Regeln aus 4.2 halten.
    superblock.encode().unwrap();
}

#[test]
fn a_finished_rebuild_leaves_the_member_clean() {
    let mut superblock = member(Role::Data, 10);
    superblock.member_state = MemberState::Stale;

    let mut plan = RebuildPlan::resume(&superblock, LOG2).unwrap();
    plan.complete_batch(0..10).unwrap();
    plan.apply_to(&mut superblock).unwrap();

    assert_eq!(superblock.member_state, MemberState::Clean);
    assert_eq!(
        superblock.rebuild_progress, 0,
        "Abschnitt 4.2: Fortschritt nur bei Rebuilding"
    );
}

#[test]
fn a_log_member_cannot_be_rebuilt() {
    let mut superblock = member(Role::Log, 8);
    superblock.payload_size = 8 * 4096;
    assert_eq!(
        RebuildPlan::resume(&superblock, LOG2),
        Err(EngineError::CannotRebuild { role: Role::Log })
    );
}

#[test]
fn a_member_with_another_block_size_is_refused() {
    let mut superblock = member(Role::Data, 8);
    superblock.parity_block_size_log2 = 16;
    superblock.payload_size = 8 * (1 << 16);
    assert_eq!(
        RebuildPlan::resume(&superblock, LOG2),
        Err(EngineError::MismatchedBlockSize {
            array: LOG2,
            member: 16
        })
    );
}

#[test]
fn an_invalid_member_is_refused_before_planning() {
    let mut superblock = member(Role::Data, 8);
    superblock.slot_index = 99; // data_slot_count ist 4
    assert!(matches!(
        RebuildPlan::resume(&superblock, LOG2),
        Err(EngineError::Format(FormatError::InvalidField {
            field: "slot_index",
            ..
        }))
    ));
}

// --- Brauchbarkeit als Datenquelle ---------------------------------------

#[test]
fn a_clean_member_is_valid_everywhere() {
    let superblock = member(Role::Data, 8);
    for block in 0..8 {
        assert!(data_is_valid_at(&superblock, block, LOG2));
    }
}

#[test]
fn a_stale_member_is_valid_nowhere_inside_its_payload() {
    let mut superblock = member(Role::Data, 8);
    superblock.member_state = MemberState::Stale;
    for block in 0..8 {
        assert!(!data_is_valid_at(&superblock, block, LOG2));
    }
}

#[test]
fn a_rebuilding_member_is_valid_only_below_its_progress() {
    let mut superblock = member(Role::Data, 8);
    superblock.member_state = MemberState::Rebuilding;
    superblock.rebuild_progress = 5 * BLOCK;

    for block in 0..5 {
        assert!(data_is_valid_at(&superblock, block, LOG2), "Block {block}");
    }
    for block in 5..8 {
        assert!(!data_is_valid_at(&superblock, block, LOG2), "Block {block}");
    }
}

#[test]
fn beyond_its_own_payload_every_member_reads_as_zero() {
    // Zero-Extension: Ein kuerzerer Member liefert jenseits seines Endes
    // Nullbytes, und die sind der richtige Wert. Auch ein Stale-Member — dort
    // gibt es nichts zu rekonstruieren.
    let mut superblock = member(Role::Data, 4);
    superblock.member_state = MemberState::Stale;
    assert!(!data_is_valid_at(&superblock, 3, LOG2));
    assert!(data_is_valid_at(&superblock, 4, LOG2));
    assert!(data_is_valid_at(&superblock, 99, LOG2));
}
