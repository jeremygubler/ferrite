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

/// Woher die Bytes einer Log-Region kommen.
///
/// `format/` bleibt I/O-frei (Regel 2): Diese Eigenschaft **beschreibt** nur,
/// wie man an einen Ausschnitt kommt. Wer eine Platte anfasst, ist die Engine.
///
/// # Warum es das gibt
///
/// Die Log-Region ist so gross wie die Payload-Region ihres Members. Sie am
/// Stueck in den Arbeitsspeicher zu lesen geht bei einem Testarray aus Dateien
/// gut und bricht auf echter Hardware ab — und zwar nicht mit einem Fehler,
/// den man zurueckgeben koennte: Eine fehlgeschlagene Allokation beendet den
/// Prozess. Gemessen auf einer VM mit einem 8-GiB-Log:
///
/// ```text
/// memory allocation of 8588820480 bytes failed
/// ```
///
/// Deshalb laeuft der Absturzpfad ueber diese Eigenschaft. Der Speicherbedarf
/// haengt danach am groessten **Record**, nicht an der Groesse der Platte.
pub trait Region {
    /// Die Laenge der Log-Region in Bytes.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bis zu `want` Bytes ab `offset`.
    ///
    /// Darf **weniger** liefern, wenn die Region vorher endet, und nie mehr.
    /// Der Ausschnitt gilt bis zum naechsten Aufruf — wer ihn laenger braucht,
    /// kopiert ihn.
    fn window(&mut self, offset: usize, want: usize) -> Result<&[u8]>;
}

/// Eine Region, die ohnehin schon im Speicher liegt.
///
/// Kostet nichts: Der Ausschnitt ist ein Teilstueck desselben Slice.
impl Region for &[u8] {
    fn len(&self) -> usize {
        <[u8]>::len(self)
    }

    fn window(&mut self, offset: usize, want: usize) -> Result<&[u8]> {
        let rest = self.get(offset..).ok_or(FormatError::InvalidField {
            field: "offset",
            reason: "hinter dem Ende der Log-Region",
        })?;
        Ok(&rest[..want.min(rest.len())])
    }
}

/// Schritt 1 aus Abschnitt 5.2 ueber eine Region, die nicht im Speicher liegt.
///
/// Ruft `each` fuer jeden Sektor auf, an dem ein gueltiger Header steht.
/// Gueltig heisst: Magic stimmt **und** `header_crc32c` stimmt.
///
/// Ein Sektor, der sich nicht lesen laesst, beendet den Scan nicht — er traegt
/// dann eben keinen Header. Der Scan ist eine Bestandsaufnahme und kein Urteil.
pub fn scan_region<R: Region + ?Sized>(
    region: &mut R,
    mut each: impl FnMut(usize, LogRecordHeader),
) -> Result<()> {
    let sectors = region.len() / LOG_SECTOR_SIZE;
    for sector in 0..sectors {
        let Ok(window) = region.window(sector * LOG_SECTOR_SIZE, LOG_HEADER_SIZE) else {
            continue;
        };
        if let Ok(header) = LogRecordHeader::decode(window) {
            each(sector, header);
        }
    }
    Ok(())
}

/// Schritt 2: Der `Checkpoint` mit der hoechsten `seq`.
pub fn newest_checkpoint_of<R: Region + ?Sized>(
    region: &mut R,
) -> Result<Option<(usize, LogRecordHeader)>> {
    let mut best: Option<(usize, LogRecordHeader)> = None;
    scan_region(region, |sector, header| {
        if header.record_type != RecordType::Checkpoint {
            return;
        }
        let besser = match &best {
            Some((_, found)) => header.seq > found.seq,
            None => true,
        };
        if besser {
            best = Some((sector, header));
        }
    })?;
    Ok(best)
}

/// Schritt 2, Ersatzweg: der gueltige Header mit der niedrigsten `seq`.
///
/// `Padding` bleibt aussen vor — es nimmt an der Kette nicht teil und taugt
/// deshalb nicht als Anfang.
pub fn lowest_sequence_of<R: Region + ?Sized>(
    region: &mut R,
) -> Result<Option<(usize, LogRecordHeader)>> {
    let mut best: Option<(usize, LogRecordHeader)> = None;
    scan_region(region, |sector, header| {
        if header.record_type == RecordType::Padding {
            return;
        }
        let besser = match &best {
            Some((_, found)) => header.seq < found.seq,
            None => true,
        };
        if besser {
            best = Some((sector, header));
        }
    })?;
    Ok(best)
}

/// Wo der Replay anfangen muss.
///
/// Steht hier und nicht zweimal: Der Weg ueber den Speicher und der ueber die
/// Platte muessen dieselbe Stelle waehlen, sonst spielt der eine zurueck, was
/// der andere ueberspringt.
fn replay_start<R: Region + ?Sized>(region: &mut R) -> Result<Option<(usize, u64)>> {
    let region_len = region.len();
    if let Some((sector, checkpoint)) = newest_checkpoint_of(region)? {
        // Auf `seq == u64::MAX` kann kein Nachfolger folgen. Es gibt nichts
        // anzuwenden.
        let Some(start_seq) = checkpoint.seq.checked_add(1) else {
            return Ok(None);
        };
        let offset = sector * LOG_SECTOR_SIZE + checkpoint.on_disk_len();
        let offset = if offset >= region_len { 0 } else { offset };
        return Ok(Some((offset, start_seq)));
    }
    // Ohne Checkpoint bei der niedrigsten gueltigen `seq` anfangen. Der Record
    // dort gehoert selbst schon zum Replay.
    Ok(lowest_sequence_of(region)?.map(|(sector, header)| (sector * LOG_SECTOR_SIZE, header.seq)))
}

/// Ein Record und seine Nutzdaten, wie [`Walk`] sie liefert.
///
/// Die Nutzdaten leihen sich den Puffer der Region und gelten nur bis zum
/// naechsten Aufruf.
#[derive(Debug)]
pub struct WalkRecord<'a> {
    pub offset: usize,
    pub header: LogRecordHeader,
    pub payload: &'a [u8],
}

/// Der Vorwaertslauf aus Abschnitt 5.2 ueber eine [`Region`].
///
/// **Hier steht die Regel, sonst nirgends.** [`Replay`] ist nur die Fassung
/// fuer eine Region, die schon im Speicher liegt, und laeuft ueber dieselbe
/// Mechanik.
///
/// Kein `Iterator`: Der Ausschnitt gilt nur bis zum naechsten Aufruf, und ein
/// `Iterator` verspraeche mehr, als eingehalten werden kann.
#[derive(Debug)]
pub struct Walk<R> {
    region: R,
    offset: usize,
    /// Verbleibende Sektoren. Begrenzt den Lauf auf eine Runde und macht damit
    /// jede Schleife im Ringpuffer endlich, egal wie kaputt die Daten sind.
    budget: usize,
    chain: ChainValidator,
    stop: Option<ReplayStop>,
}

impl<R: Region> Walk<R> {
    /// Setzt den Lauf an die Stelle, die [`replay_start`] bestimmt.
    pub fn start(mut region: R, generation: u64) -> Result<Self> {
        check_region(region.len())?;
        let budget = region.len() / LOG_SECTOR_SIZE;
        match replay_start(&mut region)? {
            Some((offset, start_seq)) => Ok(Walk {
                region,
                offset,
                budget,
                chain: ChainValidator::new(generation, start_seq),
                stop: None,
            }),
            None => Ok(Walk {
                region,
                offset: 0,
                budget: 0,
                chain: ChainValidator::new(0, 0),
                stop: Some(ReplayStop::RingExhausted),
            }),
        }
    }

    pub fn stop(&self) -> Option<ReplayStop> {
        self.stop
    }

    pub fn accepted_count(&self) -> u64 {
        self.chain.accepted_count()
    }

    pub fn last_accepted_seq(&self) -> Option<u64> {
        self.chain.last_accepted_seq()
    }

    /// Gibt die Region zurueck — nach dem Lauf, fuer alles Weitere.
    pub fn into_region(self) -> R {
        self.region
    }

    /// Der naechste Record, der angewendet werden darf.
    ///
    /// `None` heisst Ende; [`Walk::stop`] sagt, warum.
    pub fn next_record(&mut self) -> Option<WalkRecord<'_>> {
        // Erst entscheiden, dann leihen. Die Entscheidung braucht die
        // Nutzdaten, der Rueckgabewert auch — aber ein Ausschnitt, den die
        // Schleife noch haelt, laesst sich in der naechsten Runde nicht
        // wieder anfordern. Also wird er hier fallengelassen und danach ein
        // zweites Mal geholt. Bei einer Region im Speicher kostet das nichts,
        // und eine Region auf der Platte merkt sich ihren letzten Ausschnitt.
        let (offset, header) = loop {
            if self.stop.is_some() {
                return None;
            }
            if self.budget == 0 {
                self.stop = Some(ReplayStop::RingExhausted);
                return None;
            }

            let offset = self.offset;
            let region_len = self.region.len();

            let Some(header) = self
                .region
                .window(offset, LOG_HEADER_SIZE)
                .ok()
                .and_then(|window| LogRecordHeader::decode(window).ok())
            else {
                self.stop = Some(ReplayStop::NoHeader { offset });
                return None;
            };

            // Abschnitt 5.1: `Padding` traegt keine Nutzdaten und keine
            // Sequenznummer, die zur Kette gehoert. Es wird uebersprungen, der
            // naechste Record steht bei Offset 0.
            if header.record_type == RecordType::Padding {
                let to_end = region_len - offset;
                self.budget = self.budget.saturating_sub(to_end / LOG_SECTOR_SIZE);
                self.offset = 0;
                continue;
            }

            let total = header.on_disk_len();
            if total > region_len - offset {
                self.stop = Some(ReplayStop::RecordPastEnd { offset });
                return None;
            }

            let want = LOG_HEADER_SIZE + header.payload_len as usize;
            let verdict = {
                let Self { region, chain, .. } = self;
                let Ok(window) = region.window(offset, want) else {
                    break (offset, header);
                };
                if window.len() < want {
                    self.stop = Some(ReplayStop::RecordPastEnd { offset });
                    return None;
                }
                chain.offer(&header, &window[LOG_HEADER_SIZE..want])
            };

            match verdict {
                ChainVerdict::StopReplay(reason) => {
                    self.stop = Some(ReplayStop::Chain(reason));
                    return None;
                }
                ChainVerdict::Accept => {
                    self.budget -= total / LOG_SECTOR_SIZE;
                    self.offset = offset + total;
                    if self.offset >= region_len {
                        self.offset = 0;
                    }
                    break (offset, header);
                }
            }
        };

        let want = LOG_HEADER_SIZE + header.payload_len as usize;
        let window = self.region.window(offset, want).ok()?;
        Some(WalkRecord {
            offset,
            header,
            payload: &window[LOG_HEADER_SIZE..want],
        })
    }
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
        let mut region = self.region;
        newest_checkpoint_of(&mut region).ok().flatten()
    }

    /// Schritt 2, Ersatzweg: der gueltige Header mit der niedrigsten `seq`.
    ///
    /// `Padding` bleibt aussen vor — es nimmt an der Kette nicht teil und
    /// taugt deshalb nicht als Anfang.
    pub fn lowest_sequence(&self) -> Option<(usize, LogRecordHeader)> {
        let mut region = self.region;
        lowest_sequence_of(&mut region).ok().flatten()
    }

    /// Schritt 1 bis 4 zusammen: der Replay ab dem richtigen Anfang.
    ///
    /// `generation` kommt aus dem Superblock des Arrays. Der Rueckgabewert ist
    /// ein Iterator ueber die Records, die angewendet werden duerfen — nicht
    /// mehr und nicht weniger.
    ///
    /// Innen laeuft [`Walk`], derselbe Code wie auf der Platte. Der Unterschied
    /// ist allein, woher die Bytes kommen.
    pub fn replay(&self, generation: u64) -> Replay<'a> {
        // `LogRing::new` hat die Region schon geprueft, `Walk::start` kann hier
        // also nicht scheitern. Faellt diese Zusage doch einmal, ist ein leerer
        // Replay die sichere Richtung — das Log ist ein Redo-Log, und nichts
        // anzuwenden laesst das Array, wie es ist. Still soll es trotzdem nicht
        // passieren, deshalb die Zusicherung.
        match Walk::start(self.region, generation) {
            Ok(walk) => Replay {
                region: self.region,
                walk,
            },
            Err(error) => {
                debug_assert!(
                    false,
                    "LogRing::new liess eine ungueltige Region durch: {error:?}"
                );
                Replay {
                    region: self.region,
                    walk: Walk {
                        region: self.region,
                        offset: 0,
                        budget: 0,
                        chain: ChainValidator::new(0, 0),
                        stop: Some(ReplayStop::RingExhausted),
                    },
                }
            }
        }
    }
}

/// Der Vorwaertslauf aus Abschnitt 5.2, Schritt 3 und 4 — fuer eine Region,
/// die schon im Speicher liegt.
///
/// Liefert nur Records, die angewendet werden duerfen. Nach dem ersten Bruch
/// endet der Iterator und [`Replay::stop`] sagt, warum.
///
/// # Was hier absichtlich nicht steht
///
/// Die Regel. Die steht in [`Walk`], und diese Fassung laeuft ueber dieselbe
/// Mechanik — sie reicht nur die Nutzdaten aus der Region weiter, statt sie aus
/// einem Puffer zu leihen. Damit kann ein `Iterator` daraus werden, was auf der
/// Platte nicht geht.
///
/// Zwei Umsetzungen des Absturzpfads waeren zwei Gelegenheiten, ihn
/// unterschiedlich falsch zu machen.
#[derive(Debug)]
pub struct Replay<'a> {
    region: &'a [u8],
    walk: Walk<&'a [u8]>,
}

impl<'a> Replay<'a> {
    /// Warum der Lauf geendet hat, sobald er geendet hat.
    pub fn stop(&self) -> Option<ReplayStop> {
        self.walk.stop()
    }

    pub fn accepted_count(&self) -> u64 {
        self.walk.accepted_count()
    }

    pub fn last_accepted_seq(&self) -> Option<u64> {
        self.walk.last_accepted_seq()
    }
}

impl<'a> Iterator for Replay<'a> {
    type Item = ReplayRecord<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        // Der `Walk` entscheidet; die Nutzdaten kommen danach aus der Region
        // selbst. Nur deshalb darf das Ergebnis `'a` tragen und nicht nur die
        // Lebensdauer des Aufrufs.
        let (offset, header) = {
            let record = self.walk.next_record()?;
            (record.offset, record.header)
        };
        let payload_start = offset + LOG_HEADER_SIZE;
        let payload = self
            .region
            .get(payload_start..payload_start + header.payload_len as usize)?;
        Some(ReplayRecord {
            offset,
            header,
            payload,
        })
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
