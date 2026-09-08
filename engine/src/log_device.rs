// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Das Write-Log auf einer echten Platte, `docs/FORMAT.md` Abschnitt 5.
//!
//! `format::LogWriter` arbeitet auf der ganzen Region im Speicher. Auf einer
//! Platte geht das nicht: Die Log-Region ist so gross wie die Payload-Region
//! ihres Members, und die haelt kein Arbeitsspeicher. Also wird hier nur
//! geschrieben, was sich wirklich aendert.
//!
//! Damit die Platzierungsregel aus Abschnitt 5.1 nicht zweimal existiert —
//! einmal fuer den Speicher, einmal fuer die Platte, mit zwei Gelegenheiten,
//! sie unterschiedlich falsch zu machen — rechnet beides mit
//! `format::plan_append`. Dieses Modul fuehrt den Plan nur aus.
//!
//! # Was hier Geld kostet und warum es trotzdem so ist
//!
//! Ein `Padding` am Ende des Ringpuffers nullt den gesamten Rest der Region,
//! nicht nur seinen Header. Das ist teuer und trotzdem noetig: Schritt 1 des
//! Recovery sieht **jeden** Sektor an. Bliebe dort ein intakter `Checkpoint`
//! aus einer frueheren Runde stehen, faende Schritt 2 ihn und begaenne den
//! Replay an der falschen Stelle. Ein gesparter Schreibvorgang gegen einen
//! stillen Datenverlust ist kein Handel.

use ferrite_format::log::ring::{newest_checkpoint_of, scan_region, Region, Walk};
use ferrite_format::log::{LogRecordHeader, RecordType, LOG_SECTOR_SIZE};
use ferrite_format::superblock::{Role, Superblock};
use ferrite_format::{plan_append, FormatError, ReplayStop};

use crate::device::MemberDevice;
use crate::error::{EngineError, Result};

/// Groesse der Bloecke, in denen genullt wird.
///
/// Gross genug, dass das Nullen einer Region nicht an der Anzahl der Aufrufe
/// haengt, klein genug, dass der Puffer nicht auffaellt.
const ZERO_CHUNK: usize = 1 << 20;

/// Groesster Record, den dieses Modul in den Puffer laesst.
///
/// # Warum es eine Grenze braucht
///
/// `payload_len` ist ein `u32` und kommt ungeprueft von der Platte. Ein
/// angefressener Header darf behaupten, sein Record sei vier Gigabyte gross —
/// und wer ihm glaubt und einen Puffer dieser Groesse anlegt, hat den Fehler
/// bloss verschoben, den dieses Modul beseitigen soll.
///
/// Der groesste Record, den der Schreibpfad erzeugen kann, ist ein Header plus
/// eine ublk-Anfrage; `max_io_buf_bytes` steht bei 512 KiB. 64 MiB sind
/// hundertfacher Abstand dazu und trotzdem eine Zahl, die jede Maschine haelt.
/// Was darueber liegt, ist kaputt — und kaputt heisst hier: Der Replay hoert
/// dort auf, statt zu raten.
const MAX_RECORD: usize = 64 << 20;

/// Wieviel auf einmal von der Platte geholt wird.
///
/// Schritt 1 sieht jeden Sektor an. Sektorweise zu lesen waere eine
/// Systemanfrage je 4 KiB; so ist es eine je Megabyte, und die uebrigen 255
/// Sektoren kommen aus demselben Puffer.
const READ_CHUNK: usize = 1 << 20;

/// Die Log-Region auf der Platte, als `Region` fuer `format/`.
///
/// # Der Punkt, um den es geht
///
/// Der Puffer waechst bis zur Groesse **eines Records**, nie bis zur Groesse
/// der Platte. Vorher lag die ganze Region im Arbeitsspeicher; bei einem
/// 8-GiB-Log endete das mit
/// `memory allocation of 8588820480 bytes failed` — und eine fehlgeschlagene
/// Allokation ist kein Fehler, den man zurueckgeben kann, sondern das Ende des
/// Prozesses.
#[derive(Debug)]
pub struct DeviceRegion<'a> {
    device: &'a MemberDevice,
    /// Anfang der Log-Region auf dem Geraet.
    region_offset: u64,
    region_len: usize,
    buffer: Vec<u8>,
    /// Welcher Ausschnitt der Region gerade im Puffer liegt.
    held: Option<(usize, usize)>,
    /// Ein Lesefehler, der unterwegs auftrat.
    ///
    /// `format::Region` kennt nur seine eigenen Fehler, und ein `EIO` als
    /// „hier steht kein Header" zu lesen waere genau die Sorte verschluckter
    /// Fehler, die Regel 5 verbietet: Der Replay hoerte still auf, und niemand
    /// erfuehre, dass die Log-Platte defekt ist. Deshalb wird er hier
    /// aufgehoben und nach dem Lauf abgeholt.
    trouble: Option<EngineError>,
}

impl<'a> DeviceRegion<'a> {
    fn new(device: &'a MemberDevice, region_offset: u64, region_len: usize) -> Self {
        DeviceRegion {
            device,
            region_offset,
            region_len,
            buffer: Vec::new(),
            held: None,
            trouble: None,
        }
    }

    /// Der Lesefehler, falls einer auftrat — danach ist er verbraucht.
    fn take_trouble(&mut self) -> Result<()> {
        match self.trouble.take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Region for DeviceRegion<'_> {
    fn len(&self) -> usize {
        self.region_len
    }

    fn window(&mut self, offset: usize, want: usize) -> ferrite_format::Result<&[u8]> {
        if offset >= self.region_len {
            return Err(FormatError::InvalidField {
                field: "offset",
                reason: "hinter dem Ende der Log-Region",
            });
        }
        // Am Ende der Region gibt es weniger. Das ist erlaubt und kein Fehler —
        // wer mehr braucht, merkt es an der Laenge.
        let want = want.min(self.region_len - offset);
        if want > MAX_RECORD {
            return Err(FormatError::InvalidField {
                field: "payload_len",
                reason: "Record groesser, als dieser Schreibpfad je einen erzeugt",
            });
        }

        // Liegt es schon da? Genau dafuer wird ein ganzes Megabyte geholt und
        // nicht nur das Verlangte: Der Scan fragt danach 255 weitere Sektoren
        // aus demselben Puffer ab, und `Walk` fragt jeden Record zweimal.
        if let Some((start, held)) = self.held {
            if offset >= start && offset + want <= start + held {
                let from = offset - start;
                return Ok(&self.buffer[from..from + want]);
            }
        }

        let chunk = want.max(READ_CHUNK).min(self.region_len - offset);
        self.buffer.resize(chunk, 0);
        if let Err(error) = self.device.read_at(
            self.region_offset + offset as u64,
            &mut self.buffer[..chunk],
        ) {
            self.held = None;
            self.trouble.get_or_insert(error);
            return Err(FormatError::InvalidField {
                field: "log_region",
                reason: "Log-Region nicht lesbar",
            });
        }
        self.held = Some((offset, chunk));
        Ok(&self.buffer[..want])
    }
}

/// Das Write-Log eines Arrays auf seinem Log-Member.
#[derive(Debug)]
pub struct DeviceLog {
    device: MemberDevice,
    /// Offset der Log-Region auf dem Geraet.
    region_offset: u64,
    region_len: usize,
    head: usize,
    next_seq: u64,
    generation: u64,
}

/// Was ein Replay ergeben hat.
///
/// Haelt **nicht** die Region. Sie ist so gross wie die Payload-Region ihres
/// Members; wer sie festhaelt, bindet den Arbeitsspeicher an die Groesse der
/// Platte. Die Records holt [`LogRecovery::records`] bei Bedarf noch einmal von
/// dort — dieselbe Entscheidung, weil dieselbe Regel.
#[derive(Debug)]
pub struct LogRecovery {
    generation: u64,
    /// Kopf des Ringpuffers nach dem letzten akzeptierten Record.
    pub head: usize,
    /// Naechste zu vergebende Sequenznummer.
    pub next_seq: u64,
    /// Warum der Replay aufgehoert hat.
    pub stop: Option<ReplayStop>,
    pub accepted: u64,
    /// Offsets der Record-Header, die nach einem Kettenbruch verworfen wurden.
    ///
    /// Leer, wenn der Replay sauber zu Ende kam — dann gibt es nichts
    /// wegzuraeumen, und wer es trotzdem taete, loeschte gueltige Records.
    discarded: Vec<usize>,
}

impl LogRecovery {
    /// Die akzeptierten Records, in der Reihenfolge ihrer Sequenznummern.
    ///
    /// Laeuft den Replay erneut, diesmal von der Platte. Dieselbe Entscheidung
    /// wie beim Oeffnen, weil dieselbe Mechanik — `Walk` steht an genau einer
    /// Stelle, in `format/`.
    ///
    /// Kein `Iterator`: Die Nutzdaten liegen in einem Puffer, der beim
    /// naechsten Record ueberschrieben wird. Wer sie behalten will, kopiert
    /// sie; wer sie anwendet, tut es sofort.
    pub fn records<'a>(&self, log: &'a DeviceLog) -> Result<Walk<DeviceRegion<'a>>> {
        Walk::start(log.region(), self.generation).map_err(EngineError::Format)
    }
}

impl DeviceLog {
    /// Legt die Log-Region an: nullt sie vollstaendig.
    ///
    /// Ohne das Nullen stuende dort, was die Platte vorher trug. Der Scan aus
    /// Abschnitt 5.2 faende darin Header, die zu keinem Array gehoeren, und der
    /// erste Replay begaenne irgendwo.
    pub fn initialize(device: MemberDevice, superblock: &Superblock) -> Result<Self> {
        let log = Self::new(device, superblock, 0, 1)?;
        log.zero_range(0, log.region_len)?;
        log.device.flush()?;
        Ok(log)
    }

    /// Oeffnet ein bestehendes Log und spielt es zurueck.
    ///
    /// Gibt das Log mit gesetztem Kopf und der naechsten Sequenznummer sowie
    /// das Ergebnis des Replays zurueck. Die akzeptierten Writes anzuwenden ist
    /// Sache des Aufrufers — Schritt 5 aus Abschnitt 5.2 gehoert in den
    /// Schreibpfad, nicht hierher.
    pub fn open(device: MemberDevice, superblock: &Superblock) -> Result<(Self, Box<LogRecovery>)> {
        let mut log = Self::new(device, superblock, 0, 1)?;

        // Kopf und naechste Sequenznummer: erst aus dem Replay, und wenn der
        // nichts hergibt, aus dem **Scan**.
        //
        // Der Replay beginnt hinter dem juengsten Checkpoint und liefert im
        // Normalbetrieb gar nichts — dort ist immer alles gedeckt. Wer daraus
        // den Zustand ableitet, faengt nach jedem Neustart wieder bei Offset 0
        // und `seq == 1` an: Der naechste Record ueberschreibt den aeltesten
        // und traegt eine Nummer, die schon vergeben war. Die Kette ist dann
        // gebrochen, und ein Replay findet nach einem Absturz nichts mehr.
        //
        // Abschnitt 5.1 sagt es klar: `seq` steigt streng monoton **ueber die
        // Lebensdauer des Arrays**. Ein Checkpoint deckt, was vor ihm liegt —
        // er setzt keinen Zaehler zurueck.
        //
        // **Nur wenn der Replay nichts akzeptiert hat.** Hat er etwas
        // akzeptiert, ist die Kette bis dorthin gueltig und wird fortgesetzt —
        // dann gilt weiter, was der Replay sagt. Nach einem Bruch liegen hinter
        // dem Kopf noch Records einer verworfenen Runde; die zu ueberspringen
        // hiesse, den Kopf hinter Muell zu setzen.
        let region_len = log.region_len;
        let mut head = 0;
        let mut next_seq = 1;
        let mut accepted = 0;
        let mut first_accepted = None;

        // Der Lauf leiht sich das Geraet. Alles, was `log` veraendert, kommt
        // danach — deshalb der eigene Block.
        let (stop, discarded) = {
            let mut walk = Walk::start(
                DeviceRegion::new(&log.device, log.region_offset, region_len),
                superblock.generation,
            )
            .map_err(EngineError::Format)?;

            while let Some(record) = walk.next_record() {
                let (offset, header) = (record.offset, record.header);
                let plan = plan_append(region_len, offset, &header).map_err(EngineError::Format)?;
                first_accepted.get_or_insert(offset);
                head = plan.next_head;
                next_seq = header.seq.saturating_add(1);
                accepted += 1;
            }
            let stop = walk.stop();
            let mut region = walk.into_region();

            // **Zuerst der Lesefehler.** Ein `EIO` sieht von aussen aus wie ein
            // Kettenbruch: Der Replay hoert auf, und ohne diese Abfrage hiesse
            // das „hier endet das Log" statt „die Log-Platte antwortet nicht".
            // Das eine raeumt hinterher auf, das andere gehoert gemeldet.
            region.take_trouble()?;

            // Nach einem Bruch liegt hinter dem Kopf eine verworfene Runde. Sie
            // dort liegen zu lassen ist gefaehrlich: Der naechste Record schliesst
            // die Luecke, und dann passen die alten Records **wieder** in die
            // Kette. Ein spaeterer Replay wendet sie an — alte Writes
            // ueberschreiben neuere Daten, und niemand merkt es.
            //
            // `RingExhausted` ist kein Bruch: Da ist der Replay sauber zu Ende
            // gekommen, und wer dort aufraeumte, loeschte gueltige Records.
            // Weggeraeumt werden **nur die Header** der verworfenen Records, nicht
            // die ganze Spanne. Ein Record ohne gueltigen Header landet nie wieder
            // in einer Kette, und ein Sektor je Record ist billig — die Spanne zu
            // nullen koennte bei einem grossen Log-Member Gigabytes bedeuten.
            //
            // Dass die Liste leer bleibt, wenn dort ohnehin nichts liegt, ist der
            // zweite Grund fuer diese Form: Ein `NoHeader` am leeren Ende des Logs
            // ist kein Bruch, sondern der Normalfall, und es sieht genauso aus.
            let keep = match newest_checkpoint_of(&mut region).map_err(EngineError::Format)? {
                Some((sector, _)) => Some(sector * LOG_SECTOR_SIZE),
                None => first_accepted,
            };
            let discarded: Vec<usize> = match stop {
                Some(ReplayStop::RingExhausted) | None => Vec::new(),
                Some(_) => {
                    let mut found = Vec::new();
                    scan_region(&mut region, |sector, _| {
                        let offset = sector * LOG_SECTOR_SIZE;
                        if is_discarded(offset, head, keep) {
                            found.push(offset);
                        }
                    })
                    .map_err(EngineError::Format)?;
                    found
                }
            };

            if accepted == 0 {
                // `Padding` bleibt aussen vor: Es nimmt an der Kette nicht teil
                // und verbraucht keine Sequenznummer (Abschnitt 5.1). Der
                // Record mit der hoechsten `seq` ist damit der zuletzt
                // geschriebene, auch nach einem Umlauf des Ringpuffers.
                let mut newest: Option<(usize, LogRecordHeader)> = None;
                scan_region(&mut region, |sector, header| {
                    if header.record_type == RecordType::Padding {
                        return;
                    }
                    let besser = match &newest {
                        Some((_, gefunden)) => header.seq > gefunden.seq,
                        None => true,
                    };
                    if besser {
                        newest = Some((sector, header));
                    }
                })
                .map_err(EngineError::Format)?;

                if let Some((sector, header)) = newest {
                    let offset = sector * LOG_SECTOR_SIZE;
                    let plan =
                        plan_append(region_len, offset, &header).map_err(EngineError::Format)?;
                    head = plan.next_head;
                    // `saturating_add`: Bei `u64::MAX` bleibt es dabei, und ein
                    // Record mit dieser Nummer beendet die Kette ohnehin
                    // (Abschnitt 5.2).
                    next_seq = header.seq.saturating_add(1);
                }
            }

            region.take_trouble()?;
            (stop, discarded)
        };

        log.head = head;
        log.next_seq = next_seq;
        Ok((
            log,
            Box::new(LogRecovery {
                generation: superblock.generation,
                head,
                next_seq,
                stop,
                accepted,
                discarded,
            }),
        ))
    }

    fn new(
        device: MemberDevice,
        superblock: &Superblock,
        head: usize,
        next_seq: u64,
    ) -> Result<Self> {
        if superblock.role != Role::Log {
            return Err(EngineError::Format(FormatError::InvalidField {
                field: "role",
                reason: "kein Log-Member",
            }));
        }
        superblock
            .fits_on_device(device.size())
            .map_err(EngineError::Format)?;

        let region_len = usize::try_from(superblock.payload_size).map_err(|_| {
            EngineError::Format(FormatError::InvalidField {
                field: "payload_size",
                reason: "Log-Region groesser als der Adressraum",
            })
        })?;
        if region_len % LOG_SECTOR_SIZE != 0 || region_len == 0 {
            return Err(EngineError::Format(FormatError::InvalidField {
                field: "payload_size",
                reason: "kein Vielfaches der Sektorgroesse",
            }));
        }

        Ok(DeviceLog {
            device,
            region_offset: superblock.payload_offset,
            region_len,
            head,
            next_seq,
            generation: superblock.generation,
        })
    }

    /// Die Log-Region als `Region` fuer `format/` — ohne sie zu lesen.
    pub fn region(&self) -> DeviceRegion<'_> {
        DeviceRegion::new(&self.device, self.region_offset, self.region_len)
    }

    pub fn head(&self) -> usize {
        self.head
    }

    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Raeumt eine nach einem Kettenbruch verworfene Runde weg.
    ///
    /// Gibt zurueck, ob etwas geloescht wurde. Ohne Bruch passiert nichts —
    /// dort laegen gueltige Records.
    ///
    /// **Muss laufen, bevor das Log weiterschreibt.** Sonst schliesst der
    /// naechste Record die Luecke, die alten Records passen wieder in die
    /// Kette, und ein spaeterer Replay wendet sie an. Das ist der Grund,
    /// warum diese Funktion existiert; sie ist keine Kosmetik.
    ///
    /// Sie steht nicht in `open`, weil das Oeffnen auch nur lesend passieren
    /// koennen soll — ein Diagnosewerkzeug soll nichts veraendern.
    ///
    /// Kostet einen Schreibvorgang je verworfenem Record plus einen `flush`.
    /// Im Crash-Harness verlaengert das den Lauf spuerbar, weil dort fast jeder
    /// Abbruchpunkt einen Bruch erzeugt. Im Betrieb passiert es einmal nach
    /// einem Absturz — und ein stiller Datenverlust waere teurer.
    pub fn discard_after_break(&mut self, recovery: &LogRecovery) -> Result<bool> {
        if recovery.discarded.is_empty() {
            return Ok(false);
        }
        for offset in &recovery.discarded {
            self.zero_range(*offset, LOG_SECTOR_SIZE)?;
        }
        self.device.flush()?;
        Ok(true)
    }

    pub fn region_len(&self) -> usize {
        self.region_len
    }

    /// Das Geraet, auf dem das Log liegt.
    ///
    /// Der Log-Member gehoert dem Log — wer ihn braucht, bekommt ihn hier und
    /// nicht als zweiten offenen Zugriff daneben.
    pub fn device(&self) -> &MemberDevice {
        &self.device
    }

    /// Schreibt einen `Write`-Record und liefert seinen Offset in der Region.
    ///
    /// Bestaetigt ist er, wenn diese Funktion zurueckkehrt: Sie flusht, bevor
    /// sie das tut. Genau das ist die Zusage aus Abschnitt 5 — ein Write gilt,
    /// sobald sein Record durable ist.
    pub fn append_write(
        &mut self,
        slot_index: u16,
        target_offset: u64,
        payload: &[u8],
    ) -> Result<usize> {
        let mut header = LogRecordHeader::write(self.next_seq, slot_index, target_offset, payload);
        header.generation = self.generation;
        self.append(&header, payload)
    }

    /// Schreibt einen `Checkpoint`: Alles bis `seq` liegt auf den Data-Members
    /// und in der Paritaet.
    pub fn append_checkpoint(&mut self) -> Result<usize> {
        let mut header = LogRecordHeader::checkpoint(self.next_seq);
        header.generation = self.generation;
        self.append(&header, &[])
    }

    /// Fuehrt den Plan aus `format::plan_append` aus.
    pub fn append(&mut self, header: &LogRecordHeader, payload: &[u8]) -> Result<usize> {
        if payload.len() != header.payload_len as usize {
            return Err(EngineError::Format(FormatError::InvalidField {
                field: "payload_len",
                reason: "Laenge passt nicht zum Header",
            }));
        }
        let plan = plan_append(self.region_len, self.head, header).map_err(EngineError::Format)?;

        // Zuerst das Padding. Es steht vor dem Record und muss dort stehen,
        // bevor der Record gilt — sonst zeigt die Kette ueber eine Luecke.
        if let Some(padding) = &plan.padding {
            let mut sector = [0u8; LOG_SECTOR_SIZE];
            sector[..padding.header.encode().len()].copy_from_slice(&padding.header.encode());
            self.write_region(padding.offset, &sector)?;
            // Der Rest des Paddings ist Null — siehe Modulkopf.
            self.zero_range(
                padding.offset + LOG_SECTOR_SIZE,
                padding.total - LOG_SECTOR_SIZE,
            )?;
        }

        // Header und Nutzdaten in einen sektorgrossen Puffer, den Rest genullt.
        // Ein einziger Schreibvorgang: Zwei waeren zwei Gelegenheiten fuer
        // einen Absturz mittendrin.
        let mut buffer = vec![0u8; plan.total];
        let encoded = header.encode();
        buffer[..encoded.len()].copy_from_slice(&encoded);
        buffer[encoded.len()..encoded.len() + payload.len()].copy_from_slice(payload);
        self.write_region(plan.offset, &buffer)?;

        self.device.flush()?;
        self.head = plan.next_head;
        self.next_seq = header.seq.saturating_add(1);
        Ok(plan.offset)
    }

    /// Liest die ganze Region. Fuer Tests und Diagnose, nicht fuer den
    /// laufenden Betrieb.
    pub fn read_region(&self) -> Result<Vec<u8>> {
        let mut region = vec![0u8; self.region_len];
        self.device.read_at(self.region_offset, &mut region)?;
        Ok(region)
    }

    fn write_region(&self, offset: usize, data: &[u8]) -> Result<()> {
        if offset + data.len() > self.region_len {
            return Err(EngineError::BeyondDevice {
                offset: offset as u64,
                len: data.len() as u64,
                size: self.region_len as u64,
            });
        }
        self.device
            .write_at(self.region_offset + offset as u64, data)
    }

    fn zero_range(&self, offset: usize, len: usize) -> Result<()> {
        let zeros = vec![0u8; ZERO_CHUNK.min(len.max(1))];
        let mut done = 0;
        while done < len {
            let chunk = zeros.len().min(len - done);
            self.write_region(offset + done, &zeros[..chunk])?;
            done += chunk;
        }
        Ok(())
    }
}

/// Liegt dieser Offset im verworfenen Teil des Ringpuffers?
///
/// Behalten wird der Bereich `keep .. head` im Ring — dort liegen der
/// Checkpoint, der den naechsten Replay startet, und die akzeptierten Records.
/// Alles andere gehoert zu einer Runde, die der Bruch verworfen hat.
///
/// `keep == None` heisst: Es gibt nichts zu behalten, der ganze Ring ist
/// verworfen. `keep == Some(head)` heisst das Gegenteil — der Ring ist voll mit
/// Behaltenem, und nichts ist wegzuraeumen.
fn is_discarded(offset: usize, head: usize, keep: Option<usize>) -> bool {
    match keep {
        None => true,
        Some(keep) if keep == head => false,
        Some(keep) if head < keep => offset >= head && offset < keep,
        // Der behaltene Bereich laeuft ueber das Ende des Rings hinweg.
        Some(keep) => offset >= head || offset < keep,
    }
}

#[cfg(test)]
mod tests {
    use super::is_discarded;

    #[test]
    fn nothing_to_keep_discards_everything() {
        assert!(is_discarded(0, 0, None));
        assert!(is_discarded(4096, 0, None));
    }

    #[test]
    fn a_full_ring_of_kept_records_discards_nothing() {
        assert!(!is_discarded(0, 8192, Some(8192)));
        assert!(!is_discarded(4096, 8192, Some(8192)));
    }

    #[test]
    fn the_span_between_head_and_keep_is_discarded() {
        // Behalten laeuft von 12288 bis zum Kopf bei 4096, also ueber das Ende
        // hinweg. Verworfen ist genau das dazwischen: 4096 und 8192.
        assert!(is_discarded(4096, 4096, Some(12288)));
        assert!(is_discarded(8192, 4096, Some(12288)));
        assert!(!is_discarded(12288, 4096, Some(12288)));
        assert!(!is_discarded(0, 4096, Some(12288)));
    }

    #[test]
    fn a_span_wrapping_around_the_end_is_handled() {
        // Behalten laeuft ueber das Ende: keep bei 4096, Kopf bei 12288.
        // Verworfen ist damit 12288.. und ..4096.
        assert!(is_discarded(12288, 12288, Some(4096)));
        assert!(is_discarded(0, 12288, Some(4096)));
        assert!(!is_discarded(4096, 12288, Some(4096)));
        assert!(!is_discarded(8192, 12288, Some(4096)));
    }
}
