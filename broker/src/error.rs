// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

use core::fmt;

use ferrite_engine::EngineError;

/// Fehler des Repair-Brokers.
///
/// Eigener Typ und keine Wiederverwendung von [`EngineError`]: Der Broker
/// scheitert an Dingen, von denen die Engine nichts weiss — an einem Puffer,
/// den er nicht lesen darf, und an einem Schreibpfad, den ein anderer Thread
/// mitgerissen hat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerError {
    /// Der Schreibpfad ist in einem anderen Thread abgestuerzt.
    ///
    /// Weiterzuarbeiten hiesse, auf einem Zustand zu rechnen, ueber den
    /// niemand etwas weiss. Genau das darf eine Reparatur nicht.
    WriterPoisoned,
    /// Die Engine hat abgelehnt.
    Engine(EngineError),
    /// Fehler vom Betriebssystem, mit der Angabe, was versucht wurde.
    Io {
        what: &'static str,
        kind: std::io::ErrorKind,
        raw_os_error: Option<i32>,
    },
}

impl fmt::Display for BrokerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WriterPoisoned => write!(
                f,
                "der Schreibpfad ist in einem anderen Thread abgestuerzt, eine Reparatur waere Rechnen auf Unbekanntem"
            ),
            Self::Engine(error) => write!(f, "{error}"),
            Self::Io {
                what,
                kind,
                raw_os_error,
            } => match raw_os_error {
                Some(code) => write!(f, "{what}: {kind:?} (errno {code})"),
                None => write!(f, "{what}: {kind:?}"),
            },
        }
    }
}

impl std::error::Error for BrokerError {}

impl From<EngineError> for BrokerError {
    fn from(error: EngineError) -> Self {
        BrokerError::Engine(error)
    }
}

/// Verpackt einen `io::Error` mit der Angabe, was gerade versucht wurde.
///
/// Gebraucht wird das nur vom Kernel-Ringpuffer, und den gibt es nur auf
/// Linux. Anderswo bliebe die Funktion ungenutzt — das ist kein Versehen.
#[cfg(target_os = "linux")]
pub(crate) fn io_error(what: &'static str) -> impl FnOnce(std::io::Error) -> BrokerError {
    move |error| BrokerError::Io {
        what,
        kind: error.kind(),
        raw_os_error: error.raw_os_error(),
    }
}

pub type Result<T> = core::result::Result<T, BrokerError>;
