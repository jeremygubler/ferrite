//! Der Rebuild-Plan, `docs/FORMAT.md` Abschnitt 4.2.
//!
//! Hier bekommen `member_state` und `rebuild_progress` ihren Konsumenten. Der
//! Plan sagt, welche Parity-Blöcke noch fehlen, in welcher Reihenfolge sie
//! drankommen und welcher Fortschritt danach in den Superblock gehoert.
//!
//! **Die Reihenfolge ist die eigentliche Regel.** Erst die rekonstruierten
//! Bloecke durable schreiben, dann den Fortschritt in den Superblock. Wer es
//! andersherum macht und dabei abstuerzt, hat Bloecke als fertig gemeldet, die
//! nie geschrieben wurden — und liest danach Nullen als Nutzdaten. Ein Absturz
//! zwischen [`RebuildPlan::next_batch`] und [`RebuildPlan::complete_batch`]
//! kostet dagegen nur, dass derselbe Batch noch einmal laeuft.

use core::ops::Range;

use ferrite_format::superblock::{MemberState, Role, Superblock};

use crate::error::{EngineError, Result};

/// Wiederaufsetzbarer Plan fuer den Wiederaufbau eines Members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuildPlan {
    block_size_log2: u8,
    next_block: u64,
    end_block: u64,
}

impl RebuildPlan {
    /// Setzt den Plan aus dem Superblock des Ziel-Members fort.
    ///
    /// `Clean` ergibt einen leeren Plan, `Stale` einen von vorn, `Rebuilding`
    /// einen ab `rebuild_progress`. Die Ausrichtung des Fortschritts auf eine
    /// Block-Grenze garantiert bereits `Superblock::validate`.
    pub fn resume(member: &Superblock, block_size_log2: u8) -> Result<Self> {
        member.validate()?;
        if member.role == Role::Log {
            return Err(EngineError::CannotRebuild { role: member.role });
        }
        if member.parity_block_size_log2 != block_size_log2 {
            return Err(EngineError::MismatchedBlockSize {
                array: block_size_log2,
                member: member.parity_block_size_log2,
            });
        }

        let end_block = member.payload_size >> block_size_log2;
        let next_block = match member.member_state {
            MemberState::Clean => end_block,
            MemberState::Stale => 0,
            MemberState::Rebuilding => member.rebuild_progress >> block_size_log2,
        };
        Ok(RebuildPlan {
            block_size_log2,
            next_block,
            end_block,
        })
    }

    /// Plan fuer einen Member, der von vorn aufgebaut wird.
    pub fn from_scratch(block_size_log2: u8, block_count: u64) -> Self {
        RebuildPlan {
            block_size_log2,
            next_block: 0,
            end_block: block_count,
        }
    }

    pub fn is_complete(&self) -> bool {
        self.next_block >= self.end_block
    }

    pub fn next_block(&self) -> u64 {
        self.next_block
    }

    pub fn end_block(&self) -> u64 {
        self.end_block
    }

    pub fn remaining_blocks(&self) -> u64 {
        self.end_block.saturating_sub(self.next_block)
    }

    /// Der Wert, der als `rebuild_progress` in den Superblock gehoert.
    pub fn progress_bytes(&self) -> u64 {
        self.next_block << self.block_size_log2
    }

    /// Der naechste Stapel Bloecke. Veraendert den Plan **nicht** — erst
    /// [`RebuildPlan::complete_batch`] schiebt den Fortschritt vor.
    pub fn next_batch(&self, max_blocks: u64) -> Option<Range<u64>> {
        if self.is_complete() || max_blocks == 0 {
            return None;
        }
        let end = self
            .next_block
            .saturating_add(max_blocks)
            .min(self.end_block);
        Some(self.next_block..end)
    }

    /// Meldet einen Stapel als rekonstruiert **und durable geschrieben** und
    /// liefert den neuen `rebuild_progress`.
    ///
    /// Angenommen wird nur genau der Stapel, der als naechster dran ist. Wer
    /// einen ueberspringt, meldet Bloecke als fertig, die nie jemand
    /// rekonstruiert hat.
    pub fn complete_batch(&mut self, batch: Range<u64>) -> Result<u64> {
        if batch.start != self.next_block {
            return Err(EngineError::BatchOutOfOrder {
                expected: self.next_block,
                got: batch.start,
            });
        }
        if batch.end > self.end_block || batch.end < batch.start {
            return Err(EngineError::BatchPastEnd {
                end: batch.end,
                limit: self.end_block,
            });
        }
        self.next_block = batch.end;
        Ok(self.progress_bytes())
    }

    /// Schreibt Zustand und Fortschritt in den Superblock des Ziel-Members.
    ///
    /// Erst aufrufen, wenn die Bloecke des letzten Stapels durable sind. Der
    /// Superblock wird danach validiert: Ein Zustand, den Abschnitt 4.2 nicht
    /// zulaesst, kommt hier nicht heraus.
    pub fn apply_to(&self, member: &mut Superblock) -> Result<()> {
        if self.is_complete() {
            member.member_state = MemberState::Clean;
            member.rebuild_progress = 0;
        } else {
            member.member_state = MemberState::Rebuilding;
            member.rebuild_progress = self.progress_bytes();
        }
        member.validate()?;
        Ok(())
    }
}

/// Ob die Nutzdaten dieses Members im angegebenen Parity-Block brauchbar sind.
///
/// Die Frage entscheidet, ob ein Read direkt bedient werden kann und ob der
/// Member als Quelle in eine Rekonstruktion eingehen darf. Ein `Rebuilding`-
/// Member liefert nur unterhalb seines Fortschritts, ein `Stale` gar nicht.
///
/// Jenseits der eigenen Payload-Region ist die Antwort immer `true`: Dort liest
/// der Member nach der Zero-Extension-Regel Nullbytes, und die sind der
/// richtige Wert — es gibt dort nichts zu rekonstruieren.
pub fn data_is_valid_at(member: &Superblock, block: u64, block_size_log2: u8) -> bool {
    if block >= member.payload_size >> block_size_log2 {
        return true;
    }
    match member.member_state {
        MemberState::Clean => true,
        MemberState::Stale => false,
        MemberState::Rebuilding => block < (member.rebuild_progress >> block_size_log2),
    }
}
