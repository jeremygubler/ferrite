// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Ein Array anlegen und wieder oeffnen, `docs/FORMAT.md` Abschnitte 2.1 und 3.
//!
//! Das ist die erste Stelle, an der mehrere Geraete zusammen betrachtet werden.
//! Sie tut zwei Dinge und sonst nichts: Superbloecke schreiben, die den Regeln
//! aus Abschnitt 2.1 genuegen, und sie wieder einlesen, um `assemble` daraus
//! ein geprueftes Layout bilden zu lassen.
//!
//! Keine Uhrzeit aus der Umgebung. `created_unix` kommt als Parameter herein —
//! aus demselben Grund, aus dem `parity` seinen Zufall geliefert bekommt: Ein
//! Wert, der von der Systemuhr abhaengt, macht jeden Test von ihr abhaengig.

use ferrite_format::assemble::{assemble, ArrayLayout};
use ferrite_format::superblock::{
    MemberState, Role, Superblock, DEFAULT_PAYLOAD_OFFSET, SUPERBLOCK_BACKUP_FROM_END,
};
use ferrite_format::{FormatError, Uuid};

use crate::device::{read_superblock, write_superblock, MemberDevice};
use crate::error::{EngineError, Result};

/// Was ein einzelnes Geraet im Array werden soll.
///
/// Die Groesse steht hier nicht: Die bringt das Geraet mit, und sie darf sich
/// von der jedes anderen Members unterscheiden. Genau das ist der Punkt des
/// Projekts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberSpec {
    pub member_uuid: Uuid,
    pub role: Role,
    /// Nur bei [`Role::Data`] von Bedeutung, sonst null.
    pub slot_index: u16,
    pub label: String,
}

/// Was fuer alle Members gleich ist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArraySpec {
    pub array_uuid: Uuid,
    pub parity_block_size_log2: u8,
    pub created_unix: u64,
}

/// Groesste Payload, die auf ein Geraet dieser Groesse passt.
///
/// Abgerundet auf ganze Parity-Bloecke: Ein angebrochener letzter Block haette
/// auf dem Parity-Member kein Gegenstueck voller Breite, und die Kerninvariante
/// rechnet ueber Bloecke, nicht ueber Bytes.
pub fn max_payload_size(device_size: u64, parity_block_size_log2: u8) -> Result<u64> {
    let reserved = DEFAULT_PAYLOAD_OFFSET + SUPERBLOCK_BACKUP_FROM_END;
    let usable = device_size
        .checked_sub(reserved)
        .filter(|usable| *usable > 0)
        .ok_or(EngineError::Format(FormatError::InvalidField {
            field: "device_size",
            reason: "kein Platz fuer eine Payload-Region",
        }))?;

    let payload_size = (usable >> parity_block_size_log2) << parity_block_size_log2;
    if payload_size == 0 {
        return Err(EngineError::Format(FormatError::InvalidField {
            field: "device_size",
            reason: "nicht einmal ein ganzer Parity-Block passt",
        }));
    }
    Ok(payload_size)
}

/// Legt ein Array an: schreibt auf jedes Geraet seinen Superblock.
///
/// Vor dem ersten Schreibvorgang wird das ganze Vorhaben geprueft — die
/// Superbloecke werden gebaut und durch `assemble` geschickt. Erst wenn das
/// Layout haelt, geht etwas auf eine Platte. Sonst stuende nach einer
/// abgelehnten Zusammenstellung die Haelfte davon auf echten Geraeten und
/// muesste von Hand aufgeraeumt werden.
///
/// Rueckgabe sind die geschriebenen Superbloecke in derselben Reihenfolge wie
/// `devices`.
pub fn create_array(
    devices: &[MemberDevice],
    specs: &[MemberSpec],
    array: &ArraySpec,
) -> Result<Vec<Superblock>> {
    if devices.len() != specs.len() {
        return Err(EngineError::Format(FormatError::InvalidField {
            field: "members",
            reason: "zu jedem Geraet gehoert genau eine Rollenzuweisung",
        }));
    }

    let data_slot_count = specs
        .iter()
        .filter(|spec| spec.role == Role::Data)
        .count()
        .try_into()
        .map_err(|_| {
            EngineError::Format(FormatError::InvalidField {
                field: "data_slot_count",
                reason: "mehr Data-Members als der Typ fasst",
            })
        })?;

    let mut superblocks = Vec::with_capacity(specs.len());
    for (device, spec) in devices.iter().zip(specs) {
        if !device.is_writable() {
            return Err(EngineError::NotWritable);
        }
        let payload_size = max_payload_size(device.size(), array.parity_block_size_log2)?;

        let mut superblock = Superblock::new(
            array.array_uuid,
            spec.member_uuid,
            spec.role,
            data_slot_count,
            payload_size,
        );
        superblock.parity_block_size_log2 = array.parity_block_size_log2;
        superblock.slot_index = spec.slot_index;
        superblock.created_unix = array.created_unix;
        superblock.label.clone_from(&spec.label);
        superblock.member_state = MemberState::Clean;

        superblock
            .fits_on_device(device.size())
            .map_err(EngineError::Format)?;
        superblocks.push(superblock);
    }

    // Hier faellt unter anderem Regel 6 auf: ein Parity-Member, der kleiner ist
    // als der groesste Data-Member. Besser jetzt als nach dem Schreiben.
    assemble(&superblocks).map_err(EngineError::Format)?;

    for (device, superblock) in devices.iter().zip(&superblocks) {
        write_superblock(device, superblock)?;
    }
    Ok(superblocks)
}

/// Ein geoeffnetes Array: die gelesenen Superbloecke und das Layout dazu.
///
/// Das Layout haelt Positionen in `superblocks`, nicht die Superbloecke selbst
/// — deshalb liegen beide zusammen in einem Typ und nicht getrennt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenArray {
    superblocks: Vec<Superblock>,
    layout: ArrayLayout,
}

impl OpenArray {
    pub fn layout(&self) -> &ArrayLayout {
        &self.layout
    }

    pub fn superblocks(&self) -> &[Superblock] {
        &self.superblocks
    }

    /// Superblock des Data-Members in diesem Slot, falls er dabei ist.
    pub fn data_member(&self, slot_index: u16) -> Option<&Superblock> {
        self.layout
            .data_position(slot_index)
            .map(|position| &self.superblocks[position])
    }

    pub fn parity_p(&self) -> &Superblock {
        &self.superblocks[self.layout.parity_p_position()]
    }

    pub fn parity_q(&self) -> Option<&Superblock> {
        self.layout
            .parity_q_position()
            .map(|position| &self.superblocks[position])
    }

    pub fn log(&self) -> Option<&Superblock> {
        self.layout
            .log_position()
            .map(|position| &self.superblocks[position])
    }
}

/// Liest die Superbloecke aller Geraete und setzt daraus ein Array zusammen.
///
/// Die Reihenfolge von `devices` bleibt die Reihenfolge der Superbloecke, und
/// das Layout indiziert in eben diese Reihenfolge. Wer `devices[i]` haelt, hat
/// damit auch `superblocks()[i]`.
pub fn open_array(devices: &[MemberDevice]) -> Result<OpenArray> {
    let mut superblocks = Vec::with_capacity(devices.len());
    for device in devices {
        superblocks.push(read_superblock(device)?);
    }
    let layout = assemble(&superblocks).map_err(EngineError::Format)?;
    Ok(OpenArray {
        superblocks,
        layout,
    })
}
