use core::fmt;

/// Fehler der Paritaetsrechnung.
///
/// Jede Variante steht fuer eine Bedingung, unter der ein Ergebnis still
/// falsch waere. In einem Speicherprojekt ist genau das die gefaehrliche
/// Klasse: Eine Rekonstruktion, die plausible Bytes liefert, aber die falschen,
/// faellt erst auf, wenn das Dateisystem darauf stolpert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParityError {
    /// Slot-Index ausserhalb `0..64` bzw. ausserhalb `data_slot_count`.
    SlotIndexOutOfRange { index: u8 },
    /// `data_slot_count` ausserhalb `1..=64`, `docs/FORMAT.md` Abschnitt 2.
    InvalidSlotCount { count: u8 },
    /// Derselbe Slot-Index kam zweimal vor.
    DuplicateSlot { index: u8 },
    /// Zweimal derselbe Index als ausgefallen gemeldet.
    SameSlotTwice { index: u8 },
    /// Ein als ausgefallen gemeldeter Slot steht in der Liste der Ueberlebenden.
    LostSlotAmongSurvivors { index: u8 },
    /// Es fehlen Slots. Eine Rekonstruktion aus einer unvollstaendigen Menge
    /// laeuft durch und liefert Muell — deshalb ein Fehler und keine Warnung.
    IncompleteSlotSet { expected: usize, got: usize },
    /// Ein Data-Slot ist laenger als der Parity-Member. Verletzt Bedingung 1
    /// aus Abschnitt 2 des Formatdokuments.
    SlotLongerThanParity {
        index: u8,
        slot_len: usize,
        parity_len: usize,
    },
    /// Ein Eingabepuffer deckt den zu rekonstruierenden Bereich nicht ab.
    BufferTooSmall {
        what: &'static str,
        need: usize,
        got: usize,
    },
    /// Division durch null im Feld. Kann bei gueltigen Slot-Indizes nicht
    /// auftreten, wird aber zurueckgegeben statt angenommen.
    DivisionByZero,
}

impl fmt::Display for ParityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SlotIndexOutOfRange { index } => {
                write!(f, "Slot-Index {index} ausserhalb des gueltigen Bereichs")
            }
            Self::InvalidSlotCount { count } => {
                write!(f, "data_slot_count {count} ausserhalb 1..=64")
            }
            Self::DuplicateSlot { index } => write!(f, "Slot {index} kam doppelt vor"),
            Self::SameSlotTwice { index } => {
                write!(f, "Slot {index} zweimal als ausgefallen gemeldet")
            }
            Self::LostSlotAmongSurvivors { index } => write!(
                f,
                "Slot {index} gilt als ausgefallen, steht aber unter den Ueberlebenden"
            ),
            Self::IncompleteSlotSet { expected, got } => write!(
                f,
                "unvollstaendige Slot-Menge: {expected} erwartet, {got} uebergeben"
            ),
            Self::SlotLongerThanParity {
                index,
                slot_len,
                parity_len,
            } => write!(
                f,
                "Slot {index} ist {slot_len} Bytes lang, die Paritaet nur {parity_len}"
            ),
            Self::BufferTooSmall { what, need, got } => {
                write!(
                    f,
                    "Puffer `{what}` zu klein: {need} noetig, {got} vorhanden"
                )
            }
            Self::DivisionByZero => write!(f, "Division durch null in GF(2^8)"),
        }
    }
}

impl std::error::Error for ParityError {}

pub type Result<T> = core::result::Result<T, ParityError>;
