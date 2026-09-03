// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Generalprobe fuer `format` und `parity`, vollstaendig im Speicher.
//!
//! Ein Member ist hier ein `Vec<u8>` mit genau dem Layout, das nach Abschnitt 3
//! des Formatdokuments auf der Platte staende: reservierter Anfang, primaerer
//! Superblock, Payload-Region, Backup-Superblock am Ende.
//!
//! **Das ist kein Produktionscode und wird keiner.** Der Zweck ist, die Naht
//! zwischen den beiden reinen Crates einmal durchzuspielen, bevor `engine/`
//! sie mit echten Blockgeraeten nachbaut. Genau dort liegt die Fehlerklasse,
//! gegen die die `parity`-API gebaut ist: Position im Slice statt `slot_index`.
//!
//! Kein I/O, keine Konfiguration, keine Uhrzeit — dieselben Regeln wie in den
//! Crates darunter, damit die Probe reproduzierbar bleibt.

use ferrite_format::superblock::{
    Superblock, SUPERBLOCK_BACKUP_FROM_END, SUPERBLOCK_PRIMARY_OFFSET, SUPERBLOCK_SIZE,
};
use ferrite_format::{FormatError, Result};

/// Ein Member im Speicher, Byte fuer Byte wie auf der Platte.
#[derive(Debug, Clone)]
pub struct MemoryMember {
    bytes: Vec<u8>,
    superblock: Superblock,
}

impl MemoryMember {
    /// Legt einen Member an, der genau gross genug fuer seine Payload ist.
    ///
    /// Die Geraetegroesse ergibt sich aus `payload_offset + payload_size` plus
    /// dem reservierten Bereich am Ende — also der kleinste Wert, den
    /// [`Superblock::fits_on_device`] noch durchlaesst.
    pub fn new(superblock: Superblock) -> Result<Self> {
        let end = superblock.payload_end().ok_or(FormatError::InvalidField {
            field: "payload_size",
            reason: "payload_offset + payload_size laeuft ueber",
        })?;
        let device_size =
            end.checked_add(SUPERBLOCK_BACKUP_FROM_END)
                .ok_or(FormatError::InvalidField {
                    field: "payload_size",
                    reason: "Geraetegroesse laeuft ueber",
                })?;
        superblock.fits_on_device(device_size)?;

        let mut member = MemoryMember {
            bytes: vec![0u8; device_size as usize],
            superblock,
        };
        member.write_superblocks()?;
        Ok(member)
    }

    /// Schreibt beide Superbloecke, primaer zuerst (Abschnitt 3).
    pub fn write_superblocks(&mut self) -> Result<()> {
        let encoded = self.superblock.encode()?;
        let primary = SUPERBLOCK_PRIMARY_OFFSET as usize;
        self.bytes[primary..primary + SUPERBLOCK_SIZE].copy_from_slice(&encoded);
        let backup = self.backup_offset();
        self.bytes[backup..backup + SUPERBLOCK_SIZE].copy_from_slice(&encoded);
        Ok(())
    }

    /// Liest den Superblock so, wie eine Implementierung es taete: beide
    /// Kopien, der mit gueltiger Pruefsumme und hoeherer `generation` gewinnt.
    pub fn read_superblock(&self) -> Result<Superblock> {
        let primary = SUPERBLOCK_PRIMARY_OFFSET as usize;
        let backup = self.backup_offset();
        Superblock::select(
            &self.bytes[primary..primary + SUPERBLOCK_SIZE],
            &self.bytes[backup..backup + SUPERBLOCK_SIZE],
        )
    }

    pub fn superblock(&self) -> &Superblock {
        &self.superblock
    }

    /// Zugriff zum Aendern von Zustandsfeldern wie `member_state`.
    ///
    /// Wer hier `payload_offset` oder `payload_size` verstellt, bringt den
    /// Superblock gegen die bereits angelegten Bytes aus dem Tritt — die
    /// Geraetegroesse steht schon fest. Fuer alles andere ist der Weg gedacht,
    /// gefolgt von [`MemoryMember::write_superblocks`].
    pub fn superblock_mut(&mut self) -> &mut Superblock {
        &mut self.superblock
    }

    pub fn device_size(&self) -> u64 {
        self.bytes.len() as u64
    }

    pub fn payload(&self) -> &[u8] {
        let start = self.superblock.payload_offset as usize;
        let len = self.superblock.payload_size as usize;
        &self.bytes[start..start + len]
    }

    pub fn payload_mut(&mut self) -> &mut [u8] {
        let start = self.superblock.payload_offset as usize;
        let len = self.superblock.payload_size as usize;
        &mut self.bytes[start..start + len]
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Totalausfall: Der Member wird gegen eine leere Platte getauscht. Der
    /// Superblock bleibt, damit das Array weiss, welchen Slot es rekonstruieren
    /// muss — der Zustand selbst steht nirgends, siehe die offene Frage zu
    /// `member_state`.
    pub fn wipe_payload(&mut self) {
        self.payload_mut().fill(0);
    }

    /// Kippt ein Byte. Fuer Tests, die Bit-Rot nachstellen.
    pub fn flip(&mut self, offset: usize, mask: u8) {
        self.bytes[offset] ^= mask;
    }

    fn backup_offset(&self) -> usize {
        self.bytes.len() - SUPERBLOCK_BACKUP_FROM_END as usize
    }
}
