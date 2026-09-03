//! Write-Log, `docs/FORMAT.md` Abschnitt 5.

pub mod ring;

use crate::crc32c::crc32c;
use crate::error::{FormatError, Result};

pub const LOG_MAGIC: &[u8; 4] = b"FLOG";
pub const LOG_HEADER_SIZE: usize = 64;
pub const LOG_SECTOR_SIZE: usize = 4096;

const OFF_MAGIC: usize = 0;
const OFF_RECORD_TYPE: usize = 4;
const OFF_HEADER_SIZE: usize = 6;
const OFF_SEQ: usize = 8;
const OFF_TARGET_OFFSET: usize = 16;
const OFF_PAYLOAD_LEN: usize = 24;
const OFF_SLOT_INDEX: usize = 28;
const OFF_GENERATION: usize = 32;
const OFF_COMMIT_UNIX: usize = 40;
const OFF_PAYLOAD_CRC: usize = 48;
const OFF_HEADER_CRC: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum RecordType {
    Write = 1,
    Checkpoint = 2,
    Padding = 4,
}

impl RecordType {
    pub fn from_u16(value: u16) -> Result<Self> {
        match value {
            1 => Ok(RecordType::Write),
            2 => Ok(RecordType::Checkpoint),
            4 => Ok(RecordType::Padding),
            other => Err(FormatError::UnknownRecordType(other)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecordHeader {
    pub record_type: RecordType,
    pub seq: u64,
    /// Offset relativ zum Beginn der Payload-Region des Ziel-Members.
    pub target_offset: u64,
    pub payload_len: u32,
    pub slot_index: u16,
    pub generation: u64,
    pub commit_unix: u64,
    pub payload_crc32c: u32,
}

impl LogRecordHeader {
    pub fn write(seq: u64, slot_index: u16, target_offset: u64, payload: &[u8]) -> Self {
        LogRecordHeader {
            record_type: RecordType::Write,
            seq,
            target_offset,
            payload_len: payload.len() as u32,
            slot_index,
            generation: 0,
            commit_unix: 0,
            payload_crc32c: crc32c(payload),
        }
    }

    pub fn checkpoint(seq: u64) -> Self {
        LogRecordHeader {
            record_type: RecordType::Checkpoint,
            seq,
            target_offset: 0,
            payload_len: 0,
            slot_index: 0,
            generation: 0,
            commit_unix: 0,
            payload_crc32c: crc32c(&[]),
        }
    }

    /// Fuellt den Rest des Ringpuffers, wenn ein Record nicht mehr davor passt.
    pub fn padding(seq: u64, skipped_bytes: u32) -> Self {
        LogRecordHeader {
            record_type: RecordType::Padding,
            seq,
            target_offset: 0,
            payload_len: skipped_bytes,
            slot_index: 0,
            generation: 0,
            commit_unix: 0,
            payload_crc32c: 0,
        }
    }

    /// Gesamtgroesse dieses Records auf der Platte, auf 4096 aufgerundet.
    pub fn on_disk_len(&self) -> usize {
        round_up(LOG_HEADER_SIZE + self.payload_len as usize, LOG_SECTOR_SIZE)
    }

    pub fn encode(&self) -> [u8; LOG_HEADER_SIZE] {
        let mut buffer = [0u8; LOG_HEADER_SIZE];
        buffer[OFF_MAGIC..OFF_MAGIC + 4].copy_from_slice(LOG_MAGIC);
        buffer[OFF_RECORD_TYPE..OFF_RECORD_TYPE + 2]
            .copy_from_slice(&(self.record_type as u16).to_le_bytes());
        buffer[OFF_HEADER_SIZE..OFF_HEADER_SIZE + 2]
            .copy_from_slice(&(LOG_HEADER_SIZE as u16).to_le_bytes());
        buffer[OFF_SEQ..OFF_SEQ + 8].copy_from_slice(&self.seq.to_le_bytes());
        buffer[OFF_TARGET_OFFSET..OFF_TARGET_OFFSET + 8]
            .copy_from_slice(&self.target_offset.to_le_bytes());
        buffer[OFF_PAYLOAD_LEN..OFF_PAYLOAD_LEN + 4]
            .copy_from_slice(&self.payload_len.to_le_bytes());
        buffer[OFF_SLOT_INDEX..OFF_SLOT_INDEX + 2].copy_from_slice(&self.slot_index.to_le_bytes());
        buffer[OFF_GENERATION..OFF_GENERATION + 8].copy_from_slice(&self.generation.to_le_bytes());
        buffer[OFF_COMMIT_UNIX..OFF_COMMIT_UNIX + 8]
            .copy_from_slice(&self.commit_unix.to_le_bytes());
        buffer[OFF_PAYLOAD_CRC..OFF_PAYLOAD_CRC + 4]
            .copy_from_slice(&self.payload_crc32c.to_le_bytes());

        let checksum = crc32c(&buffer[..OFF_HEADER_CRC]);
        buffer[OFF_HEADER_CRC..OFF_HEADER_CRC + 4].copy_from_slice(&checksum.to_le_bytes());
        buffer
    }

    pub fn decode(buffer: &[u8]) -> Result<Self> {
        if buffer.len() < LOG_HEADER_SIZE {
            return Err(FormatError::BufferTooSmall {
                need: LOG_HEADER_SIZE,
                got: buffer.len(),
            });
        }
        let block = &buffer[..LOG_HEADER_SIZE];

        if &block[OFF_MAGIC..OFF_MAGIC + 4] != LOG_MAGIC {
            let mut found = [0u8; 8];
            found[..4].copy_from_slice(&block[OFF_MAGIC..OFF_MAGIC + 4]);
            return Err(FormatError::BadMagic {
                expected: b"FLOG\0\0\0\0",
                found,
            });
        }

        let stored = u32::from_le_bytes([
            block[OFF_HEADER_CRC],
            block[OFF_HEADER_CRC + 1],
            block[OFF_HEADER_CRC + 2],
            block[OFF_HEADER_CRC + 3],
        ]);
        let computed = crc32c(&block[..OFF_HEADER_CRC]);
        if stored != computed {
            return Err(FormatError::ChecksumMismatch {
                expected: stored,
                computed,
            });
        }

        let header_size = u16::from_le_bytes([block[OFF_HEADER_SIZE], block[OFF_HEADER_SIZE + 1]]);
        if header_size as usize != LOG_HEADER_SIZE {
            return Err(FormatError::BadHeaderSize {
                expected: LOG_HEADER_SIZE as u32,
                found: header_size as u32,
            });
        }

        Ok(LogRecordHeader {
            record_type: RecordType::from_u16(u16::from_le_bytes([
                block[OFF_RECORD_TYPE],
                block[OFF_RECORD_TYPE + 1],
            ]))?,
            seq: read_u64(block, OFF_SEQ),
            target_offset: read_u64(block, OFF_TARGET_OFFSET),
            payload_len: read_u32(block, OFF_PAYLOAD_LEN),
            slot_index: u16::from_le_bytes([block[OFF_SLOT_INDEX], block[OFF_SLOT_INDEX + 1]]),
            generation: read_u64(block, OFF_GENERATION),
            commit_unix: read_u64(block, OFF_COMMIT_UNIX),
            payload_crc32c: read_u32(block, OFF_PAYLOAD_CRC),
        })
    }

    /// Prueft die Nutzdaten gegen die im Header gespeicherte Pruefsumme.
    pub fn verify_payload(&self, payload: &[u8]) -> Result<()> {
        if payload.len() != self.payload_len as usize {
            return Err(FormatError::InvalidField {
                field: "payload_len",
                reason: "Laenge passt nicht zum Header",
            });
        }
        let computed = crc32c(payload);
        if computed != self.payload_crc32c {
            return Err(FormatError::ChecksumMismatch {
                expected: self.payload_crc32c,
                computed,
            });
        }
        Ok(())
    }
}

/// Setzt die Akzeptanzregel aus Abschnitt 5.2 durch.
///
/// Der springende Punkt ist Schritt 4: Nach dem ersten Bruch der Kette wird
/// **nichts** mehr akzeptiert, auch kein spaeter folgender, in sich gueltiger
/// Record. Nach einem Absturz koennen im Ringpuffer intakte Records aus einer
/// frueheren Runde liegen; wer die mitnimmt, schreibt alte Daten ueber neue.
#[derive(Debug, Clone)]
pub struct ChainValidator {
    generation: u64,
    expected_seq: u64,
    broken: bool,
    accepted: u64,
    last_accepted: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainVerdict {
    /// Record gehoert zum Replay.
    Accept,
    /// Kette ist hier gebrochen; ab jetzt wird nichts mehr akzeptiert.
    StopReplay(ChainBreak),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainBreak {
    SequenceGap { expected: u64, found: u64 },
    GenerationMismatch { expected: u64, found: u64 },
    CorruptPayload,
    AlreadyBroken,
}

impl ChainValidator {
    /// `start_seq` ist die Sequenznummer, die als naechstes erwartet wird —
    /// also `checkpoint.seq + 1`.
    pub fn new(generation: u64, start_seq: u64) -> Self {
        ChainValidator {
            generation,
            expected_seq: start_seq,
            broken: false,
            accepted: 0,
            last_accepted: None,
        }
    }

    /// Ob der Replay zu Ende ist — gebrochen oder am Ende des Zahlenraums.
    pub fn is_broken(&self) -> bool {
        self.broken
    }

    pub fn accepted_count(&self) -> u64 {
        self.accepted
    }

    /// Letzte akzeptierte Sequenznummer, falls es eine gibt.
    ///
    /// Wird mitgefuehrt statt aus `expected_seq` zurueckgerechnet: Am oberen
    /// Ende des Zahlenraums gibt es kein `expected_seq` mehr, aus dem sich das
    /// ableiten liesse.
    pub fn last_accepted_seq(&self) -> Option<u64> {
        self.last_accepted
    }

    pub fn offer(&mut self, header: &LogRecordHeader, payload: &[u8]) -> ChainVerdict {
        if self.broken {
            return ChainVerdict::StopReplay(ChainBreak::AlreadyBroken);
        }
        if header.generation != self.generation {
            self.broken = true;
            return ChainVerdict::StopReplay(ChainBreak::GenerationMismatch {
                expected: self.generation,
                found: header.generation,
            });
        }
        if header.seq != self.expected_seq {
            self.broken = true;
            return ChainVerdict::StopReplay(ChainBreak::SequenceGap {
                expected: self.expected_seq,
                found: header.seq,
            });
        }
        if header.verify_payload(payload).is_err() {
            self.broken = true;
            return ChainVerdict::StopReplay(ChainBreak::CorruptPayload);
        }
        self.accepted += 1;
        self.last_accepted = Some(header.seq);
        match self.expected_seq.checked_add(1) {
            Some(next) => self.expected_seq = next,
            // Abschnitt 5.2: Auf `seq == u64::MAX` kann kein Nachfolger folgen.
            // Ein Wrap auf 0 wuerde als naechstes den aeltesten Record des
            // Ringpuffers erwarten und ihn ueber neue Daten schreiben.
            None => self.broken = true,
        }
        ChainVerdict::Accept
    }
}

pub fn round_up(value: usize, alignment: usize) -> usize {
    value.div_ceil(alignment) * alignment
}

fn read_u32(buffer: &[u8], offset: usize) -> u32 {
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&buffer[offset..offset + 4]);
    u32::from_le_bytes(bytes)
}

fn read_u64(buffer: &[u8], offset: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&buffer[offset..offset + 8]);
    u64::from_le_bytes(bytes)
}
