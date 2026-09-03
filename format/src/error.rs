use core::fmt;

/// Fehler beim Dekodieren oder Validieren von On-Disk-Strukturen.
///
/// Jede Variante entspricht einer Regel aus `docs/FORMAT.md`. Wenn hier eine
/// Variante fehlt, fehlt im Zweifel auch die Regel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatError {
    /// Der Puffer ist kleiner als die Struktur, die gelesen werden soll.
    BufferTooSmall { need: usize, got: usize },
    /// Magic-Bytes passen nicht. Fremdes oder leeres Geraet.
    BadMagic {
        expected: &'static [u8],
        found: [u8; 8],
    },
    /// Pruefsumme stimmt nicht. Torn write oder Bit-Rot in den Metadaten.
    ChecksumMismatch { expected: u32, computed: u32 },
    /// Major-Version wird von dieser Implementierung nicht unterstuetzt.
    UnsupportedVersion { major: u16, minor: u16 },
    /// Unbekannte Bits in `feature_incompat`. Array darf nicht angefasst werden.
    IncompatibleFeatures { unknown: u64 },
    /// Unbekannte Bits in `feature_ro_compat`. Nur lesender Zugriff erlaubt.
    ReadOnlyFeatures { unknown: u64 },
    /// Zustandsbyte ausserhalb des definierten Bereichs, Abschnitt 4.2.
    UnknownMemberState(u8),
    /// Rollenbyte ausserhalb des definierten Bereichs.
    UnknownRole(u8),
    /// Record-Typ ausserhalb des definierten Bereichs.
    UnknownRecordType(u16),
    /// `header_size` passt nicht zur Version.
    BadHeaderSize { expected: u32, found: u32 },
    /// Ein Feld verletzt eine Gueltigkeitsregel aus Abschnitt 4 bzw. 5.
    InvalidField {
        field: &'static str,
        reason: &'static str,
    },

    // Ab hier: Array-Ebene, Abschnitt 2.1. Diese Fehler betreffen nicht einen
    // einzelnen Superblock, sondern das Verhaeltnis der Members zueinander.
    /// Regel 1: leere Member-Liste.
    NoMembers,
    /// Regel 4 und 5 zusammen begrenzen ein Array auf 67 Members.
    TooManyMembers { max: usize, got: usize },
    /// Regel 2: ein Member weicht in einem arrayweiten Parameter ab.
    MismatchedArrayParameter { field: &'static str, member: usize },
    /// Regel 3: zwei Members tragen dieselbe `member_uuid` — dieselbe Platte.
    DuplicateMemberUuid { first: usize, second: usize },
    /// Regel 4: eine Rolle kommt oefter vor, als sie darf.
    DuplicateRole {
        role: crate::superblock::Role,
        first: usize,
        second: usize,
    },
    /// Regel 4: ohne ParityP gibt es keine Redundanz und keine Bezugslaenge.
    MissingParityP,
    /// Regel 5: zwei Data-Members beanspruchen denselben Slot.
    DuplicateDataSlot {
        slot_index: u16,
        first: usize,
        second: usize,
    },
    /// Regel 5: fuer diesen Slot fehlt der Member.
    MissingDataSlot { slot_index: u16 },
    /// Regel 6: ein Parity-Member ist kuerzer als der laengste Data-Member.
    ParityTooShort {
        role: crate::superblock::Role,
        parity_size: u64,
        data_size: u64,
        slot_index: u16,
    },
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BufferTooSmall { need, got } => {
                write!(f, "Puffer zu klein: {need} Bytes noetig, {got} vorhanden")
            }
            Self::BadMagic { expected, found } => write!(
                f,
                "falsche Magic-Bytes: erwartet {expected:?}, gefunden {found:?}"
            ),
            Self::ChecksumMismatch { expected, computed } => write!(
                f,
                "Pruefsumme falsch: gespeichert {expected:#010x}, berechnet {computed:#010x}"
            ),
            Self::UnsupportedVersion { major, minor } => {
                write!(f, "Format-Version {major}.{minor} nicht unterstuetzt")
            }
            Self::IncompatibleFeatures { unknown } => write!(
                f,
                "unbekannte incompat-Features {unknown:#018x}, Array wird nicht angefasst"
            ),
            Self::ReadOnlyFeatures { unknown } => write!(
                f,
                "unbekannte ro_compat-Features {unknown:#018x}, nur lesender Zugriff"
            ),
            Self::UnknownMemberState(state) => write!(f, "unbekannter Member-Zustand {state}"),
            Self::UnknownRole(role) => write!(f, "unbekannte Rolle {role}"),
            Self::UnknownRecordType(kind) => write!(f, "unbekannter Record-Typ {kind}"),
            Self::BadHeaderSize { expected, found } => {
                write!(f, "header_size {found} statt {expected}")
            }
            Self::InvalidField { field, reason } => write!(f, "Feld `{field}` ungueltig: {reason}"),
            Self::NoMembers => write!(f, "Array ohne Members"),
            Self::TooManyMembers { max, got } => {
                write!(f, "{got} Members, hoechstens {max} sind moeglich")
            }
            Self::MismatchedArrayParameter { field, member } => {
                write!(f, "Member {member} weicht in `{field}` von den uebrigen ab")
            }
            Self::DuplicateMemberUuid { first, second } => write!(
                f,
                "Members {first} und {second} tragen dieselbe member_uuid"
            ),
            Self::DuplicateRole {
                role,
                first,
                second,
            } => write!(
                f,
                "Rolle {role:?} kommt doppelt vor, Members {first} und {second}"
            ),
            Self::MissingParityP => write!(f, "kein Member mit Rolle ParityP"),
            Self::DuplicateDataSlot {
                slot_index,
                first,
                second,
            } => write!(
                f,
                "Slot {slot_index} doppelt belegt, Members {first} und {second}"
            ),
            Self::MissingDataSlot { slot_index } => {
                write!(f, "fuer Slot {slot_index} fehlt der Member")
            }
            Self::ParityTooShort {
                role,
                parity_size,
                data_size,
                slot_index,
            } => write!(
                f,
                "{role:?} ist {parity_size} Bytes lang, Slot {slot_index} aber {data_size}"
            ),
        }
    }
}

impl std::error::Error for FormatError {}

pub type Result<T> = core::result::Result<T, FormatError>;
