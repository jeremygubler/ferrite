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

use ferrite_format::log::{LogRecordHeader, LOG_SECTOR_SIZE};
use ferrite_format::superblock::{Role, Superblock};
use ferrite_format::{plan_append, FormatError, LogRing, Replay, ReplayStop};

use crate::device::MemberDevice;
use crate::error::{EngineError, Result};

/// Groesse der Bloecke, in denen genullt wird.
///
/// Gross genug, dass das Nullen einer Region nicht an der Anzahl der Aufrufe
/// haengt, klein genug, dass der Puffer nicht auffaellt.
const ZERO_CHUNK: usize = 1 << 20;

/// Das Write-Log eines Arrays auf seinem Log-Member.
#[derive(Debug)]
pub struct DeviceLog<'a> {
    device: &'a MemberDevice,
    /// Offset der Log-Region auf dem Geraet.
    region_offset: u64,
    region_len: usize,
    head: usize,
    next_seq: u64,
    generation: u64,
}

/// Was ein Replay ergeben hat.
///
/// Haelt die gelesene Region, weil die Records auf sie zeigen. Das ist der eine
/// Punkt, an dem die ganze Log-Region im Arbeitsspeicher liegt — beim Mounten,
/// einmal. Fuer den laufenden Betrieb gilt das nicht.
#[derive(Debug)]
pub struct LogRecovery {
    region: Vec<u8>,
    generation: u64,
    /// Kopf des Ringpuffers nach dem letzten akzeptierten Record.
    pub head: usize,
    /// Naechste zu vergebende Sequenznummer.
    pub next_seq: u64,
    /// Warum der Replay aufgehoert hat.
    pub stop: Option<ReplayStop>,
    pub accepted: u64,
}

impl LogRecovery {
    /// Die akzeptierten Records, in der Reihenfolge ihrer Sequenznummern.
    ///
    /// Laeuft den Replay erneut ueber die bereits gelesene Region — dieselbe
    /// Entscheidung wie beim ersten Mal, ohne die Platte noch einmal
    /// anzufassen.
    pub fn records(&self) -> Result<Replay<'_>> {
        // Die Region wurde beim Oeffnen schon einmal angenommen, ein Fehler ist
        // hier also nicht zu erwarten. Trotzdem zurueckgegeben und nicht
        // weggeworfen: Regel 5 kennt keine Ausnahme fuer Faelle, die man fuer
        // unmoeglich haelt — genau die sind es, die spaeter still danebengehen.
        Ok(LogRing::new(&self.region)
            .map_err(EngineError::Format)?
            .replay(self.generation))
    }
}

impl<'a> DeviceLog<'a> {
    /// Legt die Log-Region an: nullt sie vollstaendig.
    ///
    /// Ohne das Nullen stuende dort, was die Platte vorher trug. Der Scan aus
    /// Abschnitt 5.2 faende darin Header, die zu keinem Array gehoeren, und der
    /// erste Replay begaenne irgendwo.
    pub fn initialize(device: &'a MemberDevice, superblock: &Superblock) -> Result<Self> {
        let log = Self::new(device, superblock, 0, 1)?;
        log.zero_range(0, log.region_len)?;
        device.flush()?;
        Ok(log)
    }

    /// Oeffnet ein bestehendes Log und spielt es zurueck.
    ///
    /// Gibt das Log mit gesetztem Kopf und der naechsten Sequenznummer sowie
    /// das Ergebnis des Replays zurueck. Die akzeptierten Writes anzuwenden ist
    /// Sache des Aufrufers — Schritt 5 aus Abschnitt 5.2 gehoert in den
    /// Schreibpfad, nicht hierher.
    pub fn open(
        device: &'a MemberDevice,
        superblock: &Superblock,
    ) -> Result<(Self, Box<LogRecovery>)> {
        let mut log = Self::new(device, superblock, 0, 1)?;

        let mut region = vec![0u8; log.region_len];
        device.read_at(log.region_offset, &mut region)?;

        let ring = LogRing::new(&region).map_err(EngineError::Format)?;
        let mut replay = ring.replay(superblock.generation);

        // Der Kopf kommt hinter den letzten akzeptierten Record. Steht dort
        // nichts, beginnt das Log bei null.
        let mut head = 0;
        let mut next_seq = 1;
        let mut accepted = 0;
        for record in replay.by_ref() {
            let plan = plan_append(log.region_len, record.offset, &record.header)
                .map_err(EngineError::Format)?;
            head = plan.next_head;
            next_seq = record.header.seq.saturating_add(1);
            accepted += 1;
        }

        // Vor dem Verschieben von `region` abfragen: `replay` leiht sie noch.
        let stop = replay.stop();

        log.head = head;
        log.next_seq = next_seq;
        Ok((
            log,
            Box::new(LogRecovery {
                region,
                generation: superblock.generation,
                head,
                next_seq,
                stop,
                accepted,
            }),
        ))
    }

    fn new(
        device: &'a MemberDevice,
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

    pub fn head(&self) -> usize {
        self.head
    }

    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    pub fn region_len(&self) -> usize {
        self.region_len
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
