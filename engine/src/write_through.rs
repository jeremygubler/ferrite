// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Der Schreibpfad auf echte Platten, `docs/FORMAT.md` Abschnitt 5, samt
//! Rekonstruktion und Rebuild.
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
//!
//! # Der degradierte Betrieb
//!
//! Ist ein Data-Member `Stale` oder oberhalb seines `rebuild_progress`, liefert
//! er keine brauchbaren Daten. Ein Read auf ihn wird dann aus der Paritaet
//! **rekonstruiert** statt von der Platte gelesen.
//!
//! Das gilt auch fuer den alten Inhalt, den das Fortschreiben braucht — und
//! genau das macht den Fall behandelbar, in dem ein Write auf einen noch nicht
//! wiederaufgebauten Block desselben Members geht: `D_alt` kommt aus der
//! Paritaet, `P' = P ^ D_alt ^ D_neu` bleibt damit stimmig, und eine spaetere
//! Rekonstruktion dieses Blocks liefert wieder `D_neu`.

use core::ops::Range;

use ferrite_format::superblock::{MemberState, Role, Superblock};
use ferrite_format::FormatError;
use ferrite_parity::{compute_p, compute_q, gf, reconstruct_from_p, Slot};

use crate::device::{write_superblock, MemberDevice};
use crate::error::{EngineError, Result};
use crate::log_device::DeviceLog;
use crate::rebuild::{data_is_valid_at, RebuildPlan};
use crate::write_path::{
    required_parity_update, BatchOrigin, BatchStage, BlockSituation, ParityUpdate, SourceState,
    WriteBatch,
};

/// Ein Member samt seinem Superblock.
///
/// Der Superblock ist nicht nur Beiwerk: `member_state` und `rebuild_progress`
/// entscheiden, ob ein Read direkt bedient werden kann oder rekonstruiert
/// werden muss (Abschnitt 4.2).
#[derive(Debug)]
pub struct Member {
    device: MemberDevice,
    superblock: Superblock,
}

impl Member {
    pub fn new(device: MemberDevice, superblock: &Superblock) -> Result<Self> {
        superblock
            .fits_on_device(device.size())
            .map_err(EngineError::Format)?;
        Ok(Member {
            device,
            superblock: superblock.clone(),
        })
    }

    pub fn payload_size(&self) -> u64 {
        self.superblock.payload_size
    }

    pub fn superblock(&self) -> &Superblock {
        &self.superblock
    }

    pub fn device(&self) -> &MemberDevice {
        &self.device
    }

    /// Liest aus der Payload-Region, **zero-extended**.
    ///
    /// Jenseits des Endes dieses Members kommen Nullbytes, kein Fehler. Das ist
    /// die Kerninvariante und nicht Bequemlichkeit: Ohne sie muesste ein Array
    /// mit gemischten Plattengroessen an jedem kurzen Member abbrechen, und die
    /// Paritaet waere fuer den Rest nicht bildbar.
    fn read_extended(&self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        buffer.fill(0);
        let Some(available) = self.superblock.payload_size.checked_sub(offset) else {
            return Ok(());
        };
        let readable = (available as usize).min(buffer.len());
        if readable == 0 {
            return Ok(());
        }
        self.device.read_at(
            self.superblock.payload_offset + offset,
            &mut buffer[..readable],
        )
    }

    /// Schreibt in die Payload-Region. Ueber ihr Ende hinaus wird abgelehnt.
    fn write_within(&self, offset: u64, data: &[u8]) -> Result<()> {
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(EngineError::OffsetOverflow {
                offset,
                len: data.len() as u64,
            })?;
        if end > self.superblock.payload_size {
            return Err(EngineError::BeyondDevice {
                offset,
                len: data.len() as u64,
                size: self.superblock.payload_size,
            });
        }
        self.device
            .write_at(self.superblock.payload_offset + offset, data)
    }

    fn flush(&self) -> Result<()> {
        self.device.flush()
    }

    /// Traegt dieser Member an diesem Bereich brauchbare Daten?
    ///
    /// Ueber alle beruehrten Bloecke, nicht nur den ersten: Ein Bereich, der
    /// die Grenze des `rebuild_progress` ueberschreitet, ist teils brauchbar
    /// und teils nicht — und teilweise brauchbar heisst hier unbrauchbar.
    fn is_valid_over(&self, offset: u64, len: usize, block_size_log2: u8) -> bool {
        if len == 0 {
            return true;
        }
        let first = offset >> block_size_log2;
        let last = (offset + len as u64 - 1) >> block_size_log2;
        (first..=last).all(|block| data_is_valid_at(&self.superblock, block, block_size_log2))
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
    block_size_log2: u8,
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
            return Err(EngineError::Format(FormatError::InvalidField {
                field: "data_slot_count",
                reason: "kein Data-Slot oder mehr als das Format erlaubt",
            }));
        }

        // Regel 2 aus Abschnitt 2.1: gleiche Blockgroesse ueberall. Ohne diese
        // Pruefung rechnete der Rebuild mit einer anderen Einteilung als der
        // Schreibpfad.
        let block_size_log2 = parity_p.superblock.parity_block_size_log2;
        for member in data.iter().chain([&parity_p]).chain(parity_q.as_ref()) {
            if member.superblock.parity_block_size_log2 != block_size_log2 {
                return Err(EngineError::MismatchedBlockSize {
                    array: block_size_log2,
                    member: member.superblock.parity_block_size_log2,
                });
            }
        }

        // Regel 6 aus Abschnitt 2.1: Ein Parity-Member, der kuerzer ist als der
        // laengste Data-Member, hat fuer die Offsets dahinter keine Paritaet.
        let longest = data
            .iter()
            .map(|member| member.superblock.payload_size)
            .max()
            .unwrap_or(0);
        for parity in [Some(&parity_p), parity_q.as_ref()].into_iter().flatten() {
            if parity.superblock.payload_size < longest {
                return Err(EngineError::Format(FormatError::InvalidField {
                    field: "payload_size",
                    reason: "Parity-Member kuerzer als der laengste Data-Member",
                }));
            }
        }

        Ok(ArrayWriter {
            log,
            data,
            parity_p,
            parity_q,
            block_size_log2,
        })
    }

    pub fn data_slot_count(&self) -> u8 {
        self.data.len() as u8
    }

    pub fn has_parity_q(&self) -> bool {
        self.parity_q.is_some()
    }

    pub fn block_size_log2(&self) -> u8 {
        self.block_size_log2
    }

    pub fn log(&self) -> &DeviceLog {
        &self.log
    }

    pub fn member(&self, slot_index: u16) -> Result<&Member> {
        self.data
            .get(usize::from(slot_index))
            .ok_or(EngineError::Format(FormatError::InvalidField {
                field: "slot_index",
                reason: "kein solcher Data-Slot",
            }))
    }

    /// Setzt Zustand und Fortschritt eines Data-Members und schreibt seinen
    /// Superblock auf die Platte.
    ///
    /// Die Reihenfolge ist die Zusage: Erst muessen die Bloecke durable sein,
    /// dann darf der Fortschritt fortgeschrieben werden. Andersherum meldete
    /// ein Neustart Bloecke als fertig, die nie geschrieben wurden.
    pub fn mark_member(
        &mut self,
        slot_index: u16,
        state: MemberState,
        rebuild_progress: u64,
    ) -> Result<()> {
        let member = self
            .data
            .get_mut(usize::from(slot_index))
            .ok_or(EngineError::Format(FormatError::InvalidField {
                field: "slot_index",
                reason: "kein solcher Data-Slot",
            }))?;

        member.superblock.member_state = state;
        member.superblock.rebuild_progress = match state {
            MemberState::Rebuilding => rebuild_progress,
            // Abschnitt 4.2: Nur bei `Rebuilding` ist der Fortschritt von null
            // verschieden. Ein stehengebliebener Wert waere ein Widerspruch,
            // den `validate` zu Recht ablehnt.
            MemberState::Clean | MemberState::Stale => 0,
        };
        member.superblock.generation = member.superblock.generation.saturating_add(1);
        member.superblock.validate().map_err(EngineError::Format)?;

        write_superblock(&member.device, &member.superblock)
    }

    // --- Lesen -----------------------------------------------------------

    /// Liest aus einem Data-Slot.
    ///
    /// Traegt der Member an dieser Stelle keine brauchbaren Daten — `Stale`
    /// oder oberhalb seines `rebuild_progress` —, wird aus der Paritaet
    /// rekonstruiert. Der Aufrufer merkt keinen Unterschied, und genau das ist
    /// der Sinn der Redundanz.
    pub fn read(&self, slot_index: u16, offset: u64, buffer: &mut [u8]) -> Result<()> {
        let member = self.member(slot_index)?;
        self.check_within(member, offset, buffer.len())?;

        if member.is_valid_over(offset, buffer.len(), self.block_size_log2) {
            return match member.read_extended(offset, buffer) {
                Ok(()) => Ok(()),
                // Die Platte ist noch da, dieser Block gibt aber nichts mehr
                // her. Genau dafuer gibt es die Paritaet — der Aufrufer soll
                // seinen Inhalt bekommen und nicht einen Fehler.
                //
                // Nur bei einem Fehler des Betriebssystems. Ein `BeyondDevice`
                // ist ein Programmierfehler, und den mit einer Rekonstruktion
                // zu beantworten hiesse, ihn zu verstecken.
                Err(EngineError::Io {
                    what,
                    kind,
                    raw_os_error,
                }) => {
                    let cause = EngineError::Io {
                        what,
                        kind,
                        raw_os_error,
                    };
                    // Scheitert auch die Rettung, wird der Lesefehler gemeldet
                    // und nicht ihr eigener Fehler: Er ist die Ursache, und der
                    // Aufrufer soll erfahren, welche Platte klemmt.
                    self.reconstruct(slot_index, offset, buffer)
                        .map_err(|_| cause)
                }
                Err(other) => Err(other),
            };
        }
        self.reconstruct(slot_index, offset, buffer)
    }

    /// Rekonstruiert einen Bereich eines Data-Slots aus P und den uebrigen
    /// Data-Slots.
    ///
    /// Setzt voraus, dass alle **anderen** Data-Slots an dieser Stelle
    /// brauchbar sind. Fehlen zwei, reicht P nicht mehr — dann braeuchte es Q,
    /// und `parity/` kann das; hier fehlt aber noch die Buchfuehrung, welcher
    /// zweite Slot gemeint ist. Der Fall wird gemeldet statt geraten.
    pub fn reconstruct(&self, slot_index: u16, offset: u64, out: &mut [u8]) -> Result<()> {
        let target = self.member(slot_index)?;
        self.check_within(target, offset, out.len())?;

        let mut contents = Vec::with_capacity(self.data.len());
        for (index, member) in self.data.iter().enumerate() {
            if index == usize::from(slot_index) {
                contents.push(Vec::new());
                continue;
            }
            if !member.is_valid_over(offset, out.len(), self.block_size_log2) {
                return Err(EngineError::CannotRebuild { role: Role::Data });
            }
            let mut buffer = vec![0u8; out.len()];
            member.read_extended(offset, &mut buffer)?;
            contents.push(buffer);
        }

        let survivors: Vec<Slot<'_>> = contents
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != usize::from(slot_index))
            .map(|(index, data)| Slot::new(index as u8, data).map_err(EngineError::from_parity))
            .collect::<Result<_>>()?;

        let mut parity = vec![0u8; out.len()];
        self.parity_p.read_extended(offset, &mut parity)?;
        reconstruct_from_p(
            self.data_slot_count(),
            slot_index as u8,
            &survivors,
            &parity,
            out,
        )
        .map_err(EngineError::from_parity)
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
        let parity_q =
            self.parity_q
                .as_ref()
                .ok_or(EngineError::Format(FormatError::InvalidField {
                    field: "role",
                    reason: "das Array hat kein ParityQ",
                }))?;
        parity_q.read_extended(offset, buffer)
    }

    // --- Schreiben --------------------------------------------------------

    /// Schreibt in einen Data-Slot und zieht die Paritaet nach.
    ///
    /// Kehrt erst zurueck, wenn Data-Member **und** Paritaet durable sind. Das
    /// ist die Zusage des Write-Through-Modus aus Abschnitt 5.3.
    pub fn write(&mut self, slot_index: u16, offset: u64, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let member = self.member(slot_index)?;
        self.check_within(member, offset, data.len())?;

        // 1. Der Record ins Log. Ab hier ueberlebt der Write einen Absturz,
        //    auch wenn keiner der folgenden Schritte mehr passiert.
        self.log.append_write(slot_index, offset, data)?;

        let method = self.choose_method(BatchOrigin::SteadyState, offset, data.len())?;
        let mut batch = WriteBatch::logged(method);

        // 2. Beim Fortschreiben den alten Inhalt holen — **vor** dem
        //    Ueberschreiben, sonst ist er weg. `read` rekonstruiert ihn, wenn
        //    der Member an dieser Stelle nichts Brauchbares traegt.
        let old = if method == ParityUpdate::Incremental {
            let mut old = vec![0u8; data.len()];
            self.read(slot_index, offset, &mut old)?;
            batch.advance_to(BatchStage::OldDataRead)?;
            Some(old)
        } else {
            None
        };

        // 3. Auf den Data-Member.
        let member = self.member(slot_index)?;
        member.write_within(offset, data)?;
        member.flush()?;
        batch.advance_to(BatchStage::DataWritten)?;

        // Die Mutation aus `crash.rs` zieht den Checkpoint hierher vor. Sie
        // ist nur mit dem Feature `crash-points` ueberhaupt vorhanden und
        // dient allein dem Nachweis, dass das Harness so etwas bemerkt.
        let mutated = checkpoint_before_parity();
        if mutated {
            self.log.append_checkpoint()?;
        }

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
        if !mutated {
            self.log.append_checkpoint()?;
        }
        batch.advance_to(BatchStage::Checkpointed)?;
        Ok(())
    }

    // --- Recovery ---------------------------------------------------------

    /// Schritt 5 aus Abschnitt 5.2: die akzeptierten Writes anwenden, dann die
    /// Paritaet neu rechnen, dann einen Checkpoint schreiben.
    ///
    /// **Nach einem Absturz wird neu gerechnet und nicht fortgeschrieben.** Der
    /// Replay wendet Writes erneut an; steht der neue Inhalt schon auf der
    /// Platte, ist `D_alt` in Wirklichkeit `D_neu`, und `P ^ D_neu ^ D_neu`
    /// liesse die Paritaet unveraendert — also veraltet. `required_parity_update`
    /// sagt genau das, und hier wird es befolgt statt neu entschieden.
    ///
    /// Ein Record, der die Bedingungen aus Abschnitt 5.2 verletzt, beendet den
    /// Replay wie jeder andere Bruch der Kette. Er wird nicht uebersprungen:
    /// Was danach kommt, gehoert zu einer Runde, deren Anfang fehlt.
    ///
    /// Rueckgabe ist die Zahl der angewendeten Writes.
    pub fn recover(&mut self, recovery: &crate::log_device::LogRecovery) -> Result<u64> {
        let mut applied = 0u64;
        let mut touched: Vec<(u64, usize)> = Vec::new();

        for record in recovery.records()? {
            if record.header.record_type != ferrite_format::log::RecordType::Write {
                continue;
            }
            let slot_index = record.header.slot_index;
            let offset = record.header.target_offset;

            // Beide Werte kommen ungeprueft von der Platte. Ohne diese
            // Pruefung schriebe der Replay ueber das Ende der Payload-Region
            // hinaus — im besten Fall in den Backup-Superblock.
            let member = match self.member(slot_index) {
                Ok(member) => member,
                Err(_) => break,
            };
            if self
                .check_within(member, offset, record.payload.len())
                .is_err()
            {
                break;
            }

            member.write_within(offset, record.payload)?;
            touched.push((offset, record.payload.len()));
            applied += 1;
        }

        if applied == 0 {
            return Ok(0);
        }
        for member in &self.data {
            member.flush()?;
        }

        // Die Paritaet fuer jeden beruehrten Bereich neu bilden. Ueberlappen
        // sich zwei Bereiche, wird der gemeinsame Teil zweimal gerechnet — das
        // kostet Zeit und aendert nichts, weil Neurechnen keinen Vorzustand
        // braucht.
        for (offset, len) in &touched {
            let method = self.choose_method(BatchOrigin::Replay, *offset, *len)?;
            match method {
                ParityUpdate::Recompute => self.recompute_parity(*offset, *len)?,
                // `required_parity_update` gibt nach einem Absturz nur
                // `Recompute` oder einen Fehler zurueck. Kaeme hier etwas
                // anderes, waere eine Annahme falsch — melden statt rechnen.
                ParityUpdate::Incremental => return Err(EngineError::CannotUpdateParity),
            }
        }

        self.log.append_checkpoint()?;
        Ok(applied)
    }

    // --- Rebuild ----------------------------------------------------------

    /// Rekonstruiert einen Stapel Bloecke und schreibt sie durable.
    ///
    /// Aendert **nicht** den Fortschritt im Superblock — das ist Sache des
    /// Aufrufers und muss danach passieren. Wer beides in einem Schritt
    /// erledigte, koennte den Fortschritt vor den Bloecken durable haben.
    ///
    /// Die Paritaet bleibt unangetastet: Der Member bekommt genau das, was
    /// bereits in ihr steckt.
    pub fn rebuild_batch(&mut self, slot_index: u16, blocks: Range<u64>) -> Result<()> {
        if blocks.start >= blocks.end {
            return Ok(());
        }
        let block_size = 1u64 << self.block_size_log2;
        let member = self.member(slot_index)?;
        let end_offset = blocks.end.saturating_mul(block_size);
        if end_offset > member.superblock.payload_size {
            return Err(EngineError::BatchPastEnd {
                end: blocks.end,
                limit: member.superblock.payload_size >> self.block_size_log2,
            });
        }

        let mut buffer = vec![0u8; block_size as usize];
        for block in blocks {
            let offset = block * block_size;
            self.reconstruct(slot_index, offset, &mut buffer)?;
            self.member(slot_index)?.write_within(offset, &buffer)?;
        }
        self.member(slot_index)?.flush()
    }

    // --- Innereien --------------------------------------------------------

    fn check_within(&self, member: &Member, offset: u64, len: usize) -> Result<()> {
        let end = offset
            .checked_add(len as u64)
            .ok_or(EngineError::OffsetOverflow {
                offset,
                len: len as u64,
            })?;
        if end > member.superblock.payload_size {
            return Err(EngineError::BeyondDevice {
                offset,
                len: len as u64,
                size: member.superblock.payload_size,
            });
        }
        Ok(())
    }

    /// Ob an dieser Stelle alle Data-Members brauchbare Daten liefern.
    ///
    /// Wird aus den Superbloecken abgeleitet und nicht von aussen gesetzt: Ein
    /// zweiter Ort, an dem derselbe Zustand steht, ist ein Ort, an dem er
    /// abweichen kann.
    fn sources_at(&self, offset: u64, len: usize) -> SourceState {
        if self
            .data
            .iter()
            .all(|member| member.is_valid_over(offset, len, self.block_size_log2))
        {
            SourceState::AllValid
        } else {
            SourceState::Degraded
        }
    }

    /// Welches Verfahren fuer diesen Bereich korrekt und guenstiger ist.
    ///
    /// Die Entscheidung faellt `write_path::required_parity_update` — sie steht
    /// dort samt Begruendung und wird hier nicht noch einmal getroffen.
    fn choose_method(&self, origin: BatchOrigin, offset: u64, len: usize) -> Result<ParityUpdate> {
        let situation = BlockSituation {
            contributing_slots: self.data.len() as u32,
            written_slots: 1,
            has_parity_q: self.parity_q.is_some(),
        };
        required_parity_update(origin, self.sources_at(offset, len), situation)
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
            let table = gf::mul_table(gf::g_pow(slot_index as u8));

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
        let contents = self.read_all_data(offset, len)?;
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

    fn read_all_data(&self, offset: u64, len: usize) -> Result<Vec<Vec<u8>>> {
        let mut contents = Vec::with_capacity(self.data.len());
        for member in &self.data {
            let mut buffer = vec![0u8; len];
            member.read_extended(offset, &mut buffer)?;
            contents.push(buffer);
        }
        Ok(contents)
    }

    /// Bildet die Paritaet fuer einen Bereich aus den Data-Members neu.
    ///
    /// Die Antwort auf einen Scrub-Befund, bei dem die Data-Members unversehrt
    /// sind und nur die Paritaet nicht mehr passt — etwa nach einem Geraet, das
    /// Writes verschluckt hat (Abschnitt 5.3).
    ///
    /// **Nur brauchbar, solange alle Data-Slots gueltige Daten liefern.** Ist
    /// einer degradiert, gaebe das Neubilden eine Paritaet ueber seinen
    /// unbrauchbaren Inhalt — und danach liesse sich nichts mehr
    /// rekonstruieren. Deshalb wird das hier geprueft und nicht dem Aufrufer
    /// ueberlassen.
    pub fn rebuild_parity(&mut self, offset: u64, len: usize) -> Result<()> {
        if self.sources_at(offset, len) != SourceState::AllValid {
            return Err(EngineError::CannotUpdateParity);
        }
        self.recompute_parity(offset, len)
    }

    /// Prueft, dass die Paritaet zum Inhalt der Data-Members passt.
    ///
    /// Fuer Tests und spaeter fuer den Scrub. Nicht fuer den Schreibpfad — dort
    /// waere es eine Pruefung, die den Fehler erst nach dem Schreiben faende.
    pub fn verify_parity(&self, offset: u64, len: usize) -> Result<bool> {
        let contents = self.read_all_data(offset, len)?;
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

/// Ein laufender Rebuild auf echten Platten.
///
/// Haelt den Plan aus `rebuild.rs` und sorgt fuer die Reihenfolge, um die es
/// geht: **erst die Bloecke durable, dann der Fortschritt**. Andersherum
/// meldete ein Neustart Bloecke als fertig, die nie geschrieben wurden — und
/// dort staende hinterher, was die Platte vorher trug.
#[derive(Debug)]
pub struct DiskRebuild {
    slot_index: u16,
    plan: RebuildPlan,
}

impl DiskRebuild {
    /// Setzt den Rebuild aus dem Superblock des Ziel-Members fort.
    ///
    /// Ein Member im Zustand `Clean` ergibt einen fertigen Plan, `Stale` einen
    /// von vorn, `Rebuilding` einen ab seinem Fortschritt. Der Zustand kommt
    /// von der Platte und nicht aus dem Arbeitsspeicher — nach einem Absturz
    /// ist er das Einzige, was noch da ist.
    pub fn resume(writer: &ArrayWriter, slot_index: u16) -> Result<Self> {
        let member = writer.member(slot_index)?;
        let plan = RebuildPlan::resume(member.superblock(), writer.block_size_log2())?;
        Ok(DiskRebuild { slot_index, plan })
    }

    pub fn slot_index(&self) -> u16 {
        self.slot_index
    }

    pub fn is_complete(&self) -> bool {
        self.plan.is_complete()
    }

    pub fn remaining_blocks(&self) -> u64 {
        self.plan.remaining_blocks()
    }

    pub fn next_block(&self) -> u64 {
        self.plan.next_block()
    }

    /// Arbeitet den naechsten Stapel ab. `false` heisst: nichts mehr zu tun.
    ///
    /// Die Reihenfolge in dieser Funktion ist die eigentliche Aussage:
    ///
    /// 1. Bloecke rekonstruieren und schreiben
    /// 2. `flush` — jetzt sind sie durable
    /// 3. erst danach `rebuild_progress` in den Superblock, und der wird
    ///    ebenfalls geflusht
    ///
    /// Wer 2 und 3 vertauscht, hat nach einem Absturz einen Member, der Bloecke
    /// als fertig meldet, die er nie bekommen hat. Sie werden dann nie wieder
    /// rekonstruiert, und niemand merkt es.
    pub fn step(&mut self, writer: &mut ArrayWriter, max_blocks: u64) -> Result<bool> {
        let Some(batch) = self.plan.next_batch(max_blocks) else {
            return Ok(false);
        };

        writer.rebuild_batch(self.slot_index, batch.clone())?;
        let progress = self.plan.complete_batch(batch)?;

        let state = if self.plan.is_complete() {
            MemberState::Clean
        } else {
            MemberState::Rebuilding
        };
        writer.mark_member(self.slot_index, state, progress)?;
        Ok(true)
    }

    /// Arbeitet den Rebuild bis zum Ende ab.
    pub fn run(&mut self, writer: &mut ArrayWriter, max_blocks: u64) -> Result<()> {
        while self.step(writer, max_blocks)? {}
        Ok(())
    }
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

/// Baut die Rolle eines Members in einen [`Member`] um.
///
/// Bequemlichkeit fuer Aufrufer, die Superbloecke und Geraete paarweise halten.
pub fn member_for(device: MemberDevice, superblock: &Superblock, expected: Role) -> Result<Member> {
    if superblock.role != expected {
        return Err(EngineError::Format(FormatError::InvalidField {
            field: "role",
            reason: "Member hat nicht die erwartete Rolle",
        }));
    }
    Member::new(device, superblock)
}

/// Ist die Mutation `CheckpointBeforeParity` scharfgestellt?
///
/// Ohne das Feature `crash-points` gibt es keine Mutationen, und diese Funktion
/// ist eine Konstante, die der Optimierer wegwirft. Siehe `crash::Mutation` —
/// sie existiert allein dafuer, dass sich nachweisen laesst, dass das
/// Crash-Harness einen Fehler dieser Art ueberhaupt bemerkt.
#[cfg(all(target_os = "linux", feature = "crash-points"))]
fn checkpoint_before_parity() -> bool {
    crate::crash::mutation() == Some(crate::crash::Mutation::CheckpointBeforeParity)
}

#[cfg(not(all(target_os = "linux", feature = "crash-points")))]
fn checkpoint_before_parity() -> bool {
    false
}
