// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Der Laeufer des Crash-Harness.
//!
//! Bricht den Schreibpfad an **jedem** I/O-Punkt ab und prueft danach die drei
//! Zusagen aus dem Modulkopf von `ferrite_harness`. Kein Root, keine
//! Loop-Geraete, kein besonderer Kernel — nur Linux, weil der Abbruch ein
//! `SIGKILL` an den eigenen Prozess ist.
//!
//! Der Lauf dauert etwas: Fuer jeden Abbruchpunkt wird das Array
//! zurueckgesetzt, der Arbeiter gestartet, getoetet und die Pruefung
//! ausgefuehrt. Das ist der Preis dafuer, dass keine Luecke bleibt.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::Command;

/// Wieviele Writes der Arbeiter ausfuehrt.
///
/// Genug, dass jeder Bereich einmal beschrieben und einmal ueberschrieben wird
/// — das Ueberschreiben ist der Teil, der nach einem Absturz falsch werden
/// kann. Mehr kostet nur Laufzeit.
const WRITES: u64 = 8;

const WORKER: &str = env!("CARGO_BIN_EXE_crash-worker");

/// Ein Arbeitsverzeichnis, das sich selbst wegraeumt.
struct Workspace(PathBuf);

impl Workspace {
    fn new(name: &str) -> Self {
        // Bewusst im Linux-Temp und nicht irgendwo unter `/mnt`: Der Lauf
        // kopiert das Array einmal pro Abbruchpunkt, und ueber einen
        // Windows-Mount dauerte das Minuten statt Sekunden.
        let path =
            std::env::temp_dir().join(format!("ferrite-crash-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("Arbeitsverzeichnis anlegen");
        Workspace(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Ergebnis eines Arbeiterlaufs.
struct Run {
    status: Option<i32>,
    killed: bool,
    stdout: String,
    stderr: String,
}

fn run_worker(phase: &str, directory: &Path, extra: &[&str], crash_at: Option<u64>) -> Run {
    let mut command = Command::new(WORKER);
    command.arg(phase).arg(directory).args(extra);
    match crash_at {
        Some(at) => command.env("FERRITE_CRASH_AT", at.to_string()),
        None => command.env_remove("FERRITE_CRASH_AT"),
    };

    let output = command.output().expect("Arbeiter starten");
    // Ein per Signal beendeter Prozess hat keinen Exit-Code. Genau daran
    // erkennt der Laeufer, dass der Abbruchpunkt getroffen hat.
    let killed = output.status.code().is_none();
    Run {
        status: output.status.code(),
        killed,
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    }
}

/// Kopiert alle Dateien eines Verzeichnisses in ein anderes.
fn copy_dir(from: &Path, to: &Path) {
    let _ = std::fs::remove_dir_all(to);
    std::fs::create_dir_all(to).expect("Zielverzeichnis anlegen");
    for entry in std::fs::read_dir(from).expect("Quellverzeichnis lesen") {
        let entry = entry.expect("Eintrag");
        if entry.path().is_file() {
            std::fs::copy(entry.path(), to.join(entry.file_name())).expect("kopieren");
        }
    }
}

// --- Die Tests ------------------------------------------------------------

#[test]
fn a_run_without_a_crash_leaves_everything_in_order() {
    // Der Bezugspunkt. Faellt schon dieser Test, sagt der Rest des Harness
    // nichts ueber Abstuerze aus, sondern nur, dass etwas grundsaetzlich
    // kaputt ist.
    let workspace = Workspace::new("ohne-absturz");
    let array = workspace.path().join("array");

    let created = run_worker("create", &array, &[], None);
    assert_eq!(created.status, Some(0), "anlegen: {}", created.stderr);

    let written = run_worker("write", &array, &[&WRITES.to_string()], None);
    assert_eq!(written.status, Some(0), "schreiben: {}", written.stderr);

    let points: u64 = written.stdout.parse().expect("Zahl der I/O-Punkte");
    assert!(points > 0, "der Ablauf hat keine I/O-Operation gezaehlt");
    eprintln!("{points} I/O-Punkte fuer {WRITES} Writes");

    let verified = run_worker("verify", &array, &[], None);
    assert_eq!(verified.status, Some(0), "pruefen: {}", verified.stderr);
}

/// Der eigentliche Test: Absturz an jedem einzelnen I/O-Punkt.
#[test]
fn the_array_survives_a_crash_at_every_single_io_point() {
    let workspace = Workspace::new("jeder-punkt");
    let array = workspace.path().join("array");
    let pristine = workspace.path().join("frisch");

    let created = run_worker("create", &array, &[], None);
    assert_eq!(created.status, Some(0), "anlegen: {}", created.stderr);
    copy_dir(&array, &pristine);

    // Vorlauf ohne Abbruch: Wieviele Punkte gibt es ueberhaupt?
    let counted = run_worker("write", &array, &[&WRITES.to_string()], None);
    assert_eq!(counted.status, Some(0), "Vorlauf: {}", counted.stderr);
    let points: u64 = counted.stdout.parse().expect("Zahl der I/O-Punkte");
    eprintln!("{points} Abbruchpunkte werden geprueft");

    let mut crashed = 0u64;
    for at in 1..=points {
        copy_dir(&pristine, &array);

        let run = run_worker("write", &array, &[&WRITES.to_string()], Some(at));
        if run.killed {
            crashed += 1;
        } else {
            // Kein Abbruch: Der Punkt lag hinter dem letzten I/O. Das darf
            // vorkommen, wenn ein frueherer Abbruch die Zahl der Operationen
            // veraendert haette — hier nicht, aber der Fall soll nicht
            // stillschweigend als Erfolg durchgehen.
            assert_eq!(
                run.status,
                Some(0),
                "Punkt {at}: weder abgestuerzt noch sauber durch: {}",
                run.stderr
            );
        }

        let verified = run_worker("verify", &array, &[], None);
        assert_eq!(
            verified.status,
            Some(0),
            "Punkt {at}: nach dem Absturz verletzt — {}",
            verified.stderr
        );
    }

    assert!(
        crashed >= points / 2,
        "nur {crashed} von {points} Punkten haben wirklich abgebrochen"
    );
    eprintln!("{crashed} von {points} Punkten haben abgebrochen, alle Zusagen gehalten");
}

#[test]
fn a_crash_and_a_second_crash_during_recovery_still_hold() {
    // Der unangenehme Fall: Der Strom faellt aus, das Recovery beginnt, und
    // mittendrin faellt er wieder aus. Ein Recovery, das nur beim ersten
    // Anlauf funktioniert, hilft niemandem.
    let workspace = Workspace::new("doppelt");
    let array = workspace.path().join("array");
    let pristine = workspace.path().join("frisch");

    assert_eq!(run_worker("create", &array, &[], None).status, Some(0));
    copy_dir(&array, &pristine);

    let counted = run_worker("write", &array, &[&WRITES.to_string()], None);
    let points: u64 = counted.stdout.parse().expect("Zahl der I/O-Punkte");

    // Ein paar Punkte quer durch den Ablauf, jeder mit einem zweiten Abbruch
    // kurz danach. Alle Kombinationen waeren points² Laeufe — das ist die
    // Stelle, an der eine Auswahl vertretbar ist, weil der erste Abbruch
    // bereits vollstaendig abgedeckt ist.
    for first in [points / 4, points / 2, (3 * points) / 4].into_iter() {
        if first == 0 {
            continue;
        }
        copy_dir(&pristine, &array);
        let run = run_worker("write", &array, &[&WRITES.to_string()], Some(first));
        assert!(run.killed, "Punkt {first} hat nicht abgebrochen");

        for second in 1..=4u64 {
            // Das Recovery laeuft beim naechsten `write` mit — und stirbt
            // dabei erneut.
            let again = run_worker("write", &array, &[&WRITES.to_string()], Some(second));
            assert!(
                again.killed || again.status == Some(0),
                "zweiter Absturz bei {second}: {}",
                again.stderr
            );

            let verified = run_worker("verify", &array, &[], None);
            assert_eq!(
                verified.status,
                Some(0),
                "Absturz {first} dann {second}: {}",
                verified.stderr
            );
        }
    }
}

// --- Der Nachweis, dass der Nachweis wirkt --------------------------------

/// Prueft, dass das Harness einen bekannten Fehler **wirklich** bemerkt.
///
/// Ein Crash-Harness, das immer gruen ist, ist eine Behauptung. Dieser Test
/// stellt die Mutation `checkpoint-early` scharf — der Checkpoint kommt dann
/// vor der Paritaet — und verlangt, dass mindestens ein Abbruchpunkt die
/// Pruefung zu Fall bringt.
///
/// Faellt dieser Test aus, ist nicht der Schreibpfad kaputt, sondern das
/// Harness: Es wuerde einen Datenverlust dieser Art durchgehen lassen.
#[test]
fn the_harness_notices_a_checkpoint_written_too_early() {
    let workspace = Workspace::new("mutation");
    let array = workspace.path().join("array");
    let pristine = workspace.path().join("frisch");

    assert_eq!(run_worker("create", &array, &[], None).status, Some(0));
    copy_dir(&array, &pristine);

    let counted = run_worker("write", &array, &[&WRITES.to_string()], None);
    let points: u64 = counted.stdout.parse().expect("Zahl der I/O-Punkte");

    let mut caught = None;
    for at in 1..=points {
        copy_dir(&pristine, &array);

        let mut command = Command::new(WORKER);
        command
            .arg("write")
            .arg(&array)
            .arg(WRITES.to_string())
            .env("FERRITE_CRASH_AT", at.to_string())
            .env("FERRITE_MUTATE", "checkpoint-early");
        let _ = command.output().expect("Arbeiter starten");

        let verified = run_worker("verify", &array, &[], None);
        if verified.status != Some(0) {
            caught = Some((at, verified.stderr));
            break;
        }
    }

    let Some((at, message)) = caught else {
        panic!(
            "das Harness hat den vorgezogenen Checkpoint an keinem der {points} Punkte bemerkt — \
             es wuerde einen Datenverlust dieser Art durchgehen lassen"
        );
    };
    eprintln!("Mutation bei Punkt {at} bemerkt: {message}");
    assert!(
        message.contains("Zusage"),
        "der Fehlschlag kam nicht von einer der Zusagen: {message}"
    );
}

// --- Der Fall, der seit Meilenstein 1 auf eine Entscheidung wartet --------

/// Absturz im degradierten Betrieb.
///
/// Ein Member ist ausgefallen, der Schreibpfad laeuft weiter — und dann faellt
/// der Strom aus. Beim Recovery ist Neurechnen unmoeglich (der fehlende Member
/// liesse sich nicht lesen) und Fortschreiben ebenfalls (nach dem Absturz ist
/// unbekannt, ob die Paritaet zum alten oder neuen Inhalt gehoert). Der Inhalt
/// des fehlenden Members ist fuer diese Bereiche verloren, und daran laesst
/// sich nichts aendern.
///
/// Die Entscheidung ist, den Verlust **einzugrenzen statt das Array
/// aufzugeben**: Die Paritaet wird neu gebildet, der fehlende Slot zaehlt dabei
/// als Nullbytes, und `recover` meldet die betroffenen Bereiche. Das Array
/// bleibt oeffenbar und die uebrigen Members voll nutzbar — genau die
/// Eigenschaft, die dieses Projekt zusichert.
///
/// Die Alternative waere gewesen, das Oeffnen zu verweigern. Dann waere ein
/// Array mit einer ausgefallenen Platte nach einem Stromausfall vollstaendig
/// unbrauchbar, obwohl alle uebrigen Members unversehrt sind.
#[test]
fn a_crash_while_degraded_loses_only_what_it_must_and_says_so() {
    let workspace = Workspace::new("degradiert");
    let array = workspace.path().join("array");
    let pristine = workspace.path().join("frisch");

    assert_eq!(run_worker("create", &array, &[], None).status, Some(0));

    // Ein paar Writes im gesunden Zustand, dann faellt Slot 2 aus.
    let warmup = run_worker("write", &array, &[&WRITES.to_string()], None);
    assert_eq!(warmup.status, Some(0), "Vorlauf: {}", warmup.stderr);
    let degraded = run_worker("degrade", &array, &["2"], None);
    assert_eq!(degraded.status, Some(0), "degradieren: {}", degraded.stderr);
    copy_dir(&array, &pristine);

    let counted = run_worker("write", &array, &[&WRITES.to_string()], None);
    assert_eq!(
        counted.status,
        Some(0),
        "der Schreibpfad kommt im degradierten Betrieb nicht durch: {}",
        counted.stderr
    );
    let points: u64 = counted.stdout.parse().expect("Zahl der I/O-Punkte");

    let mut with_recovery = 0u64;
    let mut with_loss = 0u64;
    let mut failures = Vec::new();

    for at in 1..=points {
        copy_dir(&pristine, &array);
        let run = run_worker("write", &array, &[&WRITES.to_string()], Some(at));
        if !run.killed {
            continue;
        }

        let verified = run_worker("verify", &array, &[], None);
        if verified.stderr.contains("angewendet") {
            with_recovery += 1;
        }
        if verified.stderr.contains("verloren") {
            with_loss += 1;
        }
        if verified.status != Some(0) {
            failures.push((at, verified.stderr.clone()));
        }
    }

    eprintln!(
        "Absturz im degradierten Betrieb: {points} Punkte, {with_recovery} mit Recovery, \
         {with_loss} davon mit gemeldetem Verlust"
    );

    assert!(
        failures.is_empty(),
        "das Array liess sich an {} von {points} Punkten nicht oeffnen: {:?}",
        failures.len(),
        failures.iter().take(3).collect::<Vec<_>>()
    );

    // Ohne diese beiden Zeilen waere der Test gruen, wenn das Recovery gar
    // nicht erst liefe — und genau das war er einmal, verdeckt von einem
    // Fehler im Log.
    assert!(
        with_recovery > 0,
        "an keinem Punkt lief ein Recovery — der Test hat den Fall nicht erreicht"
    );
    assert_eq!(
        with_loss, with_recovery,
        "ein Recovery im degradierten Betrieb ohne gemeldeten Verlust: \
         entweder ist der Verlust nicht eingetreten, oder er wird verschwiegen"
    );
}
