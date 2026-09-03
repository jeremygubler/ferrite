// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

use core::fmt;
use ferrite_format::superblock::Role;
use ferrite_format::FormatError;

/// Fehler bei der Planung des Schreib- und Rebuild-Pfads.
///
/// Jede Variante steht fuer eine Situation, in der Weiterrechnen ein Ergebnis
/// ergaebe, das plausibel aussieht und falsch ist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// `offset + len` laeuft ueber. Beide Werte kommen aus einem Log-Record und
    /// damit ungeprueft von der Platte.
    OffsetOverflow { offset: u64, len: u64 },
    /// Der Bereich liegt jenseits des letzten Parity-Blocks des Arrays.
    BeyondArray { block: u64, block_count: u64 },
    /// Es wurde ein anderer Batch abgeschlossen als der ausgegebene. Wer hier
    /// vorbeigreift, meldet Bloecke als fertig, die nie rekonstruiert wurden.
    BatchOutOfOrder { expected: u64, got: u64 },
    /// Der Batch reicht ueber das Ende des Ziel-Members hinaus.
    BatchPastEnd { end: u64, limit: u64 },
    /// Fuer diese Rolle gibt es keinen Rebuild. Die Log-Region ist von keiner
    /// Paritaet gedeckt (`docs/FORMAT.md` Abschnitt 4.2).
    CannotRebuild { role: Role },
    /// Der Member gehoert nicht zu diesem Array — verletzt Regel 2 aus
    /// Abschnitt 2.1 und waere beim Assemble schon aufgefallen.
    MismatchedBlockSize { array: u8, member: u8 },
    /// Nach einem Absturz **und** im degradierten Betrieb: Neurechnen geht
    /// nicht, weil der fehlende Member nicht lesbar ist, und Fortschreiben
    /// geht nicht, weil der alte Inhalt nicht mehr verlaesslich ist. Diese
    /// Kombination hat keine sichere Antwort — sie gehoert ins Crash-Harness.
    CannotUpdateParity,
    /// Eine Stufe des Schreibpfads wurde uebersprungen oder rueckwaerts
    /// gegangen.
    IllegalTransition {
        from: crate::write_path::BatchStage,
        to: crate::write_path::BatchStage,
    },
    /// Checkpoint, bevor die Paritaet durable ist. Damit wuerde Log-Platz
    /// freigegeben, dessen Inhalt noch gebraucht wird.
    CheckpointBeforeParity {
        stage: crate::write_path::BatchStage,
    },
    /// Eine Regel aus `docs/FORMAT.md` wurde verletzt.
    Format(FormatError),
}

impl From<FormatError> for EngineError {
    fn from(error: FormatError) -> Self {
        EngineError::Format(error)
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OffsetOverflow { offset, len } => {
                write!(f, "Offset {offset} plus Laenge {len} laeuft ueber")
            }
            Self::BeyondArray { block, block_count } => write!(
                f,
                "Parity-Block {block} liegt jenseits der {block_count} Bloecke des Arrays"
            ),
            Self::BatchOutOfOrder { expected, got } => {
                write!(f, "Batch beginnt bei Block {got}, erwartet war {expected}")
            }
            Self::BatchPastEnd { end, limit } => {
                write!(f, "Batch endet bei Block {end}, der Member hat {limit}")
            }
            Self::CannotRebuild { role } => write!(f, "fuer Rolle {role:?} gibt es keinen Rebuild"),
            Self::MismatchedBlockSize { array, member } => {
                write!(f, "Array rechnet mit 2^{array}, der Member mit 2^{member}")
            }
            Self::CannotUpdateParity => write!(
                f,
                "Paritaet nach Absturz im degradierten Betrieb: weder neu rechenbar noch fortschreibbar"
            ),
            Self::IllegalTransition { from, to } => {
                write!(f, "Uebergang {from:?} -> {to:?} ist nicht erlaubt")
            }
            Self::CheckpointBeforeParity { stage } => write!(
                f,
                "Checkpoint bei Stufe {stage:?}, die Paritaet ist noch nicht durable"
            ),
            Self::Format(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for EngineError {}

pub type Result<T> = core::result::Result<T, EngineError>;
