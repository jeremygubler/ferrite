//! Superblock, `docs/FORMAT.md` Abschnitt 4.
//!
//! Die Offset-Konstanten unten sind die einzige Stelle, an der das Layout
//! steht. Wer sie aendert, aendert das Format und muss `FORMAT.md` und die
//! Versionshistorie mitziehen.

use crate::crc32c::crc32c;
use crate::error::{FormatError, Result};
use crate::uuid::Uuid;

pub const SUPERBLOCK_MAGIC: &[u8; 8] = b"FERRITE1";
pub const SUPERBLOCK_SIZE: usize = 4096;

/// Byte-Offset des primaeren Superblocks auf jedem Member.
pub const SUPERBLOCK_PRIMARY_OFFSET: u64 = 65_536;
/// Abstand des Backup-Superblocks vom Geraeteende.
pub const SUPERBLOCK_BACKUP_FROM_END: u64 = 65_536;
/// Beginn der Payload-Region in Version 0.1.
pub const DEFAULT_PAYLOAD_OFFSET: u64 = 1_048_576;

/// Kleinste Geraetegroesse, auf der beide Superbloecke ueberschneidungsfrei
/// Platz haben, `docs/FORMAT.md` Abschnitt 3.
pub const MIN_DEVICE_SIZE: u64 =
    SUPERBLOCK_PRIMARY_OFFSET + SUPERBLOCK_SIZE as u64 + SUPERBLOCK_BACKUP_FROM_END;

/// Eingefroren. `version_major` ist ab hier `1`, und Superbloecke aus den
/// `0.x`-Entwuerfen werden abgewiesen — es gibt keine mit Nutzdaten.
pub const VERSION_MAJOR: u16 = 1;
pub const VERSION_MINOR: u16 = 0;

pub const MIN_PARITY_BLOCK_LOG2: u8 = 12;
pub const MAX_PARITY_BLOCK_LOG2: u8 = 24;
pub const MAX_DATA_SLOTS: u32 = 64;

/// Bekannte Feature-Bits dieser Implementierung. In 0.1 noch keine.
pub const KNOWN_COMPAT: u64 = 0;
pub const KNOWN_INCOMPAT: u64 = 0;
pub const KNOWN_RO_COMPAT: u64 = 0;

const OFF_MAGIC: usize = 0;
const OFF_VERSION_MAJOR: usize = 8;
const OFF_VERSION_MINOR: usize = 10;
const OFF_HEADER_SIZE: usize = 12;
const OFF_ARRAY_UUID: usize = 16;
const OFF_MEMBER_UUID: usize = 32;
const OFF_ROLE: usize = 48;
const OFF_PARITY_BLOCK_LOG2: usize = 49;
const OFF_SLOT_INDEX: usize = 50;
const OFF_DATA_SLOT_COUNT: usize = 52;
const OFF_PAYLOAD_OFFSET: usize = 56;
const OFF_PAYLOAD_SIZE: usize = 64;
const OFF_GENERATION: usize = 72;
const OFF_CREATED_UNIX: usize = 80;
const OFF_FEATURE_COMPAT: usize = 88;
const OFF_FEATURE_INCOMPAT: usize = 96;
const OFF_FEATURE_RO_COMPAT: usize = 104;
const OFF_LABEL: usize = 112;
const LABEL_LEN: usize = 32;
const OFF_MEMBER_STATE: usize = 144;
const OFF_REBUILD_PROGRESS: usize = 152;
// Ab Offset 160 bis zur Pruefsumme ist alles reserviert und wird als Null
// geschrieben — der Vorrat, aus dem `member_state` und `rebuild_progress` in
// Version 0.2 kamen.
const OFF_CRC: usize = 4092;

/// Rolle eines Members im Array.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Role {
    Data = 0,
    ParityP = 1,
    ParityQ = 2,
    Log = 3,
}

impl Role {
    pub fn from_u8(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Role::Data),
            1 => Ok(Role::ParityP),
            2 => Ok(Role::ParityQ),
            3 => Ok(Role::Log),
            other => Err(FormatError::UnknownRole(other)),
        }
    }

    pub fn is_parity(&self) -> bool {
        matches!(self, Role::ParityP | Role::ParityQ)
    }
}

/// Zustand eines Members, `docs/FORMAT.md` Abschnitt 4.2.
///
/// Ohne dieses Feld liesse sich eine frisch getauschte Platte nicht von einer
/// intakten unterscheiden: Beide tragen einen gueltigen Superblock, und die
/// Payload einer halb wiederhergestellten Platte sieht aus wie Daten.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum MemberState {
    /// Die Payload passt zur Paritaet. Normalfall — und der Wert, den ein
    /// Superblock aus Version 0.1 im damals reservierten Byte stehen hatte.
    #[default]
    Clean = 0,
    /// Nur `[0, rebuild_progress)` ist gueltig, der Rest noch nicht geschrieben.
    Rebuilding = 1,
    /// Der Inhalt ist aelter als das Array. Nichts davon ist gueltig.
    Stale = 2,
}

impl MemberState {
    pub fn from_u8(value: u8) -> Result<Self> {
        match value {
            0 => Ok(MemberState::Clean),
            1 => Ok(MemberState::Rebuilding),
            2 => Ok(MemberState::Stale),
            other => Err(FormatError::UnknownMemberState(other)),
        }
    }

    /// Ob dieser Member ueberhaupt als Datenquelle taugt.
    ///
    /// Bei `Rebuilding` nur unterhalb von `rebuild_progress` — das entscheidet
    /// der Aufrufer, hier steht nur, dass es nicht unbesehen geht.
    pub fn is_clean(&self) -> bool {
        matches!(self, MemberState::Clean)
    }
}

/// Wie ein Array nach der Feature-Pruefung geoeffnet werden darf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    ReadWrite,
    ReadOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Superblock {
    pub version_major: u16,
    pub version_minor: u16,
    pub array_uuid: Uuid,
    pub member_uuid: Uuid,
    pub role: Role,
    pub parity_block_size_log2: u8,
    pub slot_index: u16,
    pub data_slot_count: u32,
    pub payload_offset: u64,
    pub payload_size: u64,
    pub generation: u64,
    pub created_unix: u64,
    pub feature_compat: u64,
    pub feature_incompat: u64,
    pub feature_ro_compat: u64,
    pub label: String,
    pub member_state: MemberState,
    /// Nur bei `MemberState::Rebuilding` von null verschieden.
    pub rebuild_progress: u64,
}

impl Superblock {
    /// Neuer Superblock mit den Vorgaben von Version 0.2.
    pub fn new(
        array_uuid: Uuid,
        member_uuid: Uuid,
        role: Role,
        data_slot_count: u32,
        payload_size: u64,
    ) -> Self {
        Superblock {
            version_major: VERSION_MAJOR,
            version_minor: VERSION_MINOR,
            array_uuid,
            member_uuid,
            role,
            parity_block_size_log2: 16,
            slot_index: 0,
            data_slot_count,
            payload_offset: DEFAULT_PAYLOAD_OFFSET,
            payload_size,
            generation: 1,
            created_unix: 0,
            feature_compat: 0,
            feature_incompat: 0,
            feature_ro_compat: 0,
            label: String::new(),
            member_state: MemberState::Clean,
            rebuild_progress: 0,
        }
    }

    pub fn parity_block_size(&self) -> u64 {
        1u64 << self.parity_block_size_log2
    }

    /// Anzahl Parity-Bloecke in der Payload-Region.
    pub fn parity_block_count(&self) -> u64 {
        self.payload_size >> self.parity_block_size_log2
    }

    /// Offset des Backup-Superblocks bei gegebener Geraetegroesse.
    pub fn backup_offset(device_size: u64) -> Option<u64> {
        device_size.checked_sub(SUPERBLOCK_BACKUP_FROM_END)
    }

    /// Ende der Payload-Region, `None` bei Ueberlauf.
    pub fn payload_end(&self) -> Option<u64> {
        self.payload_offset.checked_add(self.payload_size)
    }

    /// Prueft die beiden Bedingungen aus Abschnitt 3, die die Geraetegroesse
    /// brauchen.
    ///
    /// Sie steht nicht auf der Platte, sondern kommt vom Blockgeraet — deshalb
    /// als Parameter und nicht in [`Superblock::validate`]. Ohne diese Pruefung
    /// legt ein Array seine Payload-Region ueber den Backup-Superblock und
    /// zerstoert beim ersten Write auf den letzten Block genau die Kopie, die
    /// fuer den Ausfall des primaeren da ist.
    pub fn fits_on_device(&self, device_size: u64) -> Result<()> {
        self.validate()?;

        if device_size < MIN_DEVICE_SIZE {
            return Err(FormatError::InvalidField {
                field: "device_size",
                reason: "zu klein fuer primaeren und Backup-Superblock",
            });
        }
        // Nach der Pruefung oben ist die Subtraktion sicher.
        let backup = device_size - SUPERBLOCK_BACKUP_FROM_END;

        let end = self.payload_end().ok_or(FormatError::InvalidField {
            field: "payload_size",
            reason: "payload_offset + payload_size laeuft ueber",
        })?;
        if end > backup {
            return Err(FormatError::InvalidField {
                field: "payload_size",
                reason: "reicht in den Backup-Superblock",
            });
        }
        Ok(())
    }

    /// Prueft die Feature-Flags gegen das, was diese Implementierung kennt.
    pub fn access_mode(&self) -> Result<AccessMode> {
        let unknown_incompat = self.feature_incompat & !KNOWN_INCOMPAT;
        if unknown_incompat != 0 {
            return Err(FormatError::IncompatibleFeatures {
                unknown: unknown_incompat,
            });
        }
        let unknown_ro = self.feature_ro_compat & !KNOWN_RO_COMPAT;
        if unknown_ro != 0 {
            return Ok(AccessMode::ReadOnly);
        }
        Ok(AccessMode::ReadWrite)
    }

    /// Gueltigkeitsregeln aus Abschnitt 4 des Formatdokuments.
    pub fn validate(&self) -> Result<()> {
        if self.version_major != VERSION_MAJOR {
            return Err(FormatError::UnsupportedVersion {
                major: self.version_major,
                minor: self.version_minor,
            });
        }
        if !(MIN_PARITY_BLOCK_LOG2..=MAX_PARITY_BLOCK_LOG2).contains(&self.parity_block_size_log2) {
            return Err(FormatError::InvalidField {
                field: "parity_block_size_log2",
                reason: "ausserhalb 12..=24",
            });
        }
        if self.data_slot_count == 0 || self.data_slot_count > MAX_DATA_SLOTS {
            return Err(FormatError::InvalidField {
                field: "data_slot_count",
                reason: "ausserhalb 1..=64",
            });
        }
        if self.role == Role::Data && u32::from(self.slot_index) >= self.data_slot_count {
            return Err(FormatError::InvalidField {
                field: "slot_index",
                reason: "muss kleiner als data_slot_count sein",
            });
        }
        if self.payload_offset % 4096 != 0 {
            return Err(FormatError::InvalidField {
                field: "payload_offset",
                reason: "nicht 4096-aligned",
            });
        }
        if self.payload_offset < SUPERBLOCK_PRIMARY_OFFSET + SUPERBLOCK_SIZE as u64 {
            return Err(FormatError::InvalidField {
                field: "payload_offset",
                reason: "ueberlappt den primaeren Superblock",
            });
        }
        // Fuer Log-Member ist die Region ein Ringpuffer aus 4096er-Sektoren und
        // muss nicht am Parity-Block ausgerichtet sein.
        if self.role != Role::Log {
            if self.payload_size == 0 {
                return Err(FormatError::InvalidField {
                    field: "payload_size",
                    reason: "darf nicht null sein",
                });
            }
            if self.payload_size % self.parity_block_size() != 0 {
                return Err(FormatError::InvalidField {
                    field: "payload_size",
                    reason: "kein Vielfaches der Parity-Block-Groesse",
                });
            }
        } else if self.payload_size % 4096 != 0 {
            return Err(FormatError::InvalidField {
                field: "payload_size",
                reason: "Log-Region nicht 4096-aligned",
            });
        }
        if self.label.len() > LABEL_LEN {
            return Err(FormatError::InvalidField {
                field: "label",
                reason: "laenger als 32 Bytes",
            });
        }
        // Das Feld wird mit Nullbytes aufgefuellt, das erste Nullbyte ist damit
        // das Ende des Labels. Ein Label mit eingebettetem Nullbyte laese sich
        // abgeschnitten zurueck — der Superblock waere nach einem
        // Schreib-Lese-Zyklus ein anderer.
        if self.label.as_bytes().contains(&0) {
            return Err(FormatError::InvalidField {
                field: "label",
                reason: "enthaelt ein Nullbyte",
            });
        }
        // Abschnitt 4.2. Ein Log-Member kann nicht rebuilden: Seine Region ist
        // von keiner Paritaet gedeckt, ein leeres Log ist immer zulaessig.
        if self.role == Role::Log && !self.member_state.is_clean() {
            return Err(FormatError::InvalidField {
                field: "member_state",
                reason: "ein Log-Member muss Clean sein",
            });
        }
        if self.member_state == MemberState::Rebuilding {
            if self.rebuild_progress > self.payload_size {
                return Err(FormatError::InvalidField {
                    field: "rebuild_progress",
                    reason: "groesser als payload_size",
                });
            }
            // Rekonstruiert wird blockweise. Ein Fortschritt mitten in einem
            // Parity-Block waere nicht wiederaufsetzbar.
            if self.rebuild_progress % self.parity_block_size() != 0 {
                return Err(FormatError::InvalidField {
                    field: "rebuild_progress",
                    reason: "kein Vielfaches der Parity-Block-Groesse",
                });
            }
        } else if self.rebuild_progress != 0 {
            return Err(FormatError::InvalidField {
                field: "rebuild_progress",
                reason: "nur bei Rebuilding von null verschieden",
            });
        }
        if self.array_uuid.is_nil() || self.member_uuid.is_nil() {
            return Err(FormatError::InvalidField {
                field: "uuid",
                reason: "darf nicht null sein",
            });
        }
        Ok(())
    }

    /// Serialisiert in einen 4096-Byte-Block inklusive Pruefsumme.
    pub fn encode(&self) -> Result<[u8; SUPERBLOCK_SIZE]> {
        self.validate()?;
        let mut buffer = [0u8; SUPERBLOCK_SIZE];

        buffer[OFF_MAGIC..OFF_MAGIC + 8].copy_from_slice(SUPERBLOCK_MAGIC);
        put_u16(&mut buffer, OFF_VERSION_MAJOR, self.version_major);
        put_u16(&mut buffer, OFF_VERSION_MINOR, self.version_minor);
        put_u32(&mut buffer, OFF_HEADER_SIZE, SUPERBLOCK_SIZE as u32);
        buffer[OFF_ARRAY_UUID..OFF_ARRAY_UUID + 16].copy_from_slice(self.array_uuid.as_bytes());
        buffer[OFF_MEMBER_UUID..OFF_MEMBER_UUID + 16].copy_from_slice(self.member_uuid.as_bytes());
        buffer[OFF_ROLE] = self.role as u8;
        buffer[OFF_PARITY_BLOCK_LOG2] = self.parity_block_size_log2;
        put_u16(&mut buffer, OFF_SLOT_INDEX, self.slot_index);
        put_u32(&mut buffer, OFF_DATA_SLOT_COUNT, self.data_slot_count);
        put_u64(&mut buffer, OFF_PAYLOAD_OFFSET, self.payload_offset);
        put_u64(&mut buffer, OFF_PAYLOAD_SIZE, self.payload_size);
        put_u64(&mut buffer, OFF_GENERATION, self.generation);
        put_u64(&mut buffer, OFF_CREATED_UNIX, self.created_unix);
        put_u64(&mut buffer, OFF_FEATURE_COMPAT, self.feature_compat);
        put_u64(&mut buffer, OFF_FEATURE_INCOMPAT, self.feature_incompat);
        put_u64(&mut buffer, OFF_FEATURE_RO_COMPAT, self.feature_ro_compat);

        let label = self.label.as_bytes();
        buffer[OFF_LABEL..OFF_LABEL + label.len()].copy_from_slice(label);

        buffer[OFF_MEMBER_STATE] = self.member_state as u8;
        put_u64(&mut buffer, OFF_REBUILD_PROGRESS, self.rebuild_progress);

        let checksum = crc32c(&buffer[..OFF_CRC]);
        put_u32(&mut buffer, OFF_CRC, checksum);
        Ok(buffer)
    }

    /// Liest einen Superblock und prueft Magic, Pruefsumme und Feldregeln.
    ///
    /// Reihenfolge ist Absicht: erst Magic, dann Pruefsumme, dann Semantik. Ein
    /// fremdes Geraet soll `BadMagic` melden und nicht `ChecksumMismatch`.
    pub fn decode(buffer: &[u8]) -> Result<Self> {
        if buffer.len() < SUPERBLOCK_SIZE {
            return Err(FormatError::BufferTooSmall {
                need: SUPERBLOCK_SIZE,
                got: buffer.len(),
            });
        }
        let block = &buffer[..SUPERBLOCK_SIZE];

        if &block[OFF_MAGIC..OFF_MAGIC + 8] != SUPERBLOCK_MAGIC {
            let mut found = [0u8; 8];
            found.copy_from_slice(&block[OFF_MAGIC..OFF_MAGIC + 8]);
            return Err(FormatError::BadMagic {
                expected: SUPERBLOCK_MAGIC,
                found,
            });
        }

        let stored = get_u32(block, OFF_CRC);
        let computed = crc32c(&block[..OFF_CRC]);
        if stored != computed {
            return Err(FormatError::ChecksumMismatch {
                expected: stored,
                computed,
            });
        }

        let header_size = get_u32(block, OFF_HEADER_SIZE);
        if header_size != SUPERBLOCK_SIZE as u32 {
            return Err(FormatError::BadHeaderSize {
                expected: SUPERBLOCK_SIZE as u32,
                found: header_size,
            });
        }

        let label_bytes = &block[OFF_LABEL..OFF_LABEL + LABEL_LEN];
        let label_end = label_bytes
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(LABEL_LEN);
        let label = core::str::from_utf8(&label_bytes[..label_end])
            .map_err(|_| FormatError::InvalidField {
                field: "label",
                reason: "kein gueltiges UTF-8",
            })?
            .to_owned();

        let superblock = Superblock {
            version_major: get_u16(block, OFF_VERSION_MAJOR),
            version_minor: get_u16(block, OFF_VERSION_MINOR),
            array_uuid: read_uuid(block, OFF_ARRAY_UUID),
            member_uuid: read_uuid(block, OFF_MEMBER_UUID),
            role: Role::from_u8(block[OFF_ROLE])?,
            parity_block_size_log2: block[OFF_PARITY_BLOCK_LOG2],
            slot_index: get_u16(block, OFF_SLOT_INDEX),
            data_slot_count: get_u32(block, OFF_DATA_SLOT_COUNT),
            payload_offset: get_u64(block, OFF_PAYLOAD_OFFSET),
            payload_size: get_u64(block, OFF_PAYLOAD_SIZE),
            generation: get_u64(block, OFF_GENERATION),
            created_unix: get_u64(block, OFF_CREATED_UNIX),
            feature_compat: get_u64(block, OFF_FEATURE_COMPAT),
            feature_incompat: get_u64(block, OFF_FEATURE_INCOMPAT),
            feature_ro_compat: get_u64(block, OFF_FEATURE_RO_COMPAT),
            label,
            member_state: MemberState::from_u8(block[OFF_MEMBER_STATE])?,
            rebuild_progress: get_u64(block, OFF_REBUILD_PROGRESS),
        };

        superblock.validate()?;
        Ok(superblock)
    }

    /// Waehlt zwischen primaerem und Backup-Superblock.
    ///
    /// Gewinner ist der gueltige mit der hoeheren `generation`. Sind beide
    /// kaputt, wird der Fehler des primaeren zurueckgegeben — der ist fuer die
    /// Diagnose der interessantere.
    pub fn select(primary: &[u8], backup: &[u8]) -> Result<Self> {
        match (Self::decode(primary), Self::decode(backup)) {
            (Ok(a), Ok(b)) => Ok(if b.generation > a.generation { b } else { a }),
            (Ok(a), Err(_)) => Ok(a),
            (Err(_), Ok(b)) => Ok(b),
            (Err(primary_error), Err(_)) => Err(primary_error),
        }
    }
}

fn read_uuid(buffer: &[u8], offset: usize) -> Uuid {
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&buffer[offset..offset + 16]);
    Uuid::from_bytes(bytes)
}

fn put_u16(buffer: &mut [u8], offset: usize, value: u16) {
    buffer[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(buffer: &mut [u8], offset: usize, value: u64) {
    buffer[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn get_u16(buffer: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([buffer[offset], buffer[offset + 1]])
}

fn get_u32(buffer: &[u8], offset: usize) -> u32 {
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&buffer[offset..offset + 4]);
    u32::from_le_bytes(bytes)
}

fn get_u64(buffer: &[u8], offset: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&buffer[offset..offset + 8]);
    u64::from_le_bytes(bytes)
}
