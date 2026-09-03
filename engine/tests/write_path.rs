// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Tests des Schreibpfads.
//!
//! Die beiden Regeln, an denen Daten haengen: kein Checkpoint vor durabler
//! Paritaet, und nach einem Absturz wird die Paritaet neu gerechnet statt
//! fortgeschrieben.

use ferrite_engine::{
    required_parity_update, BatchOrigin, BatchStage, BlockSituation, EngineError, ParityUpdate,
    SourceState, WriteBatch,
};

fn situation(contributing: u32, written: u32, q: bool) -> BlockSituation {
    BlockSituation {
        contributing_slots: contributing,
        written_slots: written,
        has_parity_q: q,
    }
}

// --- Welches Verfahren ist korrekt ---------------------------------------

#[test]
fn after_a_crash_the_parity_is_recomputed() {
    // Der Replay wendet die Writes erneut an. Steht der neue Inhalt schon auf
    // der Platte, ist `D_alt` in Wirklichkeit `D_neu`, und
    // `P ^ D_neu ^ D_neu` laesst die Paritaet unveraendert — also veraltet.
    for written in 0..4 {
        assert_eq!(
            required_parity_update(
                BatchOrigin::Replay,
                SourceState::AllValid,
                situation(20, written, true)
            ),
            Ok(ParityUpdate::Recompute),
            "auch wenn Fortschreiben hier viel billiger waere"
        );
    }
}

#[test]
fn a_degraded_array_can_only_carry_the_parity_forward() {
    // Neurechnen braucht den Inhalt aller Members. Der fehlende ist nicht
    // lesbar — aber die alte Paritaet enthaelt seinen Beitrag bereits, und der
    // aendert sich nicht.
    assert_eq!(
        required_parity_update(
            BatchOrigin::SteadyState,
            SourceState::Degraded,
            situation(4, 3, false)
        ),
        Ok(ParityUpdate::Incremental),
        "auch wenn Neurechnen hier billiger aussaehe"
    );
}

#[test]
fn a_crash_in_a_degraded_array_has_no_safe_answer() {
    // Der Fall, den ich nicht raten will: Neurechnen geht nicht, weil der
    // fehlende Member unlesbar ist. Fortschreiben geht nicht, weil der alte
    // Inhalt nach dem Absturz nicht mehr verlaesslich ist.
    assert_eq!(
        required_parity_update(
            BatchOrigin::Replay,
            SourceState::Degraded,
            situation(4, 1, true)
        ),
        Err(EngineError::CannotUpdateParity)
    );
    assert_eq!(
        WriteBatch::after_crash(SourceState::Degraded),
        Err(EngineError::CannotUpdateParity)
    );
}

#[test]
fn in_the_steady_state_the_cheaper_method_wins() {
    // Viele Slots, wenige geaendert: Fortschreiben. Ein Read fuer den alten
    // Inhalt plus einer fuer P, statt 19 Data-Members zu lesen.
    assert_eq!(
        required_parity_update(
            BatchOrigin::SteadyState,
            SourceState::AllValid,
            situation(20, 1, false)
        ),
        Ok(ParityUpdate::Incremental)
    );

    // Fast alles ueberschrieben: Neurechnen. Ein einziger Read gegen vier.
    assert_eq!(
        required_parity_update(
            BatchOrigin::SteadyState,
            SourceState::AllValid,
            situation(4, 3, false)
        ),
        Ok(ParityUpdate::Recompute)
    );
}

#[test]
fn a_fully_overwritten_block_needs_no_reads_at_all() {
    // Alle beitragenden Slots liegen schon vor — Neurechnen kostet nichts.
    let situation = situation(4, 4, true);
    assert_eq!(situation.reads_for_recompute(), 0);
    assert_eq!(situation.reads_for_incremental(), 6);
    assert_eq!(
        required_parity_update(BatchOrigin::SteadyState, SourceState::AllValid, situation),
        Ok(ParityUpdate::Recompute)
    );
}

#[test]
fn parity_q_makes_carrying_forward_more_expensive() {
    // Fortschreiben braucht auch die alte Q. Das kann die Entscheidung kippen.
    let without_q = situation(4, 1, false);
    let with_q = situation(4, 1, true);
    assert_eq!(without_q.reads_for_incremental(), 2);
    assert_eq!(with_q.reads_for_incremental(), 3);
    assert_eq!(with_q.reads_for_recompute(), 3);

    assert_eq!(
        required_parity_update(BatchOrigin::SteadyState, SourceState::AllValid, without_q),
        Ok(ParityUpdate::Incremental)
    );
    assert_eq!(
        required_parity_update(BatchOrigin::SteadyState, SourceState::AllValid, with_q),
        Ok(ParityUpdate::Recompute),
        "bei Gleichstand das Verfahren ohne Abhaengigkeit vom alten Inhalt"
    );
}

#[test]
fn short_members_cost_nothing_to_recompute() {
    // Zero-Extension: Ein Member, der an diesem Block gar keine Daten mehr
    // traegt, liefert Nullbytes und muss nicht gelesen werden. Er zaehlt
    // deshalb nicht zu den beitragenden Slots.
    let situation = situation(2, 1, false);
    assert_eq!(situation.reads_for_recompute(), 1);
}

// --- Die Reihenfolge im Schreibpfad --------------------------------------

#[test]
fn carrying_forward_reads_the_old_content_before_overwriting() {
    let mut batch = WriteBatch::logged(ParityUpdate::Incremental);
    assert_eq!(batch.next_stage(), Some(BatchStage::OldDataRead));

    for stage in [
        BatchStage::OldDataRead,
        BatchStage::DataWritten,
        BatchStage::ParityWritten,
        BatchStage::Checkpointed,
    ] {
        batch.advance_to(stage).unwrap();
        assert_eq!(batch.stage(), stage);
    }
    assert_eq!(batch.next_stage(), None);
    assert!(batch.log_space_reusable());
}

#[test]
fn recomputing_skips_reading_the_old_content() {
    // Es gibt nichts zu merken: Alle Slots werden ohnehin gelesen.
    let mut batch = WriteBatch::logged(ParityUpdate::Recompute);
    assert_eq!(batch.next_stage(), Some(BatchStage::DataWritten));
    assert_eq!(
        batch.advance_to(BatchStage::OldDataRead),
        Err(EngineError::IllegalTransition {
            from: BatchStage::Logged,
            to: BatchStage::OldDataRead
        })
    );
    batch.advance_to(BatchStage::DataWritten).unwrap();
}

#[test]
fn there_is_no_checkpoint_before_the_parity_is_durable() {
    // Die teuerste Regel des ganzen Schreibpfads. Ein Checkpoint gibt den
    // Log-Platz davor frei — sind die Writes dann schon auf den Data-Members,
    // aber die Paritaet noch nicht nachgezogen, rekonstruiert das Array nach
    // dem naechsten Plattenausfall Muell, und im Log steht nichts mehr, womit
    // sich das reparieren liesse.
    let mut batch = WriteBatch::logged(ParityUpdate::Recompute);
    assert_eq!(
        batch.advance_to(BatchStage::Checkpointed),
        Err(EngineError::CheckpointBeforeParity {
            stage: BatchStage::Logged
        })
    );

    batch.advance_to(BatchStage::DataWritten).unwrap();
    assert_eq!(
        batch.advance_to(BatchStage::Checkpointed),
        Err(EngineError::CheckpointBeforeParity {
            stage: BatchStage::DataWritten
        })
    );
    assert!(!batch.log_space_reusable());

    batch.advance_to(BatchStage::ParityWritten).unwrap();
    batch.advance_to(BatchStage::Checkpointed).unwrap();
    assert!(batch.log_space_reusable());
}

#[test]
fn a_stage_cannot_be_skipped() {
    let mut batch = WriteBatch::logged(ParityUpdate::Recompute);
    assert_eq!(
        batch.advance_to(BatchStage::ParityWritten),
        Err(EngineError::IllegalTransition {
            from: BatchStage::Logged,
            to: BatchStage::ParityWritten
        })
    );
    assert_eq!(
        batch.stage(),
        BatchStage::Logged,
        "der Stapel bleibt stehen"
    );
}

#[test]
fn a_stage_cannot_be_repeated_or_taken_back() {
    let mut batch = WriteBatch::logged(ParityUpdate::Recompute);
    batch.advance_to(BatchStage::DataWritten).unwrap();
    assert!(batch.advance_to(BatchStage::DataWritten).is_err());
    assert!(batch.advance_to(BatchStage::Logged).is_err());
    assert_eq!(batch.stage(), BatchStage::DataWritten);
}

#[test]
fn a_finished_batch_goes_no_further() {
    let mut batch = WriteBatch::logged(ParityUpdate::Recompute);
    for stage in [
        BatchStage::DataWritten,
        BatchStage::ParityWritten,
        BatchStage::Checkpointed,
    ] {
        batch.advance_to(stage).unwrap();
    }
    assert_eq!(batch.next_stage(), None);
    assert!(batch.advance_to(BatchStage::Checkpointed).is_err());
}

#[test]
fn a_batch_taken_up_after_a_crash_recomputes() {
    // Zusammengefuehrt: Der Stapel faengt wieder bei `Logged` an, und das
    // Verfahren ist nicht frei waehlbar.
    let batch = WriteBatch::after_crash(SourceState::AllValid).unwrap();
    assert_eq!(batch.stage(), BatchStage::Logged);
    assert_eq!(batch.method(), ParityUpdate::Recompute);
    assert_eq!(batch.next_stage(), Some(BatchStage::DataWritten));
    assert!(!batch.log_space_reusable());
}

#[test]
fn the_log_space_is_only_free_after_the_checkpoint() {
    // Abschnitt 5.1: Erst der Checkpoint gibt den Platz davor frei.
    let mut batch = WriteBatch::logged(ParityUpdate::Incremental);
    for stage in [
        BatchStage::OldDataRead,
        BatchStage::DataWritten,
        BatchStage::ParityWritten,
    ] {
        assert!(!batch.log_space_reusable(), "noch nicht bei {stage:?}");
        batch.advance_to(stage).unwrap();
    }
    assert!(!batch.log_space_reusable());
    batch.advance_to(BatchStage::Checkpointed).unwrap();
    assert!(batch.log_space_reusable());
}
