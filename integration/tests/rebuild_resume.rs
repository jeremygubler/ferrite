//! Der Plattentausch mit Absturz mittendrin.
//!
//! Die Generalprobe in `rehearsal.rs` baut einen Member in einem Zug wieder
//! auf. Hier wird der Rebuild an einer beliebigen Stelle unterbrochen, der
//! Fortschritt aus dem Superblock zurueckgelesen und weitergemacht — und zwar
//! ohne dass irgendwo im Speicher noch ein Plan liegt. Genau dafuer sind
//! `member_state` und `rebuild_progress` im Format.

use ferrite_engine::{data_is_valid_at, BlockGeometry, RebuildPlan};
use ferrite_format::assemble::{assemble, ArrayLayout};
use ferrite_format::superblock::{MemberState, Role, Superblock, DEFAULT_PAYLOAD_OFFSET};
use ferrite_format::Uuid;
use ferrite_integration::MemoryMember;
use ferrite_parity::{compute_p, reconstruct_from_p, Slot};

const BLOCK_LOG2: u8 = 12;
const BLOCK: u64 = 1 << BLOCK_LOG2;
const DATA_SLOTS: u32 = 4;
const GENERATION: u64 = 23;

/// Blockzahl je Slot, absichtlich ungleich.
const SLOT_BLOCKS: [u64; 4] = [6, 2, 9, 4];
const PARITY_BLOCKS: u64 = 9;

struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed)
    }
    fn fill(&mut self, target: &mut [u8]) {
        for byte in target.iter_mut() {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *byte = ((self.0 >> 33) & 0xFF) as u8;
        }
    }
}

fn superblock(role: Role, slot_index: u16, payload_size: u64, seed: u8) -> Superblock {
    let mut superblock = Superblock::new(
        Uuid::from_random_bytes([0xC3; 16]),
        Uuid::from_random_bytes([seed; 16]),
        role,
        DATA_SLOTS,
        payload_size,
    );
    superblock.parity_block_size_log2 = BLOCK_LOG2;
    superblock.payload_offset = DEFAULT_PAYLOAD_OFFSET;
    superblock.slot_index = slot_index;
    superblock.generation = GENERATION;
    superblock
}

/// Members in verwuerfelter Reihenfolge — Position ist nie gleich `slot_index`.
fn build_array() -> Vec<MemoryMember> {
    [
        superblock(Role::Data, 3, SLOT_BLOCKS[3] * BLOCK, 0x13),
        superblock(Role::ParityP, 0, PARITY_BLOCKS * BLOCK, 0x50),
        superblock(Role::Data, 0, SLOT_BLOCKS[0] * BLOCK, 0x10),
        superblock(Role::Data, 2, SLOT_BLOCKS[2] * BLOCK, 0x12),
        superblock(Role::Data, 1, SLOT_BLOCKS[1] * BLOCK, 0x11),
    ]
    .into_iter()
    .map(|sb| MemoryMember::new(sb).expect("Member muss anlegbar sein"))
    .collect()
}

fn layout_of(members: &[MemoryMember]) -> ArrayLayout {
    let superblocks: Vec<Superblock> = members
        .iter()
        .map(|m| m.read_superblock().expect("Superblock muss lesbar sein"))
        .collect();
    assemble(&superblocks).expect("Array muss zusammengehen")
}

/// Ausschnitt eines Payloads, am eigenen Ende abgeschnitten.
///
/// Das **ist** die Zero-Extension: Wo der Member endet, endet der Ausschnitt,
/// und `parity` liest dahinter Nullbytes.
fn window(payload: &[u8], start: usize, end: usize) -> &[u8] {
    let start = start.min(payload.len());
    &payload[start..end.min(payload.len())]
}

fn fill_and_parity(members: &mut [MemoryMember], layout: &ArrayLayout, rng: &mut Lcg) {
    for slot in 0..DATA_SLOTS as u16 {
        let position = layout.data_position(slot).unwrap();
        let mut payload = vec![0u8; members[position].payload().len()];
        rng.fill(&mut payload);
        members[position].payload_mut().copy_from_slice(&payload);
    }

    let payloads: Vec<Vec<u8>> = (0..DATA_SLOTS as u16)
        .map(|slot| {
            members[layout.data_position(slot).unwrap()]
                .payload()
                .to_vec()
        })
        .collect();
    let slots: Vec<Slot<'_>> = payloads
        .iter()
        .enumerate()
        .map(|(index, bytes)| Slot::new(index as u8, bytes).unwrap())
        .collect();

    let parity_position = layout.parity_p_position();
    let mut p = vec![0u8; members[parity_position].payload().len()];
    compute_p(DATA_SLOTS as u8, &slots, &mut p).unwrap();
    members[parity_position].payload_mut().copy_from_slice(&p);
}

/// Rekonstruiert genau die Bloecke `batch` des Slots `target` aus P.
fn rebuild_batch(
    members: &mut [MemoryMember],
    layout: &ArrayLayout,
    target: u8,
    batch: &core::ops::Range<u64>,
) {
    let start = (batch.start * BLOCK) as usize;
    let end = (batch.end * BLOCK) as usize;

    let survivors: Vec<Vec<u8>> = (0..DATA_SLOTS as u16)
        .filter(|slot| *slot != u16::from(target))
        .map(|slot| {
            let payload = members[layout.data_position(slot).unwrap()].payload();
            window(payload, start, end).to_vec()
        })
        .collect();
    let slots: Vec<Slot<'_>> = (0..DATA_SLOTS as u16)
        .filter(|slot| *slot != u16::from(target))
        .zip(survivors.iter())
        .map(|(slot, bytes)| Slot::new(slot as u8, bytes).unwrap())
        .collect();

    let parity = members[layout.parity_p_position()].payload();
    let p = window(parity, start, end).to_vec();

    let mut out = vec![0u8; end - start];
    reconstruct_from_p(DATA_SLOTS as u8, target, &slots, &p, &mut out).unwrap();

    let position = layout.data_position(u16::from(target)).unwrap();
    members[position].payload_mut()[start..end].copy_from_slice(&out);
}

#[test]
fn a_rebuild_survives_a_crash_and_resumes_from_the_superblock() {
    let mut rng = Lcg::new(0x0B00_B1E5);
    let mut members = build_array();
    let layout = layout_of(&members);
    fill_and_parity(&mut members, &layout, &mut rng);

    let target = 2u8; // der laengste Slot, 9 Bloecke
    let position = layout.data_position(u16::from(target)).unwrap();
    let original = members[position].payload().to_vec();

    // Plattentausch: leere Platte, Superblock sagt `Stale`.
    members[position].wipe_payload();
    {
        let superblock = members[position].superblock_mut();
        superblock.member_state = MemberState::Stale;
        superblock.generation += 1;
    }
    members[position].write_superblocks().unwrap();

    let geometry = BlockGeometry::new(BLOCK_LOG2, PARITY_BLOCKS);
    let mut rounds = 0;
    let mut batches = Vec::new();

    // Jede Runde beginnt damit, den Plan aus dem Superblock **neu** zu lesen —
    // so, als waere die Maschine nach der letzten Runde abgestuerzt und gerade
    // wieder hochgekommen. Im Speicher bleibt nichts erhalten.
    loop {
        let from_disk = members[position].read_superblock().unwrap();
        let mut plan = RebuildPlan::resume(&from_disk, geometry.block_size_log2()).unwrap();
        let Some(batch) = plan.next_batch(2) else {
            break;
        };
        batches.push(batch.clone());

        rebuild_batch(&mut members, &layout, target, &batch);
        plan.complete_batch(batch).unwrap();

        // Erst die Bloecke, dann der Fortschritt. Andersherum waeren nach einem
        // Absturz Bloecke als fertig gemeldet, die nie geschrieben wurden.
        {
            let superblock = members[position].superblock_mut();
            plan.apply_to(superblock).unwrap();
            superblock.generation += 1;
        }
        members[position].write_superblocks().unwrap();

        rounds += 1;
        assert!(rounds <= 10, "der Rebuild kommt nicht voran");
    }

    // 9 Bloecke in Zweierschritten: 0..2, 2..4, 4..6, 6..8, 8..9.
    assert_eq!(batches, vec![0..2, 2..4, 4..6, 6..8, 8..9]);
    assert_eq!(members[position].payload(), original.as_slice());

    let finished = members[position].read_superblock().unwrap();
    assert_eq!(finished.member_state, MemberState::Clean);
    assert_eq!(finished.rebuild_progress, 0);
}

#[test]
fn a_half_rebuilt_member_is_only_usable_below_its_progress() {
    // Waehrend der Rebuild laeuft, darf der Member nicht als Quelle fuer die
    // Rekonstruktion eines *anderen* Members dienen — jedenfalls nicht
    // oberhalb seines Fortschritts. Dort steht noch nichts.
    let mut rng = Lcg::new(0xFEED_F00D);
    let mut members = build_array();
    let layout = layout_of(&members);
    fill_and_parity(&mut members, &layout, &mut rng);

    let target = 2u8;
    let position = layout.data_position(u16::from(target)).unwrap();
    members[position].wipe_payload();

    let geometry = BlockGeometry::new(BLOCK_LOG2, PARITY_BLOCKS);
    let mut plan = RebuildPlan::from_scratch(geometry.block_size_log2(), SLOT_BLOCKS[2]);
    let batch = plan.next_batch(4).unwrap();
    rebuild_batch(&mut members, &layout, target, &batch);
    plan.complete_batch(batch).unwrap();
    {
        let superblock = members[position].superblock_mut();
        plan.apply_to(superblock).unwrap();
    }
    members[position].write_superblocks().unwrap();

    let half = members[position].read_superblock().unwrap();
    assert_eq!(half.member_state, MemberState::Rebuilding);
    assert_eq!(half.rebuild_progress, 4 * BLOCK);

    for block in 0..4 {
        assert!(
            data_is_valid_at(&half, block, BLOCK_LOG2),
            "Block {block} ist wiederhergestellt"
        );
    }
    for block in 4..SLOT_BLOCKS[2] {
        assert!(
            !data_is_valid_at(&half, block, BLOCK_LOG2),
            "Block {block} steht noch aus"
        );
    }
}

#[test]
fn rebuilding_a_short_member_stops_at_its_own_end() {
    // Slot 1 hat 2 Bloecke, das Array 9. Der Rebuild darf nur die eigenen
    // beruehren — dahinter liegt beim Member nichts, was zu fuellen waere.
    let mut rng = Lcg::new(0x1234_5678);
    let mut members = build_array();
    let layout = layout_of(&members);
    fill_and_parity(&mut members, &layout, &mut rng);

    let target = 1u8;
    let position = layout.data_position(u16::from(target)).unwrap();
    let original = members[position].payload().to_vec();
    assert_eq!(original.len() as u64, SLOT_BLOCKS[1] * BLOCK);

    members[position].wipe_payload();
    {
        let superblock = members[position].superblock_mut();
        superblock.member_state = MemberState::Stale;
    }
    members[position].write_superblocks().unwrap();

    let from_disk = members[position].read_superblock().unwrap();
    let mut plan = RebuildPlan::resume(&from_disk, BLOCK_LOG2).unwrap();
    assert_eq!(plan.end_block(), SLOT_BLOCKS[1], "nur die eigenen Bloecke");

    let batch = plan.next_batch(100).unwrap();
    assert_eq!(batch, 0..SLOT_BLOCKS[1]);
    rebuild_batch(&mut members, &layout, target, &batch);
    plan.complete_batch(batch).unwrap();
    assert!(plan.is_complete());

    assert_eq!(members[position].payload(), original.as_slice());
}
