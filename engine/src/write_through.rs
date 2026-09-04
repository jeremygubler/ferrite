// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Der Schreibpfad auf echte Platten, `docs/FORMAT.md` Abschnitt 5.
//!
//! # Warum nur Write-Through
//!
//! Abschnitt 5 kennt zwei Betriebsarten. Im **Write-Back** gilt ein Write,
//! sobald sein Log-Record durable ist; Data-Member und Paritaet ziehen danach
//! nach. Im **Write-Through** wird erst bestaetigt, wenn beides steht.
//!
//! Hier steht nur Write-Through, und das ist eine bewusste Entscheidung, keine
//! halbe Sache: Write-Back ist nach Abschnitt 5.3 nur erlaubt, wenn der
//! Flush-Test `Honest` ergibt — und das tut er auf keiner Maschine, die dieses
//! Projekt bisher gesehen hat (siehe `flush.rs`). Ein Write-Back-Pfad waere
//! damit Code im Schreibpfad, den niemand ausfuehren kann. Er braucht ausserdem
//! einen Index ueber die noch nicht angewendeten Writes, damit ein Read sie
//! sieht — Zustand, dessen Absturzverhalten erst das Crash-Harness aus
//! Meilenstein 3 pruefen kann.
//!
//! Das Log ist deshalb nicht ueberfluessig: Es traegt den Absturzpfad. Faellt
//! der Strom zwischen Data-Member und Paritaet aus, sagt der Replay, welche
//! Bereiche neu zu rechnen sind.
//!
//! # Die Reihenfolge
//!
//! ```text
//! Logged → [OldDataRead] → DataWritten → ParityWritten → Checkpointed
//! ```
//!
//! [`WriteBatch`] gibt sie vor und laesst keine Abkuerzung zu. Der Checkpoint
//! kommt zuletzt, weil er Log-Platz freigibt: Waere die Paritaet dann noch
//! nicht durable, rekonstruierte das Array nach dem naechsten Plattenausfall
//! Muell.
//!
//! # Paritaet ueber Bytebereiche, nicht ueber Bloecke
//!
//! `P[i] = ⊕ⱼ Dⱼ[i]` gilt fuer jedes einzelne Byte. Ein Write, der die Bytes
//! `a..b` eines Slots aendert, aendert genau die Bytes `a..b` der Paritaet —
//! Bloecke sind eine Einheit fuer die Buendelung, nicht fuer die Korrektheit.
//! Deshalb wird hier nur der beruehrte Bereich angefasst.

use ferrite_format::superblock::{Role, Superblock};
use ferrite_parity::{compute_p, compute_q, gf, Slot};

use crate::device::MemberDevice;
use crate::error::{EngineError, Result};
use crate::log_device::DeviceLog;
use crate::write_path::{
    required_parity_update, BatchOrigin, BatchStage, BlockSituation, ParityUpdate, SourceState,
    WriteBatch,
};

/// Ein Member samt der Lage seiner Payload-Region.
#[derive(Debug)]
pub struct Member {
    device: MemberDevice,
    payload_offset: u64,
    payload_size: u64,
}

impl Member {
    /// Uebernimmt Lage und Groesse aus dem Superblock des Members.
    pub fn new(device: MemberDevice, superblock: &Superblock) -> Result<Self> {
        superblock
            .fits_on_device(device.size())
            .map_err(EngineError::Format)?;
        Ok(Member {
            device,
            payload_offset: superblock.payload_offset,
            payload_size: superblock.payload_size,
        })
    }

    pub fn payload_size(&self) -> u64 {
        self.payload_size
    }

    /// Liest aus der Payload-Region, **zero-extended**.
    ///
    /// Jenseits des Endes dieses Members kommen Nullbytes, kein Fehler. Das ist
    /// die Kerninvariante und nicht Bequemlichkeit: Ohne sie muesste ein Array
    /// mit gemischten Plattengroessen an jedem kurzen Member abbrechen, und die
    /// Paritaet waere fuer den Rest nicht bildbar.
    fn read_extended(&self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        buffer.fill(0);
        let Some(available) = self.payload_size.checked_sub(offset) else {
            return Ok(());
        };
        let readable = (available as usize).min(buffer.len());
        if readable == 0 {
            return Ok(());
        }
        self.device
            .read_at(self.payload_offset + offset, &mut buffer[..readable])
    }

    /// Schreibt in die Payload-Region. Ueber ihr Ende hinaus wird abgelehnt.
    fn write_within(&self, offset: u64, data: &[u8]) -> Result<()> {
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(EngineError::OffsetOverflow {
                offset,
                len: data.len() as u64,
            })?;
        if end > self.payload_size {
            return Err(EngineError::BeyondDevice {
                offset,
                len: data.len() as u64,
                size: self.payload_size,
            });
        }
        self.device.write_at(self.payload_offset + offset, data)
    }

    fn flush(&self) -> Result<()> {
        self.device.flush()
    }
}

/// Der Schreibpfad eines Arrays.
///
/// Haelt alle Members offen. Ein Write geht durch das Log, dann auf den
/// Data-Member, dann in die Paritaet, dann in einen Checkpoint — und erst dann
/// zurueck an den Aufrufer.
#[derive(Debug)]
pub struct ArrayWriter {
    log: DeviceLog,
    /// Nach `slot_index` geordnet, Laenge `data_slot_count`.
    data: Vec<Member>,
    parity_p: Member,
    parity_q: Option<Member>,
    /// Ob alle Data-Members an ihren Bloecken brauchbare Daten liefern.
    ///
    /// Vorerst ein Wert fuer das ganze Array. Die feinere Betrachtung je Block
    /// braucht `rebuild_progress`, und die gehoert zum Rebuild.
    sources: SourceState,
}

impl ArrayWriter {
    /// Setzt den Schreibpfad aus bereits geoeffneten Members zusammen.
    ///
    /// `data` muss nach `slot_index` geordnet und vollstaendig sein — eine
    /// Luecke hiesse, Paritaet ueber eine unvollstaendige Menge zu rechnen. Das
    /// faellt beim Assemble bereits auf; hier steht es noch einmal, weil dieser
    /// Typ auch ohne Assemble gebaut werden kann.
    pub fn new(
        log: DeviceLog,
        data: Vec<Member>,
        parity_p: Member,
        parity_q: Option<Member>,
    ) -> Result<Self> {
        if data.is_empty() || data.len() > ferrite_parity::MAX_DATA_SLOTS as usize {
            return Err(EngineError::Format(
                ferrite_format::FormatError::InvalidField {
                    field: "data_slot_count",
                    reason: "kein Data-Slot oder mehr als das Format erlaubt",
                },
            ));
        }

        // Regel 6 aus Abschnitt 2.1, hier noch einmal: Ein Parity-Member, der
        // kuerzer ist als der laengste Data-Member, hat fuer die Offsets
        // dahinter keine Paritaet.
        let longest = data
            .iter()
            .map(|member| member.payload_size)
            .max()
            .unwrap_or(0);
        for parity in [Some(&parity_p), parity_q.as_ref()].into_iter().flatten() {
            if parity.payload_size < longest {
                return Err(EngineError::Format(
                    ferrite_format::FormatError::InvalidField {
                        field: "payload_size",
                        reason: "Parity-Member kuerzer als der laengste Data-Member",
                    },
                ));
            }
        }

        Ok(ArrayWriter {
            log,
            data,
            parity_p,
            parity_q,
            sources: SourceState::AllValid,
        })
    }

    pub fn data_slot_count(&self) -> u8 {
        self.data.len() as u8
    }

    pub fn has_parity_q(&self) -> bool {
        self.parity_q.is_some()
    }

    pub fn log(&self) -> &DeviceLog {
        &self.log
    }

    /// Meldet, dass mindestens ein Data-Member gerade nicht brauchbar ist.
    ///
    /// Aendert das Verfahren der Paritaetsaktualisierung: Neurechnen ginge
    /// nicht, weil der Inhalt des fehlenden Members fehlt.
    pub fn set_sources(&mut self, sources: SourceState) {
        self.sources = sources;
    }

    /// Liest aus einem Data-Slot.
    ///
    /// Direkt vom Member. Im Write-Through steht dort immer der aktuelle
    /// Inhalt — es gibt keine bestaetigten Writes, die nur im Log liegen.
    pub fn read(&self, slot_index: u16, offset: u64, buffer: &mut [u8]) -> Result<()> {
        let member = self.slot(slot_index)?;
        let end = offset
            .checked_add(buffer.len() as u64)
            .ok_or(EngineError::OffsetOverflow {
                offset,
                len: buffer.len() as u64,
            })?;
        if end > member.payload_size {
            return Err(EngineError::BeyondDevice {
                offset,
                len: buffer.len() as u64,
                size: member.payload_size,
            });
        }
        member
            .device
            .read_at(member.payload_offset + offset, buffer)
    }

    /// Schreibt in einen Data-Slot und zieht die Paritaet nach.
    ///
    /// Kehrt erst zurueck, wenn Data-Member **und** Paritaet durable sind. Das
    /// ist die Zusage des Write-Through-Modus aus Abschnitt 5.3.
    pub fn write(&mut self, slot_index: u16, offset: u64, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let member = self.slot(slot_index)?;
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(EngineError::OffsetOverflow {
                offset,
                len: data.len() as u64,
            })?;
        if end > member.payload_size {
            return Err(EngineError::BeyondDevice {
                offset,
                len: data.len() as u64,
                size: member.payload_size,
            });
        }

        // 1. Der Record ins Log. Ab hier ueberlebt der Write einen Absturz,
        //    auch wenn keiner der folgenden Schritte mehr passiert.
        self.log.append_write(slot_index, offset, data)?;

        let method = self.choose_method(BatchOrigin::SteadyState, data.len())?;
        let mut batch = WriteBatch::logged(method);

        // 2. Beim Fortschreiben den alten Inhalt lesen — **vor** dem
        //    Ueberschreiben, sonst ist er weg.
        let old = if method == ParityUpdate::Incremental {
            let mut old = vec![0u8; data.len()];
            self.slot(slot_index)?.read_extended(offset, &mut old)?;
            batch.advance_to(BatchStage::OldDataRead)?;
            Some(old)
        } else {
            None
        };

        // 3. Auf den Data-Member.
        let member = self.slot(slot_index)?;
        member.write_within(offset, data)?;
        member.flush()?;
        batch.advance_to(BatchStage::DataWritten)?;

        // 4. Paritaet.
        match method {
            ParityUpdate::Incremental => {
                let old = old.ok_or(EngineError::CannotUpdateParity)?;
                self.update_parity_incremental(slot_index, offset, &old, data)?;
            }
            ParityUpdate::Recompute => self.recompute_parity(offset, data.len())?,
        }
        batch.advance_to(BatchStage::ParityWritten)?;

        // 5. Erst jetzt der Checkpoint. Er gibt Log-Platz frei, und was er
        //    freigibt, muss auf den Platten stehen.
        self.log.append_checkpoint()?;
        batch.advance_to(BatchStage::Checkpointed)?;
        Ok(())
    }

    /// Welches Verfahren fuer diesen Bereich korrekt und guenstiger ist.
    ///
    /// Die Entscheidung faellt `write_path::required_parity_update` — sie steht
    /// dort samt Begruendung und wird hier nicht noch einmal getroffen.
    fn choose_method(&self, origin: BatchOrigin, len: usize) -> Result<ParityUpdate> {
        let situation = BlockSituation {
            contributing_slots: self.contributing_slots(len),
            written_slots: 1,
            has_parity_q: self.parity_q.is_some(),
        };
        required_parity_update(origin, self.sources, situation)
    }

    /// Data-Slots, die an diesem Bereich ueberhaupt Daten tragen.
    ///
    /// Members, die vor dem Bereich enden, liefern per Zero-Extension Nullen —
    /// die muss niemand von der Platte holen.
    fn contributing_slots(&self, _len: usize) -> u32 {
        self.data.len() as u32
    }

    fn slot(&self, slot_index: u16) -> Result<&Member> {
        self.data
            .get(usize::from(slot_index))
            .ok_or(EngineError::Format(
                ferrite_format::FormatError::InvalidField {
                    field: "slot_index",
                    reason: "kein solcher Data-Slot",
                },
            ))
    }

    /// `P' = P ^ D_alt ^ D_neu`, entsprechend `Q' = Q ^ gʲ·(D_alt ^ D_neu)`.
    ///
    /// Beides ueber genau den geaenderten Bytebereich. Der Beitrag aller
    /// anderen Slots steckt bereits in der alten Paritaet und aendert sich
    /// nicht — das ist der Grund, warum dieses Verfahren auch im degradierten
    /// Betrieb funktioniert.
    fn update_parity_incremental(
        &self,
        slot_index: u16,
        offset: u64,
        old: &[u8],
        new: &[u8],
    ) -> Result<()> {
        let mut delta = vec![0u8; new.len()];
        for (index, byte) in delta.iter_mut().enumerate() {
            *byte = old[index] ^ new[index];
        }

        let mut buffer = vec![0u8; new.len()];
        self.parity_p.read_extended(offset, &mut buffer)?;
        for (index, byte) in buffer.iter_mut().enumerate() {
            *byte ^= delta[index];
        }
        self.parity_p.write_within(offset, &buffer)?;
        self.parity_p.flush()?;

        if let Some(parity_q) = &self.parity_q {
            // Der Faktor ist `g^slot_index` — nach `slot_index`, nicht nach
            // der Position in irgendeiner Liste.
            let factor = gf::g_pow(slot_index as u8);
            let table = gf::mul_table(factor);

            parity_q.read_extended(offset, &mut buffer)?;
            for (index, byte) in buffer.iter_mut().enumerate() {
                *byte ^= table[usize::from(delta[index])];
            }
            parity_q.write_within(offset, &buffer)?;
            parity_q.flush()?;
        }
        Ok(())
    }

    /// Paritaet aus allen Data-Slots neu bilden.
    ///
    /// Liest den Bereich von jedem Data-Member — zero-extended, damit kuerzere
    /// Members mit Nullen beitragen statt den Vorgang abzubrechen.
    fn recompute_parity(&self, offset: u64, len: usize) -> Result<()> {
        let mut contents = Vec::with_capacity(self.data.len());
        for member in &self.data {
            let mut buffer = vec![0u8; len];
            member.read_extended(offset, &mut buffer)?;
            contents.push(buffer);
        }

        let slots = build_slots(&contents)?;

        let mut parity = vec![0u8; len];
        compute_p(self.data_slot_count(), &slots, &mut parity).map_err(EngineError::from_parity)?;
        self.parity_p.write_within(offset, &parity)?;
        self.parity_p.flush()?;

        if let Some(parity_q) = &self.parity_q {
            compute_q(self.data_slot_count(), &slots, &mut parity)
                .map_err(EngineError::from_parity)?;
            parity_q.write_within(offset, &parity)?;
            parity_q.flush()?;
        }
        Ok(())
    }

    /// Liest aus dem ParityP-Member, zero-extended.
    ///
    /// Fuer den Scrub und fuer Tests, die gegen die Definition der
    /// Kerninvariante pruefen wollen statt gegen `verify_parity` — das
    /// benutzte sonst dieselbe Rechnung, die es bestaetigen soll.
    pub fn read_parity_p(&self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        self.parity_p.read_extended(offset, buffer)
    }

    /// Liest aus dem ParityQ-Member, falls es einen gibt.
    pub fn read_parity_q(&self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        let parity_q = self.parity_q.as_ref().ok_or(EngineError::Format(
            ferrite_format::FormatError::InvalidField {
                field: "role",
                reason: "das Array hat kein ParityQ",
            },
        ))?;
        parity_q.read_extended(offset, buffer)
    }

    /// Prueft, dass die Paritaet zum Inhalt der Data-Members passt.
    ///
    /// Fuer Tests und spaeter fuer den Scrub. Nicht fuer den Schreibpfad — dort
    /// waere es eine Pruefung, die den Fehler erst nach dem Schreiben faende.
    pub fn verify_parity(&self, offset: u64, len: usize) -> Result<bool> {
        let mut contents = Vec::with_capacity(self.data.len());
        for member in &self.data {
            let mut buffer = vec![0u8; len];
            member.read_extended(offset, &mut buffer)?;
            contents.push(buffer);
        }
        let slots = build_slots(&contents)?;

        let mut expected = vec![0u8; len];
        let mut found = vec![0u8; len];

        compute_p(self.data_slot_count(), &slots, &mut expected)
            .map_err(EngineError::from_parity)?;
        self.parity_p.read_extended(offset, &mut found)?;
        if expected != found {
            return Ok(false);
        }

        if let Some(parity_q) = &self.parity_q {
            compute_q(self.data_slot_count(), &slots, &mut expected)
                .map_err(EngineError::from_parity)?;
            parity_q.read_extended(offset, &mut found)?;
            if expected != found {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// Baut die Rolle eines Members in einen [`Member`] um.
///
/// Bequemlichkeit fuer Aufrufer, die Superbloecke und Geraete paarweise halten.
pub fn member_for(device: MemberDevice, superblock: &Superblock, expected: Role) -> Result<Member> {
    if superblock.role != expected {
        return Err(EngineError::Format(
            ferrite_format::FormatError::InvalidField {
                field: "role",
                reason: "Member hat nicht die erwartete Rolle",
            },
        ));
    }
    Member::new(device, superblock)
}

/// Baut die Slot-Liste fuer die Paritaetsrechnung.
///
/// Die Position im Vektor **ist** der `slot_index` — `ArrayWriter::new` haelt
/// `data` nach ihm geordnet und vollstaendig. Waere das nicht so, bekaeme Q die
/// falschen Koeffizienten, und der Fehler faellt erst bei der Rekonstruktion
/// auf, also im Ernstfall.
fn build_slots(contents: &[Vec<u8>]) -> Result<Vec<Slot<'_>>> {
    contents
        .iter()
        .enumerate()
        .map(|(index, data)| Slot::new(index as u8, data).map_err(EngineError::from_parity))
        .collect()
}
