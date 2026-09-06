// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Der Broker selbst: Meldung herein, Reparatur hinaus.

use std::sync::{Arc, Mutex};

use ferrite_engine::{ArrayWriter, EngineError, Repair};

use crate::btrfs::parse_scrub_error;
use crate::damage::{coalesce, DamageReport, SlotMap};
use crate::error::{BrokerError, Result};

/// Was eine Kernelzeile ergeben hat.
///
/// Drei Faelle und nicht `Option`, weil der mittlere sonst unsichtbar bliebe:
/// Eine Meldung ueber ein fremdes Geraet ist etwas anderes als eine Zeile ohne
/// Meldung. Wer beides zu `None` zusammenzieht, merkt einen Tippfehler in der
/// Zuordnung nie — der Broker bliebe still, und still heisst hier: repariert
/// nichts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classified {
    /// Keine btrfs-Meldung mit Geraeteoffset.
    NotOurs,
    /// Eine Meldung, aber ueber ein Geraet, das zu diesem Array nicht gehoert.
    ForeignDevice,
    /// Eine Meldung ueber einen Data-Slot dieses Arrays.
    Damage(DamageReport),
}

/// Was der Broker bisher getan hat.
///
/// Zaehler und keine Logzeilen: Wer wissen will, ob die Selbstheilung
/// arbeitet, will eine Zahl sehen und keinen Text durchsuchen. `refused` ist
/// dabei der wichtigste Wert — er ist nicht null, wenn mehr als eine Quelle
/// beschaedigt ist, und das ist der Punkt, an dem ein Mensch hinsehen muss.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RepairStats {
    /// btrfs-Meldungen mit Geraeteoffset, die gesehen wurden.
    pub seen: u64,
    /// Davon ueber Geraete, die nicht zu diesem Array gehoeren.
    pub foreign: u64,
    /// Bereiche, die aus der Redundanz zurueckgeholt wurden.
    pub repaired: u64,
    /// Wieviele Bytes das waren.
    pub bytes_repaired: u64,
    /// Bereiche, die schon in Ordnung waren.
    pub already_intact: u64,
    /// Bereiche, die dem Rebuild ueberlassen wurden.
    pub left_to_rebuild: u64,
    /// Bereiche, deren Inhalt sich nicht eindeutig bestimmen liess.
    pub refused: u64,
}

impl RepairStats {
    /// Gibt es einen Befund, den ein Mensch ansehen muss?
    ///
    /// Als eigene Frage, damit ein Aufrufer sie stellen muss, statt sie zu
    /// uebersehen — dieselbe Ueberlegung wie bei `Recovered::had_loss`.
    pub fn needs_attention(&self) -> bool {
        self.refused > 0
    }
}

/// Nimmt Befunde von btrfs entgegen und holt den Inhalt aus der Redundanz
/// zurueck.
///
/// Haelt den Schreibpfad geteilt: Der Broker repariert, waehrend das
/// ublk-Target Gast-I/O bedient. Die Sperre wird **pro Meldung** genommen und
/// wieder abgegeben — eine Reparatur ueber tausend Bereiche darf das
/// Dateisystem nicht fuer ihre ganze Dauer anhalten.
#[derive(Debug)]
pub struct RepairBroker {
    writer: Arc<Mutex<ArrayWriter>>,
    slots: SlotMap,
    fallback_len: usize,
    stats: RepairStats,
}

impl RepairBroker {
    /// `fallback_len` ist die Laenge, mit der ein Bereich repariert wird,
    /// dessen Meldung keine nennt — bei Metadaten ist das der Regelfall. Zu
    /// waehlen ist die Sektorgroesse des Dateisystems; zu gross ist harmlos
    /// (der Rest wird als `AlreadyIntact` erkannt und nicht geschrieben), zu
    /// klein liesse einen Rest Rost stehen.
    pub fn new(writer: Arc<Mutex<ArrayWriter>>, slots: SlotMap, fallback_len: usize) -> Self {
        RepairBroker {
            writer,
            slots,
            fallback_len,
            stats: RepairStats::default(),
        }
    }

    pub fn stats(&self) -> &RepairStats {
        &self.stats
    }

    pub fn slots(&self) -> &SlotMap {
        &self.slots
    }

    /// Liest eine Kernelzeile und ordnet sie einem Data-Slot zu.
    ///
    /// Zaehlt dabei mit: Jede erkannte Meldung erhoeht `seen`, jede ueber ein
    /// fremdes Geraet zusaetzlich `foreign`.
    pub fn classify(&mut self, line: &str) -> Classified {
        let Some(error) = parse_scrub_error(line) else {
            return Classified::NotOurs;
        };
        self.stats.seen += 1;

        let Some(slot_index) = self.slots.slot_of(error.device) else {
            self.stats.foreign += 1;
            return Classified::ForeignDevice;
        };

        Classified::Damage(DamageReport {
            slot_index,
            offset: error.physical,
            len: error.length.map_or(self.fallback_len, |len| len as usize),
        })
    }

    /// Arbeitet eine Meldung ab.
    ///
    /// Zwei Ausgaenge werden hier **nicht** zum Fehler gemacht, obwohl sie
    /// welche zurueckgeben: Eine mehrdeutige Rekonstruktion und ein fehlendes
    /// ParityQ heissen, dass dieser Bereich nicht sicher bestimmbar ist — sie
    /// heissen nicht, dass die naechste Meldung es auch nicht waere. Sie
    /// werden gezaehlt und weitergereicht, damit der Aufrufer weiterarbeiten
    /// kann und trotzdem erfaehrt, dass etwas offen blieb.
    pub fn repair(&mut self, report: DamageReport) -> Result<Repair> {
        let outcome = {
            let mut writer = self
                .writer
                .lock()
                .map_err(|_| BrokerError::WriterPoisoned)?;
            writer.repair(report.slot_index, report.offset, report.len)
        };

        match outcome {
            Ok(Repair::Written { len }) => {
                self.stats.repaired += 1;
                self.stats.bytes_repaired += len as u64;
            }
            Ok(Repair::AlreadyIntact) => self.stats.already_intact += 1,
            Ok(Repair::LeftToRebuild) => self.stats.left_to_rebuild += 1,
            Err(
                EngineError::AmbiguousReconstruction { .. }
                | EngineError::NoSecondSource
                | EngineError::CannotRebuild { .. },
            ) => self.stats.refused += 1,
            Err(_) => {}
        }

        outcome.map_err(BrokerError::Engine)
    }

    /// Arbeitet einen Schwung Meldungen ab.
    ///
    /// Fasst sie vorher zusammen und laeuft weiter, wenn eine scheitert: Ein
    /// Bereich, dessen Inhalt sich nicht bestimmen laesst, ist kein Grund, die
    /// uebrigen stehen zu lassen. Zurueck kommt zu jeder Meldung ihr Ausgang,
    /// damit nichts unbemerkt bleibt.
    #[allow(clippy::type_complexity)]
    pub fn repair_all(
        &mut self,
        reports: &mut Vec<DamageReport>,
    ) -> Vec<(DamageReport, Result<Repair>)> {
        coalesce(reports);
        reports
            .iter()
            .map(|report| (*report, self.repair(*report)))
            .collect()
    }
}
