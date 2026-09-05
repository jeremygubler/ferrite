// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Abbruchpunkte fuer das Crash-Harness aus Meilenstein 3.
//!
//! **Nur mit dem Cargo-Feature `crash-points`.** Ohne das Feature existiert
//! dieses Modul nicht, und `MemberDevice` zaehlt nichts. Ein Speicherprojekt,
//! das eine Selbstmordfunktion im Schreibpfad mitliefert, hat ein Problem mehr
//! als es loest — deshalb die Trennung zur Bauzeit und nicht zur Laufzeit.
//!
//! Das Modul ist ausserdem Linux-only: `SIGKILL` an den eigenen Prozess gibt es
//! nur dort, und das Harness braucht ohnehin einen Linux-Kernel.
//!
//! # Was hier simuliert wird und was nicht
//!
//! `SIGKILL` an den eigenen Prozess ist ein ehrlicher Stromausfall **fuer die
//! Softwareebene**: keine Destruktoren, keine Puffer, kein `Drop`, das noch
//! etwas rettet. Was auf der Platte steht, steht dort.
//!
//! Was er **nicht** simuliert: einen halb geschriebenen Sektor. Ein echter
//! Stromausfall kann mitten in einem Schreibvorgang zuschlagen; hier faellt
//! immer die ganze Operation aus oder gar keine. Diese Luecke deckt
//! `dm-flakey` mit `drop_writes` auf Geraeteebene ab — sie steht hier, damit
//! niemand den Nachweis fuer vollstaendiger haelt, als er ist.
//!
//! # Warum durchgezaehlt und nicht gewuerfelt
//!
//! Der Abbruchpunkt kommt aus `FERRITE_CRASH_AT` und wird vom Harness von 1 an
//! hochgezaehlt. Damit ist **jeder** Punkt abgedeckt statt einer Stichprobe,
//! und ein Fehlschlag ist mit derselben Zahl exakt wiederholbar. Ein
//! zufaelliger Abbruch findet denselben Fehler vielleicht, aber niemand kann
//! ihn danach nachstellen.

use std::sync::atomic::{AtomicU64, Ordering};

/// Der Abbruchpunkt. `0` heisst: nie abbrechen.
static CRASH_AT: AtomicU64 = AtomicU64::new(0);

/// Wieviele Operationen bisher gezaehlt wurden.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Name der Umgebungsvariablen, aus der der Abbruchpunkt kommt.
pub const CRASH_AT_ENV: &str = "FERRITE_CRASH_AT";

/// Uebernimmt den Abbruchpunkt aus der Umgebung.
///
/// Ohne Aufruf passiert nichts — auch mit gesetztem Feature bricht ein Prozess
/// nur ab, wenn er das ausdruecklich einschaltet.
pub fn arm_from_env() {
    let at = std::env::var(CRASH_AT_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    CRASH_AT.store(at, Ordering::SeqCst);
    COUNTER.store(0, Ordering::SeqCst);
}

/// Setzt den Abbruchpunkt von Hand. Fuer Tests, die keine Umgebung setzen.
pub fn arm(at: u64) {
    CRASH_AT.store(at, Ordering::SeqCst);
    COUNTER.store(0, Ordering::SeqCst);
}

/// Wieviele Operationen bisher durchgelaufen sind.
///
/// Das Harness braucht die Zahl, um zu wissen, wieviele Abbruchpunkte es
/// ueberhaupt gibt: Ein Lauf ohne Abbruch zaehlt sie.
pub fn count() -> u64 {
    COUNTER.load(Ordering::SeqCst)
}

/// Zaehlt eine Operation und bricht ab, wenn sie den Punkt trifft.
///
/// Wird **vor** der Operation aufgerufen: Trifft der Punkt, hat sie nicht
/// stattgefunden. So bedeutet `FERRITE_CRASH_AT=1` „nichts ist passiert" und
/// die Numerierung hat keine Luecke am Anfang.
#[inline]
pub fn before_io() {
    let at = CRASH_AT.load(Ordering::SeqCst);
    let seen = COUNTER.fetch_add(1, Ordering::SeqCst) + 1;
    if at != 0 && seen == at {
        die();
    }
}

// --- Mutationen -----------------------------------------------------------

/// Eine absichtlich falsche Reihenfolge im Schreibpfad.
///
/// Ein Crash-Harness, das nie rot wird, ist eine Behauptung und kein Nachweis.
/// Diese Mutationen drehen bekannte Invarianten um, damit sich pruefen laesst,
/// **dass das Harness sie ueberhaupt bemerken wuerde**. Sie existieren nur mit
/// dem Feature `crash-points` und werden nur ueber eine Umgebungsvariable
/// scharf — in einem Produktionsbau gibt es sie nicht.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mutation {
    /// Der Checkpoint kommt **vor** der Paritaet. Ein Absturz dazwischen laesst
    /// die Paritaet veraltet zurueck, und der Replay ueberspringt den Write,
    /// weil der Checkpoint ihn deckt.
    CheckpointBeforeParity,
}

/// Name der Umgebungsvariablen, die eine Mutation scharfstellt.
pub const MUTATE_ENV: &str = "FERRITE_MUTATE";

static MUTATION: AtomicU64 = AtomicU64::new(0);

const MUTATION_NONE: u64 = 0;
const MUTATION_CHECKPOINT_EARLY: u64 = 1;

/// Uebernimmt die Mutation aus der Umgebung.
pub fn mutation_from_env() {
    let value = match std::env::var(MUTATE_ENV).as_deref() {
        Ok("checkpoint-early") => MUTATION_CHECKPOINT_EARLY,
        _ => MUTATION_NONE,
    };
    MUTATION.store(value, Ordering::SeqCst);
}

/// Die scharfgestellte Mutation, falls es eine gibt.
pub fn mutation() -> Option<Mutation> {
    match MUTATION.load(Ordering::SeqCst) {
        MUTATION_CHECKPOINT_EARLY => Some(Mutation::CheckpointBeforeParity),
        _ => None,
    }
}

/// Beendet den Prozess so hart wie moeglich.
///
/// `SIGKILL` an sich selbst laesst sich nicht abfangen und nicht verzoegern.
/// `process::exit` waere falsch: Es laeuft `atexit`-Handler und flusht, was
/// noch offen ist — also genau das, was ein Stromausfall nicht tut.
fn die() -> ! {
    // SAFETY: `kill` mit der eigenen PID und `SIGKILL` hat keine
    // Vorbedingungen und kehrt nicht zurueck.
    unsafe {
        libc::kill(libc::getpid(), libc::SIGKILL);
    }
    // Erreicht der Kernel den Prozess wider Erwarten nicht, bleibt `abort` —
    // ebenfalls ohne Aufraeumen.
    std::process::abort()
}
