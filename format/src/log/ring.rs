// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Der Ringpuffer der Log-Region, `docs/FORMAT.md` Abschnitt 5.1 und 5.2.
//!
//! Hier liegt der Absturzpfad. Alles, was dieses Modul tut, arbeitet auf einem
//! Byte-Slice — kein I/O, keine Allokation. Wer die Region von der Platte
//! liest, ist die Engine.
//!
//! Die Aufteilung folgt Abschnitt 5.2: [`LogRing::scan`] ist Schritt 1,
//! [`LogRing::newest_checkpoint`] Schritt 2, [`LogRing::replay`] setzt Schritt 3
//! und 4 zusammen und gibt den [`ChainValidator`] den Takt vor.
//!
//! Der Punkt, an dem naive Implementierungen still Daten verlieren, ist
//! Schritt 4: Nach dem ersten Bruch wird **nichts** mehr angewendet, auch kein
//! spaeter folgender, in sich gueltiger Record. Nach einem Absturz liegen im
//! Ringpuffer intakte Records aus einer frueheren Runde.

use super::{ChainBreak, ChainValidator, ChainVerdict, LogRecordHeader, RecordType};
use super::{LOG_HEADER_SIZE, LOG_SECTOR_SIZE};
use crate::error::{FormatError, Result};

/// Ein Record und seine Nutzdaten, so wie der Replay sie liefert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayRecord<'a> {
    /// Offset des Headers innerhalb der Log-Region.
    pub offset: usize,
    pub header: LogRecordHeader,
    pub payload: &'a [u8],
}

/// Warum der Replay aufgehoert hat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayStop {
    /// Die Kette ist gebrochen. Grund kommt vom [`ChainValidator`].
    Chain(ChainBreak),
    /// An dieser Stelle steht kein lesbarer Header — Magic oder Pruefsumme
    /// passen nicht. Ein torn write beim Absturz sieht genau so aus.
    NoHeader { offset: usize },
    /// Der Header behauptet eine Laenge, die ueber das Ende der Region reicht.
    /// Nach Abschnitt 5.1 kann das nicht sein: Passt ein Record nicht mehr,
    /// steht dort ein `Padding`.
    RecordPastEnd { offset: usize },
    /// Der Ringpuffer wurde einmal ganz durchlaufen.
    RingExhausted,
}

/// Lesender Blick auf eine Log-Region.
#[derive(Debug, Clone, Copy)]
pub struct LogRing<'a> {
    region: &'a [u8],
}

impl<'a> LogRing<'a> {
    pub fn new(region: &'a [u8]) -> Result<Self> {
        check_region(region.len())?;
        Ok(LogRing { region })
    }

    pub fn region(&self) -> &'a [u8] {
        self.region
    }

    pub fn sector_count(&self) -> usize {
        self.region.len() / LOG_SECTOR_SIZE
    }

    /// Schritt 1: Alle Sektoren scannen, gueltige Header sammeln.
    ///
    /// Gueltig heisst nach Abschnitt 5.2 nur: Magic stimmt **und**
    /// `header_crc32c` stimmt. Ob der Header an einem echten Record-Anfang
    /// steht, sagt das noch nicht — die Sektoren der Nutzdaten werden
    /// mitgescannt.
    pub fn scan(&self) -> impl Iterator<Item = (usize, LogRecordHeader)> + 'a {
        let region = self.region;
        (0..region.len() / LOG_SECTOR_SIZE).filter_map(move |sector| {
            LogRecordHeader::decode(&region[sector * LOG_SECTOR_SIZE..])
                .ok()
                .map(|header| (sector, header))
        })
    }

    /// Schritt 2: Der `Checkpoint` mit der hoechsten `seq`.
    pub fn newest_checkpoint(&self) -> Option<(usize, LogRecordHeader)> {
        self.scan()
            .filter(|(_, header)| header.record_type == RecordType::Checkpoint)
            .max_by_key(|(_, header)| header.seq)
    }

    /// Schritt 2, Ersatzweg: der gueltige Header mit der niedrigsten `seq`.
    ///
    /// `Padding` bleibt aussen vor — es nimmt an der Kette nicht teil und
    /// taugt deshalb nicht als Anfang.
    pub fn lowest_sequence(&self) -> Option<(usize, LogRecordHeader)> {
        self.scan()
            .filter(|(_, header)| header.record_type != RecordType::Padding)
            .min_by_key(|(_, header)| header.seq)
    }

    /// Schritt 1 bis 4 zusammen: der Replay ab dem richtigen Anfang.
    ///
    /// `generation` kommt aus dem Superblock des Arrays. Der Rueckgabewert ist
    /// ein Iterator ueber die Records, die angewendet werden duerfen — nicht
    /// mehr und nicht weniger.
    pub fn replay(&self, generation: u64) -> Replay<'a> {
        match self.newest_checkpoint() {
            Some((sector, checkpoint)) => {
                let Some(start_seq) = checkpoint.seq.checked_add(1) else {
                    // Auf `seq == u64::MAX` kann kein Nachfolger folgen. Es
                    // gibt nichts anzuwenden.
                    return Replay::exhausted(self.region);
                };
                let offset = self.wrap(sector * LOG_SECTOR_SIZE + checkpoint.on_disk_len());
                Replay::new(self.region, offset, generation, start_seq)
            }
            // Ohne Checkpoint bei der niedrigsten gueltigen `seq` anfangen. Der
            // Record dort gehoert selbst schon zum Replay.
            None => match self.lowest_sequence() {
                Some((sector, header)) => Replay::new(
                    self.region,
                    sector * LOG_SECTOR_SIZE,
                    generation,
                    header.seq,
                ),
                None => Replay::exhausted(self.region),
            },
        }
    }

    fn wrap(&self, offset: usize) -> usize {
        if offset >= self.region.len() {
            0
        } else {
            offset
        }
    }
}

/// Der Vorwaertslauf aus Abschnitt 5.2, Schritt 3 und 4.
///
/// Liefert nur Records, die angewendet werden duerfen. Nach dem ersten Bruch
/// endet der Iterator und [`Replay::stop`] sagt, warum.
#[derive(Debug, Clone)]
pub struct Replay<'a> {
    region: &'a [u8],
    offset: usize,
    /// Verbleibende Sektoren. Begrenzt den Lauf auf eine Runde und macht damit
    /// jede Schleife im Ringpuffer endlich, egal wie kaputt die Daten sind.
    budget: usize,
    chain: ChainValidator,
    stop: Option<ReplayStop>,
}

impl<'a> Replay<'a> {
    fn new(region: &'a [u8], offset: usize, generation: u64, start_seq: u64) -> Self {
        Replay {
            region,
            offset,
            budget: region.len() / LOG_SECTOR_SIZE,
            chain: ChainValidator::new(generation, start_seq),
            stop: None,
        }
    }

    fn exhausted(region: &'a [u8]) -> Self {
        Replay {
            region,
            offset: 0,
            budget: 0,
            chain: ChainValidator::new(0, 0),
            stop: Some(ReplayStop::RingExhausted),
        }
    }

    /// Warum der Lauf geendet hat, sobald er geendet hat.
    pub fn stop(&self) -> Option<ReplayStop> {
        self.stop
    }

    pub fn accepted_count(&self) -> u64 {
        self.chain.accepted_count()
    }

    pub fn last_accepted_seq(&self) -> Option<u64> {
        self.chain.last_accepted_seq()
    }

    fn halt(&mut self, reason: ReplayStop) -> Option<ReplayRecord<'a>> {
        self.stop = Some(reason);
        None
    }
}

impl<'a> Iterator for Replay<'a> {
    type Item = ReplayRecord<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.stop.is_some() {
                return None;
            }
            if self.budget == 0 {
                return self.halt(ReplayStop::RingExhausted);
            }

            let offset = self.offset;
            let Ok(header) = LogRecordHeader::decode(&self.region[offset..]) else {
                return self.halt(ReplayStop::NoHeader { offset });
            };

            // Abschnitt 5.1: `Padding` traegt keine Nutzdaten und keine
            // Sequenznummer, die zur Kette gehoert. Es wird uebersprungen, der
            // naechste Record steht bei Offset 0.
            if header.record_type == RecordType::Padding {
                let to_end = self.region.len() - offset;
                self.budget = self.budget.saturating_sub(to_end / LOG_SECTOR_SIZE);
                self.offset = 0;
                continue;
            }

            let total = header.on_disk_len();
            if total > self.region.len() - offset {
                return self.halt(ReplayStop::RecordPastEnd { offset });
            }
            let payload_start = offset + LOG_HEADER_SIZE;
            let payload = &self.region[payload_start..payload_start + header.payload_len as usize];

            match self.chain.offer(&header, payload) {
                ChainVerdict::StopReplay(reason) => {
                    return self.halt(ReplayStop::Chain(reason));
                }
                ChainVerdict::Accept => {
                    self.budget -= total / LOG_SECTOR_SIZE;
                    self.offset = offset + total;
                    if self.offset >= self.region.len() {
                        self.offset = 0;
                    }
                    return Some(ReplayRecord {
                        offset,
                        header,
                        payload,
                    });
                }
            }
        }
    }
}

/// Ein `Padding`, das vor einem Record noch geschrieben werden muss.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaddingPlacement {
    pub offset: usize,
    pub header: LogRecordHeader,
    /// Bytes, die das Padding belegt: sein Header plus der Rest der Region.
    pub total: usize,
}

/// Wohin ein Record kommt und was vorher noch zu tun ist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// Nur gesetzt, wenn der Record nicht mehr vor das Ende passt.
    pub padding: Option<PaddingPlacement>,
    pub offset: usize,
    /// Bytes, die der Record belegt, aufgerundet auf ganze Sektoren.
    pub total: usize,
    /// Kopf des Ringpuffers nach diesem Record.
    pub next_head: usize,
}

/// Rechnet aus, wohin der naechste Record kommt — ohne ihn zu schreiben.
///
/// Die Platzierungsregel aus Abschnitt 5.1 steht damit an genau einer Stelle.
/// [`LogWriter`] benutzt sie fuer eine Region im Speicher, die Engine fuer eine
/// auf einer Platte, wo die ganze Region nicht in den Arbeitsspeicher passt.
/// Zwei Umsetzungen derselben Regel waeren zwei Gelegenheiten, sie
/// unterschiedlich falsch zu machen.
pub fn plan_append(region_len: usize, head: usize, header: &LogRecordHeader) -> Result<Placement> {
    check_region(region_len)?;
    if head % LOG_SECTOR_SIZE != 0 || head >= region_len {
        return Err(FormatError::InvalidField {
            field: "head",
            reason: "kein Sektoranfang innerhalb der Log-Region",
        });
    }

    let total = header.on_disk_len();
    if total > region_len {
        return Err(FormatError::InvalidField {
            field: "payload_len",
            reason: "Record passt nicht in die Log-Region",
        });
    }

    let mut padding = None;
    let mut offset = head;
    if total > region_len - head {
        // Der Rest der Region ist immer mindestens ein Sektor gross, weil
        // `head` ein Sektoranfang unterhalb der Regionsgroesse ist.
        let to_end = region_len - head;
        let skipped = (to_end - LOG_HEADER_SIZE) as u32;
        padding = Some(PaddingPlacement {
            offset: head,
            header: LogRecordHeader::padding(header.seq, skipped),
            total: to_end,
        });
        offset = 0;
    }

    let mut next_head = offset + total;
    if next_head >= region_len {
        next_head = 0;
    }
    Ok(Placement {
        padding,
        offset,
        total,
        next_head,
    })
}

/// Schreibender Zugriff auf eine Log-Region.
///
/// Kennt nur den Kopf des Ringpuffers. Wann ein Checkpoint faellig ist und was
/// ueberschrieben werden darf, entscheidet die Engine — hier wird nur nach den
/// Regeln aus Abschnitt 5.1 plaziert.
#[derive(Debug)]
pub struct LogWriter<'a> {
    region: &'a mut [u8],
    head: usize,
}

impl<'a> LogWriter<'a> {
    pub fn new(region: &'a mut [u8]) -> Result<Self> {
        check_region(region.len())?;
        Ok(LogWriter { region, head: 0 })
    }

    pub fn head(&self) -> usize {
        self.head
    }

    /// Setzt den Kopf, etwa nach einem Replay auf das Ende des letzten
    /// akzeptierten Records.
    pub fn set_head(&mut self, offset: usize) -> Result<()> {
        if offset % LOG_SECTOR_SIZE != 0 || offset >= self.region.len() {
            return Err(FormatError::InvalidField {
                field: "head",
                reason: "kein Sektoranfang innerhalb der Log-Region",
            });
        }
        self.head = offset;
        Ok(())
    }

    pub fn region(&self) -> &[u8] {
        self.region
    }

    /// Schreibt einen Record und liefert den Offset, an dem er gelandet ist.
    ///
    /// Passt er nicht mehr vor das Ende, kommt davor ein `Padding` und der
    /// Record beginnt bei Offset 0 (Abschnitt 5.1).
    pub fn append(&mut self, header: &LogRecordHeader, payload: &[u8]) -> Result<usize> {
        if payload.len() != header.payload_len as usize {
            return Err(FormatError::InvalidField {
                field: "payload_len",
                reason: "Laenge passt nicht zum Header",
            });
        }
        let plan = plan_append(self.region.len(), self.head, header)?;

        if let Some(padding) = &plan.padding {
            self.put(padding.offset, &padding.header.encode(), &[], padding.total);
        }
        self.put(plan.offset, &header.encode(), payload, plan.total);
        self.head = plan.next_head;
        Ok(plan.offset)
    }

    /// Schreibt Header und Nutzdaten und nullt den Rest des letzten Sektors.
    ///
    /// Das Nullen ist keine Kosmetik: Ohne es bliebe im Rest des Sektors
    /// stehen, was eine frueherere Runde des Ringpuffers dort hinterlassen hat.
    /// Der Scan aus Abschnitt 5.2 sieht jeden Sektor an und faende dort einen
    /// intakten, uralten Header.
    fn put(&mut self, offset: usize, header: &[u8], payload: &[u8], total: usize) {
        let target = &mut self.region[offset..offset + total];
        target[..LOG_HEADER_SIZE].copy_from_slice(header);
        target[LOG_HEADER_SIZE..LOG_HEADER_SIZE + payload.len()].copy_from_slice(payload);
        target[LOG_HEADER_SIZE + payload.len()..].fill(0);
    }
}

fn check_region(len: usize) -> Result<()> {
    if len == 0 {
        return Err(FormatError::InvalidField {
            field: "log_region",
            reason: "leer",
        });
    }
    if len % LOG_SECTOR_SIZE != 0 {
        return Err(FormatError::InvalidField {
            field: "log_region",
            reason: "kein Vielfaches der Sektorgroesse",
        });
    }
    Ok(())
}
