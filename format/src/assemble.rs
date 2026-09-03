// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Zusammensetzen eines Arrays, `docs/FORMAT.md` Abschnitt 2.1.
//!
//! Herein kommen die bereits dekodierten Superbloecke aller Members, heraus
//! kommt entweder ein geprueftes [`ArrayLayout`] oder ein Fehler. Kein I/O —
//! wer die Bloecke von den Geraeten liest, ist die Engine.
//!
//! Warum diese Pruefungen hart abbrechen statt zu warnen: Ein Array, das mit
//! einem fehlenden Slot zusammengesetzt wird, rechnet Paritaet ueber eine
//! unvollstaendige Menge. Die sieht gueltig aus und faellt erst auf, wenn
//! jemand daraus rekonstruiert — also im Ernstfall.

use crate::error::{FormatError, Result};
use crate::superblock::{Role, Superblock, MAX_DATA_SLOTS};
use crate::uuid::Uuid;

const SLOT_CAPACITY: usize = MAX_DATA_SLOTS as usize;

/// Groesstes Array nach Abschnitt 2.1: 64 Data-Slots, ein ParityP, je hoechstens
/// ein ParityQ und ein Log.
pub const MAX_MEMBERS: usize = SLOT_CAPACITY + 3;

/// Platzhalter in der Slot-Tabelle. Kein gueltiger Index, weil ein Array nie
/// so viele Members hat.
const NO_MEMBER: u16 = u16::MAX;

/// Ein geprueftes Array.
///
/// Haelt keine Superbloecke, sondern nur Positionen in dem Slice, das
/// [`assemble`] uebergeben bekam. Damit bleibt der Typ `Copy`-gross und ohne
/// Allokation — `format` darf keine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrayLayout {
    array_uuid: Uuid,
    data_slot_count: u32,
    parity_block_size_log2: u8,
    /// Nach `slot_index` indiziert, Wert ist die Position im Member-Slice.
    data: [u16; SLOT_CAPACITY],
    parity_p: u16,
    parity_q: Option<u16>,
    log: Option<u16>,
}

impl ArrayLayout {
    pub fn array_uuid(&self) -> Uuid {
        self.array_uuid
    }

    pub fn data_slot_count(&self) -> u32 {
        self.data_slot_count
    }

    pub fn parity_block_size_log2(&self) -> u8 {
        self.parity_block_size_log2
    }

    pub fn parity_block_size(&self) -> u64 {
        1u64 << self.parity_block_size_log2
    }

    /// Position des Data-Members fuer `slot_index` im Member-Slice.
    pub fn data_position(&self, slot_index: u16) -> Option<usize> {
        let entry = *self.data.get(usize::from(slot_index))?;
        (entry != NO_MEMBER).then(|| usize::from(entry))
    }

    pub fn parity_p_position(&self) -> usize {
        usize::from(self.parity_p)
    }

    pub fn parity_q_position(&self) -> Option<usize> {
        self.parity_q.map(usize::from)
    }

    pub fn log_position(&self) -> Option<usize> {
        self.log.map(usize::from)
    }

    pub fn has_parity_q(&self) -> bool {
        self.parity_q.is_some()
    }

    pub fn has_log(&self) -> bool {
        self.log.is_some()
    }

    /// Positionen aller Data-Members, nach `slot_index` geordnet.
    ///
    /// Die Reihenfolge ist die der Slots, nicht die des uebergebenen Slices.
    /// Fuer die Q-Rechnung zaehlt der Slot-Index, nicht die Position.
    pub fn data_positions(&self) -> impl Iterator<Item = usize> + '_ {
        self.data[..self.data_slot_count as usize]
            .iter()
            .map(|&entry| usize::from(entry))
    }
}

/// Prueft die Regeln aus Abschnitt 2.1 und baut daraus das Layout.
pub fn assemble(members: &[Superblock]) -> Result<ArrayLayout> {
    // Regel 1
    if members.is_empty() {
        return Err(FormatError::NoMembers);
    }
    if members.len() > MAX_MEMBERS {
        return Err(FormatError::TooManyMembers {
            max: MAX_MEMBERS,
            got: members.len(),
        });
    }
    // Jeder Member fuer sich, bevor ueber die Menge geredet wird. `decode`
    // erledigt das schon; hier steht es fuer Superbloecke, die im Speicher
    // gebaut wurden und nie ueber die Platte gingen.
    for member in members {
        member.validate()?;
    }

    let reference = &members[0];
    let mut data = [NO_MEMBER; SLOT_CAPACITY];
    let mut parity_p: Option<u16> = None;
    let mut parity_q: Option<u16> = None;
    let mut log: Option<u16> = None;

    for (position, member) in members.iter().enumerate() {
        // Regel 2
        if member.array_uuid != reference.array_uuid {
            return Err(FormatError::MismatchedArrayParameter {
                field: "array_uuid",
                member: position,
            });
        }
        if member.parity_block_size_log2 != reference.parity_block_size_log2 {
            return Err(FormatError::MismatchedArrayParameter {
                field: "parity_block_size_log2",
                member: position,
            });
        }
        if member.data_slot_count != reference.data_slot_count {
            return Err(FormatError::MismatchedArrayParameter {
                field: "data_slot_count",
                member: position,
            });
        }

        // Regel 3
        if let Some(first) = members[..position]
            .iter()
            .position(|other| other.member_uuid == member.member_uuid)
        {
            return Err(FormatError::DuplicateMemberUuid {
                first,
                second: position,
            });
        }

        // Die Laengenpruefung oben haelt `position` unter `MAX_MEMBERS`.
        let position_u16 = position as u16;
        match member.role {
            Role::Data => {
                // `validate` hat `slot_index < data_slot_count <= 64` bereits
                // durchgesetzt, der Index ist also in der Tabelle.
                let slot = usize::from(member.slot_index);
                if data[slot] != NO_MEMBER {
                    return Err(FormatError::DuplicateDataSlot {
                        slot_index: member.slot_index,
                        first: usize::from(data[slot]),
                        second: position,
                    });
                }
                data[slot] = position_u16;
            }
            Role::ParityP => claim(&mut parity_p, Role::ParityP, position_u16)?,
            Role::ParityQ => claim(&mut parity_q, Role::ParityQ, position_u16)?,
            Role::Log => claim(&mut log, Role::Log, position_u16)?,
        }
    }

    // Regel 4
    let parity_p = parity_p.ok_or(FormatError::MissingParityP)?;

    // Regel 5. Doppelte Slots sind oben ausgeschlossen und `slot_index` ist
    // immer kleiner als `data_slot_count` — bleibt zu pruefen, dass keiner
    // fehlt.
    for slot_index in 0..reference.data_slot_count {
        if data[slot_index as usize] == NO_MEMBER {
            return Err(FormatError::MissingDataSlot {
                slot_index: slot_index as u16,
            });
        }
    }

    // Regel 6
    let longest = longest_data(members, &data, reference.data_slot_count);
    check_parity_covers(members, parity_p, Role::ParityP, longest)?;
    if let Some(position) = parity_q {
        check_parity_covers(members, position, Role::ParityQ, longest)?;
    }

    Ok(ArrayLayout {
        array_uuid: reference.array_uuid,
        data_slot_count: reference.data_slot_count,
        parity_block_size_log2: reference.parity_block_size_log2,
        data,
        parity_p,
        parity_q,
        log,
    })
}

fn claim(place: &mut Option<u16>, role: Role, position: u16) -> Result<()> {
    match *place {
        Some(first) => Err(FormatError::DuplicateRole {
            role,
            first: usize::from(first),
            second: usize::from(position),
        }),
        None => {
            *place = Some(position);
            Ok(())
        }
    }
}

/// Laengster Data-Member als `(payload_size, slot_index)`.
fn longest_data(members: &[Superblock], data: &[u16; SLOT_CAPACITY], count: u32) -> (u64, u16) {
    let mut longest = 0u64;
    let mut slot_index = 0u16;
    for slot in 0..count as usize {
        let member = &members[usize::from(data[slot])];
        if member.payload_size > longest {
            longest = member.payload_size;
            slot_index = slot as u16;
        }
    }
    (longest, slot_index)
}

fn check_parity_covers(
    members: &[Superblock],
    position: u16,
    role: Role,
    longest: (u64, u16),
) -> Result<()> {
    let parity = &members[usize::from(position)];
    let (data_size, slot_index) = longest;
    if parity.payload_size < data_size {
        return Err(FormatError::ParityTooShort {
            role,
            parity_size: parity.payload_size,
            data_size,
            slot_index,
        });
    }
    Ok(())
}
