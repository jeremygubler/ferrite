// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Der Schreibpfad als reine Zustandslogik, `docs/FORMAT.md` Abschnitt 5.
//!
//! Ein Write gilt als bestaetigt, sobald sein Record im Log durable ist. Was
//! danach passiert — Uebertragung auf den Data-Member, Paritaet, Checkpoint —
//! laeuft gebuendelt und in einer Reihenfolge, die nicht verhandelbar ist.
//! Dieses Modul haelt diese Reihenfolge fest und rechnet aus, wie die Paritaet
//! eines Blocks nachgezogen wird. Es liest und schreibt dabei nichts.
//!
//! Die teuerste Regel steht in [`BatchStage`]: **Kein Checkpoint, bevor die
//! Paritaet durable ist.** Wer sie bricht, gibt Log-Platz frei, dessen Inhalt
//! noch gebraucht wuerde — die Writes sind bestaetigt, stehen auf den
//! Data-Members, und die Paritaet passt nicht dazu. Nach dem naechsten
//! Plattenausfall rekonstruiert das Array Muell.

use crate::error::{EngineError, Result};

/// Wie die Paritaet eines Parity-Blocks auf den neuen Stand kommt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParityUpdate {
    /// Vollstaendig neu rechnen: alle beitragenden Data-Slots dieses Blocks
    /// lesen und `P` beziehungsweise `Q` daraus bilden.
    Recompute,
    /// Fortschreiben: `P' = P ^ D_alt ^ D_neu`, entsprechend fuer `Q`. Braucht
    /// den **alten** Inhalt der geaenderten Bereiche, also einen Read, bevor
    /// der Data-Member ueberschrieben wird.
    Incremental,
}

/// Woher der Stapel kommt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BatchOrigin {
    /// Laufender Betrieb. Die Paritaet passt zum aktuellen Inhalt der
    /// Data-Members, und der alte Inhalt ist noch lesbar.
    SteadyState,
    /// Aus dem Log wiederhergestellt, nach einem Absturz. Ob die Writes schon
    /// auf den Data-Members stehen, ist **unbekannt** — der Absturz kann
    /// jederzeit passiert sein.
    Replay,
}

/// Ob alle Data-Slots an diesem Block brauchbare Daten liefern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceState {
    AllValid,
    /// Mindestens ein Member ist `Stale` oder oberhalb seines
    /// `rebuild_progress` (Abschnitt 4.2).
    Degraded,
}

/// Die Lage eines einzelnen Parity-Blocks, aus der sich die Kosten ergeben.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockSituation {
    /// Data-Slots, die an diesem Block ueberhaupt Daten tragen.
    ///
    /// Kuerzere Members zaehlen nicht mit: Sie liefern jenseits ihres Endes
    /// per Zero-Extension Nullbytes, und die muss niemand von der Platte holen.
    pub contributing_slots: u32,
    /// Davon die, deren neuer Inhalt bereits vorliegt — er stand im Log.
    pub written_slots: u32,
    pub has_parity_q: bool,
}

impl BlockSituation {
    /// Lesezugriffe fuer das vollstaendige Neurechnen.
    ///
    /// Die Slots, deren neuer Inhalt schon vorliegt, muessen nicht gelesen
    /// werden. Die Paritaet selbst auch nicht — sie wird ueberschrieben.
    pub fn reads_for_recompute(&self) -> u32 {
        self.contributing_slots.saturating_sub(self.written_slots)
    }

    /// Lesezugriffe fuer das Fortschreiben: je ein alter Inhalt pro
    /// geschriebenem Slot, dazu die alte Paritaet.
    pub fn reads_for_incremental(&self) -> u32 {
        self.written_slots + 1 + u32::from(self.has_parity_q)
    }
}

/// Welches Verfahren fuer diesen Block **korrekt** ist.
///
/// Die beiden interessanten Zeilen der Tabelle:
///
/// - Nach einem Absturz ist Fortschreiben falsch. Der Replay wendet die Writes
///   erneut an; steht der neue Inhalt schon auf der Platte, ist `D_alt` in
///   Wirklichkeit `D_neu`, und `P ^ D_neu ^ D_neu` laesst die Paritaet
///   unveraendert — also veraltet. Es muss neu gerechnet werden.
/// - Im degradierten Betrieb ist Neurechnen unmoeglich: Der Inhalt des
///   fehlenden Members laesst sich nicht lesen. Fortschreiben geht dagegen,
///   weil die alte Paritaet den Beitrag des fehlenden Members bereits enthaelt
///   und er sich nicht aendert.
///
/// Beides zusammen — Absturz **und** degradiert — geht nicht, und diese
/// Funktion sagt das, statt eine der beiden falschen Antworten zu geben.
pub fn required_parity_update(
    origin: BatchOrigin,
    sources: SourceState,
    situation: BlockSituation,
) -> Result<ParityUpdate> {
    match (origin, sources) {
        (BatchOrigin::Replay, SourceState::Degraded) => Err(EngineError::CannotUpdateParity),
        (BatchOrigin::Replay, SourceState::AllValid) => Ok(ParityUpdate::Recompute),
        (BatchOrigin::SteadyState, SourceState::Degraded) => Ok(ParityUpdate::Incremental),
        // Beide Verfahren sind korrekt. Es entscheidet, was weniger Reads
        // kostet — bei vielen Slots und wenigen geaenderten ist das
        // Fortschreiben, bei fast vollstaendig ueberschriebenen Bloecken das
        // Neurechnen.
        (BatchOrigin::SteadyState, SourceState::AllValid) => {
            if situation.reads_for_incremental() < situation.reads_for_recompute() {
                Ok(ParityUpdate::Incremental)
            } else {
                Ok(ParityUpdate::Recompute)
            }
        }
    }
}

/// Die Stufen, die ein Stapel akzeptierter Writes durchlaeuft.
///
/// Die Reihenfolge ist die Deklarationsreihenfolge, und sie ist die Regel:
/// Rueckwaerts geht es nur ueber einen Absturz, und `Checkpointed` ist ohne
/// `ParityWritten` davor nicht erreichbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BatchStage {
    /// Die Records sind im Log durable. **Ab hier gilt der Write als
    /// bestaetigt** — alles Weitere schuldet das Array dem Nutzer bereits.
    Logged,
    /// Der alte Inhalt der geaenderten Bereiche liegt vor. Nur beim
    /// Fortschreiben noetig, und nur **vor** dem Ueberschreiben zu haben.
    OldDataRead,
    /// Die neuen Daten stehen durable auf den Data-Members.
    DataWritten,
    /// `P` und, falls vorhanden, `Q` stehen durable.
    ParityWritten,
    /// Der Checkpoint ist geschrieben. Erst jetzt darf der Log-Platz davor
    /// ueberschrieben werden (Abschnitt 5.1).
    Checkpointed,
}

/// Ein Stapel auf seinem Weg durch den Schreibpfad.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteBatch {
    stage: BatchStage,
    method: ParityUpdate,
}

impl WriteBatch {
    /// Ein Stapel, dessen Records gerade durable geworden sind.
    pub fn logged(method: ParityUpdate) -> Self {
        WriteBatch {
            stage: BatchStage::Logged,
            method,
        }
    }

    /// Der Stapel, wie er nach einem Absturz aus dem Log wieder aufgenommen
    /// wird.
    ///
    /// Das Verfahren ist dabei nicht frei waehlbar: Es muss neu gerechnet
    /// werden, weil der alte Inhalt der Data-Members nicht mehr verlaesslich
    /// ist. Ist das Array gleichzeitig degradiert, geht beides nicht — dann
    /// kommt hier ein Fehler und keine geratene Antwort.
    pub fn after_crash(sources: SourceState) -> Result<Self> {
        let method = required_parity_update(
            BatchOrigin::Replay,
            sources,
            BlockSituation {
                contributing_slots: 0,
                written_slots: 0,
                has_parity_q: false,
            },
        )?;
        Ok(WriteBatch {
            stage: BatchStage::Logged,
            method,
        })
    }

    pub fn stage(&self) -> BatchStage {
        self.stage
    }

    pub fn method(&self) -> ParityUpdate {
        self.method
    }

    /// Ob der Log-Platz dieses Stapels freigegeben werden darf.
    pub fn log_space_reusable(&self) -> bool {
        self.stage == BatchStage::Checkpointed
    }

    /// Die Stufe, die als naechste ansteht.
    ///
    /// Beim Neurechnen entfaellt [`BatchStage::OldDataRead`] — es gibt nichts
    /// zu merken, alle Slots werden ohnehin gelesen.
    pub fn next_stage(&self) -> Option<BatchStage> {
        match self.stage {
            BatchStage::Logged => Some(match self.method {
                ParityUpdate::Incremental => BatchStage::OldDataRead,
                ParityUpdate::Recompute => BatchStage::DataWritten,
            }),
            BatchStage::OldDataRead => Some(BatchStage::DataWritten),
            BatchStage::DataWritten => Some(BatchStage::ParityWritten),
            BatchStage::ParityWritten => Some(BatchStage::Checkpointed),
            BatchStage::Checkpointed => None,
        }
    }

    /// Schiebt den Stapel eine Stufe weiter.
    ///
    /// Angenommen wird nur genau die Stufe aus [`WriteBatch::next_stage`]. Jede
    /// andere waere entweder ein Rueckschritt oder ein uebersprungener Schritt,
    /// und beides bedeutet, dass etwas als erledigt gilt, das nicht passiert
    /// ist.
    pub fn advance_to(&mut self, stage: BatchStage) -> Result<()> {
        if stage == BatchStage::Checkpointed && self.stage < BatchStage::ParityWritten {
            return Err(EngineError::CheckpointBeforeParity { stage: self.stage });
        }
        match self.next_stage() {
            Some(expected) if expected == stage => {
                self.stage = stage;
                Ok(())
            }
            _ => Err(EngineError::IllegalTransition {
                from: self.stage,
                to: stage,
            }),
        }
    }
}
