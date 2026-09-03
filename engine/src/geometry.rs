//! Rechnen in Parity-Blöcken, `docs/FORMAT.md` Abschnitt 2.
//!
//! Ferrite ist kein Striping-Layout: Parity-Block `i` deckt bei **jedem**
//! Member dieselben Byte-Offsets ab. Deshalb haengt die Frage, welche Bloecke
//! ein Write dreckig macht, nur am Offset und nicht daran, welcher Slot
//! geschrieben wurde.

use core::ops::Range;

use ferrite_format::assemble::ArrayLayout;
use ferrite_format::log::LogRecordHeader;
use ferrite_format::superblock::Superblock;

use crate::error::{EngineError, Result};

/// Die Einteilung eines Arrays in Parity-Blöcke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockGeometry {
    block_size_log2: u8,
    block_count: u64,
}

impl BlockGeometry {
    /// Leitet die Geometrie aus dem ParityP-Member ab.
    ///
    /// Er ist nach Regel 6 aus Abschnitt 2.1 mindestens so lang wie jeder
    /// Data-Member und gibt damit die Zahl der Bloecke vor, ueber die das Array
    /// Paritaet fuehrt.
    pub fn of(layout: &ArrayLayout, members: &[Superblock]) -> Result<Self> {
        let parity = members
            .get(layout.parity_p_position())
            .ok_or(EngineError::Format(
                ferrite_format::FormatError::MissingParityP,
            ))?;
        let block_size_log2 = layout.parity_block_size_log2();
        Ok(BlockGeometry {
            block_size_log2,
            block_count: parity.payload_size >> block_size_log2,
        })
    }

    pub fn new(block_size_log2: u8, block_count: u64) -> Self {
        BlockGeometry {
            block_size_log2,
            block_count,
        }
    }

    pub fn block_size_log2(&self) -> u8 {
        self.block_size_log2
    }

    pub fn block_size(&self) -> u64 {
        1u64 << self.block_size_log2
    }

    pub fn block_count(&self) -> u64 {
        self.block_count
    }

    /// Der Block, in dem dieses Byte liegt.
    pub fn block_of(&self, offset: u64) -> u64 {
        offset >> self.block_size_log2
    }

    /// Byte-Bereich eines Blocks innerhalb der Payload-Region.
    pub fn byte_range(&self, block: u64) -> Range<u64> {
        let start = block << self.block_size_log2;
        start..start + self.block_size()
    }

    /// Die Bloecke, die ein Bereich `offset..offset + len` beruehrt.
    ///
    /// Eine Laenge von null beruehrt nichts — nicht etwa den Block, in dem der
    /// Offset liegt.
    pub fn blocks_touched(&self, offset: u64, len: u64) -> Result<Range<u64>> {
        if len == 0 {
            let block = self.block_of(offset);
            return Ok(block..block);
        }
        let end = offset
            .checked_add(len)
            .ok_or(EngineError::OffsetOverflow { offset, len })?;
        let first = self.block_of(offset);
        // `end` ist hier groesser als null, das letzte beruehrte Byte ist
        // `end - 1`.
        let last = self.block_of(end - 1);
        if last >= self.block_count {
            return Err(EngineError::BeyondArray {
                block: last,
                block_count: self.block_count,
            });
        }
        Ok(first..last + 1)
    }
}

/// Ein Write, so wie ihn der Replay liefert — reduziert auf das, was fuer die
/// Paritaet zaehlt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteTarget {
    pub slot_index: u16,
    pub offset: u64,
    pub len: u64,
}

impl WriteTarget {
    pub fn from_record(header: &LogRecordHeader) -> Self {
        WriteTarget {
            slot_index: header.slot_index,
            offset: header.target_offset,
            len: u64::from(header.payload_len),
        }
    }
}

/// Die Parity-Bloecke, die dieser Stapel Writes neu berechnet werden muessen.
///
/// Zusammenhaengende und ueberlappende Bereiche werden verschmolzen, das
/// Ergebnis ist aufsteigend sortiert und ueberschneidungsfrei. Wer hier einen
/// Block auslaesst, laesst die Paritaet dieses Blocks veraltet zurueck — sie
/// sieht gueltig aus, und der Fehler faellt erst auf, wenn jemand daraus
/// rekonstruiert.
pub fn dirty_blocks(geometry: &BlockGeometry, writes: &[WriteTarget]) -> Result<Vec<Range<u64>>> {
    let mut ranges = Vec::with_capacity(writes.len());
    for write in writes {
        let range = geometry.blocks_touched(write.offset, write.len)?;
        if !range.is_empty() {
            ranges.push(range);
        }
    }
    ranges.sort_by_key(|range| range.start);

    let mut merged: Vec<Range<u64>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            // `<=` statt `<`: Zwei aneinandergrenzende Bereiche werden
            // ebenfalls zusammengefasst, sie werden ohnehin am Stueck gelesen.
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    Ok(merged)
}

/// Wie viele Bloecke eine Liste von Bereichen umfasst.
pub fn total_blocks(ranges: &[Range<u64>]) -> u64 {
    ranges.iter().map(|range| range.end - range.start).sum()
}
