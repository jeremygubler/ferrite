// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Die Generalprobe: ein vollstaendiges Array, einmal durch den ganzen Weg.
//!
//! Superbloecke schreiben → zurueckleseen → assemble → Log schreiben → Replay →
//! Writes anwenden → Paritaet rechnen → Member verlieren → rekonstruieren →
//! byteweise vergleichen. Ohne ein einziges Blockgeraet.
//!
//! Das ist die Probe auf die Naht zwischen `format` und `parity`. Die Members
//! werden deshalb absichtlich in verwuerfelter Reihenfolge angelegt: Wer die
//! Position im Slice statt `slot_index` als Q-Koeffizienten nimmt, faellt hier
//! auf und nirgends sonst.

use ferrite_format::assemble::{assemble, ArrayLayout};
use ferrite_format::log::ring::{LogRing, LogWriter};
use ferrite_format::log::{LogRecordHeader, RecordType};
use ferrite_format::superblock::{Role, Superblock, DEFAULT_PAYLOAD_OFFSET};
use ferrite_format::Uuid;
use ferrite_integration::MemoryMember;
use ferrite_parity::{
    compute_p, compute_q, reconstruct_data_and_p, reconstruct_data_and_q, reconstruct_from_p,
    reconstruct_from_q, reconstruct_two_from_pq, Slot,
};

/// 4 KiB Parity-Bloecke, die kleinste erlaubte Groesse — haelt die Probe klein.
const BLOCK_LOG2: u8 = 12;
const BLOCK: u64 = 1 << BLOCK_LOG2;
const DATA_SLOTS: u32 = 4;
const GENERATION: u64 = 11;

/// Blockzahl je Slot. Bewusst verschieden: gemischte Plattengroessen sind der
/// Sinn des Layouts, nicht ein Randfall.
const SLOT_BLOCKS: [u64; 4] = [6, 2, 9, 4];
const PARITY_BLOCKS: u64 = 9;
const LOG_SECTORS: u64 = 24;

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
    fn fill(&mut self, target: &mut [u8]) {
        for byte in target.iter_mut() {
            *byte = (self.next_u64() & 0xFF) as u8;
        }
    }
}

fn superblock(role: Role, slot_index: u16, payload_size: u64, seed: u8) -> Superblock {
    let mut superblock = Superblock::new(
        Uuid::from_random_bytes([0xA5; 16]),
        Uuid::from_random_bytes([seed; 16]),
        role,
        DATA_SLOTS,
        payload_size,
    );
    superblock.parity_block_size_log2 = BLOCK_LOG2;
    superblock.payload_offset = DEFAULT_PAYLOAD_OFFSET;
    superblock.slot_index = slot_index;
    superblock.generation = GENERATION;
    superblock.label = "probe".to_string();
    superblock
}

/// Die Members in absichtlich verwuerfelter Reihenfolge — so, wie sie beim
/// Scannen der Geraete anfallen wuerden. An keiner Stelle ist die Position im
/// Vektor gleich dem `slot_index`.
fn build_array() -> Vec<MemoryMember> {
    let members = [
        superblock(Role::ParityQ, 0, PARITY_BLOCKS * BLOCK, 0x51),
        superblock(Role::Data, 2, SLOT_BLOCKS[2] * BLOCK, 0x12),
        superblock(Role::Log, 0, LOG_SECTORS * 4096, 0x60),
        superblock(Role::Data, 0, SLOT_BLOCKS[0] * BLOCK, 0x10),
        superblock(Role::ParityP, 0, PARITY_BLOCKS * BLOCK, 0x50),
        superblock(Role::Data, 3, SLOT_BLOCKS[3] * BLOCK, 0x13),
        superblock(Role::Data, 1, SLOT_BLOCKS[1] * BLOCK, 0x11),
    ];
    members
        .into_iter()
        .map(|sb| MemoryMember::new(sb).expect("Member muss anlegbar sein"))
        .collect()
}

/// Liest die Superbloecke so zurueck, wie eine Implementierung es taete, und
/// setzt daraus das Array zusammen.
fn layout_of(members: &[MemoryMember]) -> (Vec<Superblock>, ArrayLayout) {
    let superblocks: Vec<Superblock> = members
        .iter()
        .map(|m| m.read_superblock().expect("Superblock muss lesbar sein"))
        .collect();
    let layout = assemble(&superblocks).expect("Array muss zusammengehen");
    (superblocks, layout)
}

/// Payload-Kopien aller Data-Slots, nach `slot_index` geordnet.
fn data_payloads(members: &[MemoryMember], layout: &ArrayLayout) -> Vec<Vec<u8>> {
    (0..layout.data_slot_count() as u16)
        .map(|slot| {
            let position = layout.data_position(slot).expect("Slot muss belegt sein");
            members[position].payload().to_vec()
        })
        .collect()
}

/// Baut die `Slot`-Liste fuer `parity`. Der Index kommt aus dem Superblock,
/// nicht aus der Position — das ist der Punkt der ganzen Uebung.
fn slots_of<'a>(payloads: &'a [Vec<u8>], skip: &[u8]) -> Vec<Slot<'a>> {
    payloads
        .iter()
        .enumerate()
        .filter(|(index, _)| !skip.contains(&(*index as u8)))
        .map(|(index, bytes)| Slot::new(index as u8, bytes).expect("Index unter 64"))
        .collect()
}

/// Rechnet P und Q ueber alle Data-Slots und legt sie in die Parity-Members.
fn recompute_parity(members: &mut [MemoryMember], layout: &ArrayLayout) {
    let payloads = data_payloads(members, layout);
    let slots = slots_of(&payloads, &[]);
    let count = layout.data_slot_count() as u8;

    let parity_len = members[layout.parity_p_position()].payload().len();
    let mut p = vec![0u8; parity_len];
    compute_p(count, &slots, &mut p).expect("P muss rechenbar sein");

    let mut q = vec![0u8; parity_len];
    compute_q(count, &slots, &mut q).expect("Q muss rechenbar sein");

    members[layout.parity_p_position()]
        .payload_mut()
        .copy_from_slice(&p);
    let q_position = layout.parity_q_position().expect("Array hat ein Q");
    members[q_position].payload_mut().copy_from_slice(&q);
}

/// Wendet einen Write-Record an, mit den Pruefungen, die Abschnitt 5.2
/// Schritt 5 dafuer verlangt.
fn apply(
    members: &mut [MemoryMember],
    layout: &ArrayLayout,
    header: &LogRecordHeader,
    payload: &[u8],
) -> Result<(), &'static str> {
    if header.record_type != RecordType::Write {
        return Ok(());
    }
    let position = layout
        .data_position(header.slot_index)
        .ok_or("slot_index zeigt auf keinen Data-Member")?;
    let target = members[position].payload_mut();

    let start = usize::try_from(header.target_offset).map_err(|_| "target_offset zu gross")?;
    let end = start
        .checked_add(payload.len())
        .ok_or("target_offset + payload_len laeuft ueber")?;
    if end > target.len() {
        return Err("Write reicht ueber die Payload-Region des Ziel-Members hinaus");
    }
    target[start..end].copy_from_slice(payload);
    Ok(())
}

/// Schreibt eine Folge von Writes samt Checkpoint in die Log-Region.
fn fill_log(members: &mut [MemoryMember], layout: &ArrayLayout, rng: &mut Lcg) -> Vec<(u16, u64)> {
    let log_position = layout.log_position().expect("Array hat ein Log");
    let mut written = Vec::new();

    let region = members[log_position].payload_mut();
    let mut writer = LogWriter::new(region).expect("Log-Region muss gueltig sein");

    let mut checkpoint = LogRecordHeader::checkpoint(100);
    checkpoint.generation = GENERATION;
    writer.append(&checkpoint, &[]).unwrap();

    for (step, (slot, offset)) in [(2u16, 0u64), (0, 4096), (3, 8192), (1, 0), (2, 4096)]
        .into_iter()
        .enumerate()
    {
        let mut payload = vec![0u8; 512];
        rng.fill(&mut payload);
        let mut header = LogRecordHeader::write(101 + step as u64, slot, offset, &payload);
        header.generation = GENERATION;
        writer.append(&header, &payload).unwrap();
        written.push((slot, offset));
    }
    written
}

#[test]
fn the_whole_path_from_superblock_to_reconstruction() {
    let mut rng = Lcg::new(0x5EED_1234);
    let mut members = build_array();
    let (_, layout) = layout_of(&members);

    // --- Das Layout muss die Verwuerfelung aufloesen -----------------------
    assert_eq!(layout.data_slot_count(), DATA_SLOTS);
    assert_eq!(layout.parity_block_size(), BLOCK);
    for slot in 0..DATA_SLOTS as u16 {
        let position = layout.data_position(slot).unwrap();
        assert_eq!(members[position].superblock().slot_index, slot);
        assert_ne!(
            position, slot as usize,
            "Position und slot_index duerfen sich nicht decken, sonst prueft die Probe nichts"
        );
    }

    // --- Ausgangszustand: Daten und passende Paritaet ----------------------
    for slot in 0..DATA_SLOTS as u16 {
        let position = layout.data_position(slot).unwrap();
        let mut payload = vec![0u8; members[position].payload().len()];
        rng.fill(&mut payload);
        members[position].payload_mut().copy_from_slice(&payload);
    }
    recompute_parity(&mut members, &layout);

    // --- Log schreiben, zuruecklesen, anwenden -----------------------------
    let expected_targets = fill_log(&mut members, &layout, &mut rng);

    // Die Engine liest die Log-Region am Stueck ein; hier eine Kopie, damit
    // die Data-Members waehrend des Replays veraenderbar bleiben.
    let log_region = members[layout.log_position().unwrap()].payload().to_vec();
    let ring = LogRing::new(&log_region).unwrap();
    assert_eq!(ring.newest_checkpoint().unwrap().1.seq, 100);

    let mut replay = ring.replay(GENERATION);
    let mut applied = Vec::new();
    for record in replay.by_ref() {
        apply(&mut members, &layout, &record.header, record.payload)
            .expect("die selbst geschriebenen Records muessen anwendbar sein");
        applied.push((record.header.slot_index, record.header.target_offset));
    }
    assert_eq!(applied, expected_targets, "alle Writes, in Reihenfolge");
    assert_eq!(replay.accepted_count(), 5);

    // --- Paritaet nachziehen ----------------------------------------------
    recompute_parity(&mut members, &layout);
    let before = data_payloads(&members, &layout);
    let parity_p = members[layout.parity_p_position()].payload().to_vec();
    let parity_q = members[layout.parity_q_position().unwrap()]
        .payload()
        .to_vec();

    // --- Ein Data-Member faellt aus, aus P rekonstruieren ------------------
    for lost in 0..DATA_SLOTS as u8 {
        let survivors = slots_of(&before, &[lost]);
        let mut out = vec![0u8; before[lost as usize].len()];
        reconstruct_from_p(DATA_SLOTS as u8, lost, &survivors, &parity_p, &mut out).unwrap();
        assert_eq!(out, before[lost as usize], "Slot {lost} aus P");

        let mut out = vec![0u8; before[lost as usize].len()];
        reconstruct_from_q(DATA_SLOTS as u8, lost, &survivors, &parity_q, &mut out).unwrap();
        assert_eq!(out, before[lost as usize], "Slot {lost} aus Q");
    }

    // --- Zwei Data-Members faellen aus -------------------------------------
    for first in 0..DATA_SLOTS as u8 {
        for second in (first + 1)..DATA_SLOTS as u8 {
            let survivors = slots_of(&before, &[first, second]);
            let mut out_first = vec![0u8; before[first as usize].len()];
            let mut out_second = vec![0u8; before[second as usize].len()];
            reconstruct_two_from_pq(
                DATA_SLOTS as u8,
                first,
                second,
                &survivors,
                &parity_p,
                &parity_q,
                &mut out_first,
                &mut out_second,
            )
            .unwrap();
            assert_eq!(out_first, before[first as usize], "Paar ({first},{second})");
            assert_eq!(
                out_second, before[second as usize],
                "Paar ({first},{second})"
            );
        }
    }

    // --- Data-Member plus Paritaets-Member ---------------------------------
    for lost in 0..DATA_SLOTS as u8 {
        let survivors = slots_of(&before, &[lost]);

        let mut out = vec![0u8; before[lost as usize].len()];
        let mut rebuilt_p = vec![0u8; parity_p.len()];
        reconstruct_data_and_p(
            DATA_SLOTS as u8,
            lost,
            &survivors,
            &parity_q,
            &mut out,
            &mut rebuilt_p,
        )
        .unwrap();
        assert_eq!(out, before[lost as usize]);
        assert_eq!(rebuilt_p, parity_p, "P nach Verlust von Slot {lost}");

        let mut out = vec![0u8; before[lost as usize].len()];
        let mut rebuilt_q = vec![0u8; parity_q.len()];
        reconstruct_data_and_q(
            DATA_SLOTS as u8,
            lost,
            &survivors,
            &parity_p,
            &mut out,
            &mut rebuilt_q,
        )
        .unwrap();
        assert_eq!(out, before[lost as usize]);
        assert_eq!(rebuilt_q, parity_q, "Q nach Verlust von Slot {lost}");
    }
}

#[test]
fn a_replaced_disk_is_rebuilt_byte_for_byte() {
    // Der Plattentausch, durchgespielt: Slot 2 stirbt, wird gegen eine leere
    // Platte getauscht und aus P plus den ueberlebenden Data-Members wieder
    // hergestellt.
    let mut rng = Lcg::new(0x5EED_4321);
    let mut members = build_array();
    let (_, layout) = layout_of(&members);

    for slot in 0..DATA_SLOTS as u16 {
        let position = layout.data_position(slot).unwrap();
        let mut payload = vec![0u8; members[position].payload().len()];
        rng.fill(&mut payload);
        members[position].payload_mut().copy_from_slice(&payload);
    }
    recompute_parity(&mut members, &layout);

    let lost = 2u8;
    let position = layout.data_position(u16::from(lost)).unwrap();
    let original = members[position].payload().to_vec();

    members[position].wipe_payload();
    assert!(members[position].payload().iter().all(|&b| b == 0));

    // Der Superblock ueberlebt den Tausch nicht — er stuende auf der alten
    // Platte. Hier bleibt er stehen, damit das Array weiss, welchen Slot es
    // rebuilden muss. Dass dieser Zustand nirgends auf der Platte vermerkt ist,
    // ist die offene Frage zu `member_state`.
    assert_eq!(
        members[position].read_superblock().unwrap().slot_index,
        u16::from(lost)
    );

    let payloads = data_payloads(&members, &layout);
    let survivors = slots_of(&payloads, &[lost]);
    let parity_p = members[layout.parity_p_position()].payload().to_vec();

    let mut rebuilt = vec![0u8; original.len()];
    reconstruct_from_p(DATA_SLOTS as u8, lost, &survivors, &parity_p, &mut rebuilt).unwrap();
    members[position].payload_mut().copy_from_slice(&rebuilt);

    assert_eq!(members[position].payload(), original.as_slice());
}

#[test]
fn a_crash_in_the_middle_of_the_log_applies_only_the_prefix() {
    // Halber Schreibvorgang: Der dritte Record ist beschaedigt. Alles davor
    // wird angewendet, alles danach nicht — auch der vierte und fuenfte, die
    // fuer sich vollkommen intakt sind.
    let mut rng = Lcg::new(0x5EED_9999);
    let mut members = build_array();
    let (_, layout) = layout_of(&members);
    recompute_parity(&mut members, &layout);
    fill_log(&mut members, &layout, &mut rng);

    let log_position = layout.log_position().unwrap();
    let mut log_region = members[log_position].payload().to_vec();

    // Der Checkpoint belegt einen Sektor, dann folgen die Writes zu je einem.
    // Der dritte Write beginnt also im vierten Sektor; ein gekipptes Bit in
    // seinen Nutzdaten reicht.
    log_region[3 * 4096 + 64 + 7] ^= 0x40;

    let ring = LogRing::new(&log_region).unwrap();
    let mut replay = ring.replay(GENERATION);
    let seqs: Vec<u64> = replay.by_ref().map(|r| r.header.seq).collect();
    assert_eq!(seqs, vec![101, 102], "nur der intakte Anfang");
    assert!(replay.stop().is_some());
}

#[test]
fn a_write_that_reaches_past_its_member_is_refused() {
    // `target_offset` kommt ungeprueft von der Platte. Slot 1 ist der kleinste
    // Member; ein Write kurz vor seinem Ende reicht darueber hinaus.
    let members = build_array();
    let (_, layout) = layout_of(&members);
    let mut members = members;

    let slot = 1u16;
    let size = SLOT_BLOCKS[slot as usize] * BLOCK;
    let payload = vec![0xABu8; 512];
    let mut header = LogRecordHeader::write(1, slot, size - 256, &payload);
    header.generation = GENERATION;

    assert_eq!(
        apply(&mut members, &layout, &header, &payload),
        Err("Write reicht ueber die Payload-Region des Ziel-Members hinaus")
    );
}

#[test]
fn a_write_to_an_unknown_slot_is_refused() {
    let members = build_array();
    let (_, layout) = layout_of(&members);
    let mut members = members;

    let payload = vec![0u8; 64];
    let mut header = LogRecordHeader::write(1, DATA_SLOTS as u16, 0, &payload);
    header.generation = GENERATION;

    assert_eq!(
        apply(&mut members, &layout, &header, &payload),
        Err("slot_index zeigt auf keinen Data-Member")
    );
}

#[test]
fn a_torn_primary_superblock_is_survived_by_the_backup() {
    // Abschnitt 3: Beim Lesen gilt der Superblock mit gueltiger Pruefsumme.
    // Der Backup steht am Geraeteende und ist deshalb von einem torn write am
    // Anfang nicht betroffen.
    let mut members = build_array();
    members[0].flip(65_536 + 100, 0xFF);

    let superblock = members[0]
        .read_superblock()
        .expect("Backup muss einspringen");
    assert_eq!(superblock.role, Role::ParityQ);

    // Das Array laesst sich damit weiterhin zusammensetzen.
    let (_, layout) = layout_of(&members);
    assert_eq!(layout.data_slot_count(), DATA_SLOTS);
}
