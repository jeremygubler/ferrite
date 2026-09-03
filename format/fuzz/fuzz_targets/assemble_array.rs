//! `assemble` gegen beliebige Member-Mengen, `docs/FORMAT.md` Abschnitt 2.1.
//!
//! Invariante: Was `assemble` durchlaesst, ist ein Array, aus dem sich
//! rekonstruieren laesst. Konkret muessen die Slots `0..data_slot_count` genau
//! einmal belegt sein, die Positionen paarweise verschieden, und die Paritaet
//! mindestens so lang wie der laengste Data-Member. Kommt hier ein Layout
//! heraus, das eine dieser Zusagen bricht, rechnet die Engine spaeter Paritaet
//! ueber eine unvollstaendige Menge.
//!
//! Der Aufbau ist absichtlich zweistufig: Erst werden arrayweite Parameter
//! gezogen, dann duerfen einzelne Members davon abweichen. Ohne den gemeinsamen
//! Teil scheiterte fast jede Eingabe schon an Regel 2, und die interessanten
//! Pruefungen dahinter blieben ungetestet.

#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use ferrite_format::assemble::{assemble, MAX_MEMBERS};
use ferrite_format::superblock::{MemberState, Role, Superblock, MAX_DATA_SLOTS};
use ferrite_format::Uuid;
use libfuzzer_sys::fuzz_target;

const MIN_PAYLOAD_OFFSET: u64 = 17 * 4096;

fn build(u: &mut Unstructured) -> arbitrary::Result<Vec<Superblock>> {
    // Arrayweite Parameter, an die sich die meisten Members halten.
    let array_uuid = Uuid::from_random_bytes(<[u8; 16]>::arbitrary(u)?);
    let block_log2 = 12 + u8::arbitrary(u)? % 13;
    let slot_count = 1 + u32::arbitrary(u)? % MAX_DATA_SLOTS;
    let parity_blocks = 1 + u64::arbitrary(u)? % 64;

    let count = usize::from(u8::arbitrary(u)?) % (MAX_MEMBERS + 2);
    let mut members = Vec::with_capacity(count);

    for index in 0..count {
        if u.is_empty() {
            break;
        }
        let knobs = u8::arbitrary(u)?;
        let role = match knobs % 4 {
            0 => Role::ParityP,
            1 => Role::ParityQ,
            2 => Role::Log,
            _ => Role::Data,
        };

        let blocks = 1 + u64::arbitrary(u)? % parity_blocks.max(1);
        let payload_size = if role == Role::Log {
            (u64::arbitrary(u)? % 64) * 4096
        } else if role == Role::Data {
            blocks << block_log2
        } else {
            parity_blocks << block_log2
        };

        let mut member = Superblock::new(
            // Ein Member aus einem fremden Array — selten, aber es muss
            // auffallen.
            if knobs & 0x40 != 0 {
                Uuid::from_random_bytes(<[u8; 16]>::arbitrary(u)?)
            } else {
                array_uuid
            },
            Uuid::from_random_bytes(<[u8; 16]>::arbitrary(u)?),
            role,
            slot_count,
            payload_size,
        );
        member.parity_block_size_log2 = block_log2;
        member.payload_offset = MIN_PAYLOAD_OFFSET;
        member.slot_index = if role == Role::Data {
            // Meist der Reihe nach, damit vollstaendige Arrays entstehen.
            if knobs & 0x80 != 0 {
                (u32::arbitrary(u)? % slot_count) as u16
            } else {
                (index as u32 % slot_count) as u16
            }
        } else {
            u16::arbitrary(u)?
        };
        if role != Role::Log && knobs & 0x20 != 0 {
            member.member_state = MemberState::Stale;
        }
        members.push(member);
    }

    Ok(members)
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let Ok(members) = build(&mut u) else {
        return;
    };

    let Ok(layout) = assemble(&members) else {
        return;
    };

    let count = layout.data_slot_count();
    assert_eq!(count, members[0].data_slot_count);
    assert!((1..=MAX_DATA_SLOTS).contains(&count));

    // Regel 5: Jeder Slot genau einmal, und zwar auf einen Data-Member.
    let mut seen = Vec::with_capacity(count as usize);
    for slot in 0..count as u16 {
        let position = layout
            .data_position(slot)
            .expect("jeder Slot unter data_slot_count muss belegt sein");
        let member = &members[position];
        assert_eq!(member.role, Role::Data);
        assert_eq!(member.slot_index, slot);
        assert!(
            !seen.contains(&position),
            "zwei Slots zeigen auf denselben Member"
        );
        seen.push(position);
    }
    assert_eq!(layout.data_position(count as u16), None);
    assert_eq!(layout.data_positions().count(), count as usize);

    // Regel 4: genau ein ParityP, hoechstens je ein ParityQ und Log, und keine
    // Position doppelt vergeben.
    let parity_p = layout.parity_p_position();
    assert_eq!(members[parity_p].role, Role::ParityP);
    assert!(!seen.contains(&parity_p));

    if let Some(position) = layout.parity_q_position() {
        assert_eq!(members[position].role, Role::ParityQ);
        assert!(!seen.contains(&position));
        assert_ne!(position, parity_p);
    }
    if let Some(position) = layout.log_position() {
        assert_eq!(members[position].role, Role::Log);
        assert!(!seen.contains(&position));
        assert_ne!(position, parity_p);
        assert_ne!(Some(position), layout.parity_q_position());
    }

    // Regel 2 und 3.
    let reference = &members[0];
    for member in &members {
        assert_eq!(member.array_uuid, reference.array_uuid);
        assert_eq!(
            member.parity_block_size_log2,
            layout.parity_block_size_log2()
        );
        assert_eq!(member.data_slot_count, count);
    }
    for (index, member) in members.iter().enumerate() {
        for other in &members[..index] {
            assert_ne!(member.member_uuid, other.member_uuid);
        }
    }

    // Regel 6: Die Paritaet deckt den laengsten Data-Member ab. Faellt sie
    // kuerzer aus, endet die Redundanz still mitten im Array.
    let longest = layout
        .data_positions()
        .map(|position| members[position].payload_size)
        .max()
        .unwrap_or(0);
    assert!(members[parity_p].payload_size >= longest);
    if let Some(position) = layout.parity_q_position() {
        assert!(members[position].payload_size >= longest);
    }
});
