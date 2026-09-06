// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

use core::fmt;

/// Fehler bei einer Pool-Entscheidung.
///
/// Jede Variante steht fuer eine Lage, in der es keine richtige Antwort gibt.
/// Insbesondere gibt es kein „nimm halt die am wenigsten schlechte Platte":
/// Wer bei vollem Pool trotzdem etwas anlegt, verschiebt das Problem in ein
/// ENOSPC mitten im Schreiben — und dort ist es teurer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoolError {
    /// Der Pfad taugt nicht als Pfad im Pool.
    InvalidPath { reason: &'static str },
    /// Fuer diesen Share kommt kein einziger Branch in Frage — alle
    /// ausgeschlossen, alle nur lesbar, oder die Liste ist leer.
    NoBranch,
    /// Alle in Frage kommenden Branches liegen unter der Reserve.
    NoSpace { needed: u64, min_free: u64 },
}

// Ein doppelter Name steht bewusst **nicht** hier. Er ist kein Fehler der
// Entscheidung, sondern eine Eigenschaft des Ergebnisses: `resolve` liefert
// ihn als [`Resolution::Conflict`](crate::Resolution::Conflict) samt der
// Angabe, welcher Branch bedient wird. Ein Dateisystem, das bei einem
// Konflikt einen Fehler wirft, macht den ganzen Ordner unbenutzbar.

impl fmt::Display for PoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath { reason } => write!(f, "Pfad unbrauchbar: {reason}"),
            Self::NoBranch => write!(
                f,
                "kein Branch kommt fuer diesen Share in Frage: alle ausgeschlossen oder nur lesbar"
            ),
            Self::NoSpace { needed, min_free } => write!(
                f,
                "kein Branch hat {needed} Bytes frei und behaelt dabei die Reserve von {min_free}"
            ),
        }
    }
}

impl std::error::Error for PoolError {}

pub type Result<T> = core::result::Result<T, PoolError>;
