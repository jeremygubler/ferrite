// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Die tägliche Wache, über das echte Binary.
//!
//! # Was hier wirklich geprüft wird
//!
//! Nicht, dass `check` den Zustand erkennt — das tut `status`, und das ist
//! anderswo geprüft. Hier geht es um die eine Regel, an der alles hängt:
//! **gemeldet wird nur eine Änderung.**
//!
//! Beide Fehler sind still und beide sind teuer. Wer jeden Tag meldet,
//! erzeugt eine Mail, die nach einer Woche niemand mehr liest — und dann wird
//! auch die übersehen, die etwas Neues sagt. Wer nie meldet, hat ein Array,
//! das drei Wochen degradiert läuft, ohne dass es jemand weiss; der zweite
//! Ausfall kommt dann unangekündigt.
//!
//! Der `notify`-Befehl ist deshalb hier ein Skript, das eine Datei anlegt.
//! Danach lässt sich zählen, wie oft gemeldet wurde — und das ist die Zahl,
//! um die es geht.
//!
//! Ohne Root und ohne Kernel: `check` liest Superblöcke aus Dateien.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FERRITE: &str = env!("CARGO_BIN_EXE_ferrite");
const DISK: u64 = 8 << 20;

struct Werkstatt(PathBuf);

impl Werkstatt {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("ferrite-check-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("Arbeitsverzeichnis");
        Werkstatt(path)
    }

    fn disk(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        let file = std::fs::File::create(&path).expect("Geraetedatei");
        file.set_len(DISK).expect("Groesse");
        path
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// Eine Konfiguration mit Tagebuch und einem `notify`, das mitschreibt.
    ///
    /// Das Skript haengt bei jedem Aufruf eine Zeile an. Ein Zaehler in einer
    /// Datei ist umstaendlicher als ein Mock — aber er misst, was wirklich
    /// passiert ist, und nicht, was der Test erwartet hat.
    fn config(&self, mit_notify: bool) -> PathBuf {
        let melder = self.path("melder.sh");
        std::fs::write(
            &melder,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$1\" >> {}\n",
                self.path("meldungen").display()
            ),
        )
        .expect("Melder schreiben");
        let mut rechte = std::fs::metadata(&melder).expect("Rechte").permissions();
        {
            use std::os::unix::fs::PermissionsExt;
            rechte.set_mode(0o755);
        }
        std::fs::set_permissions(&melder, rechte).expect("Melder ausfuehrbar machen");

        let mut text = format!("journal = {}\n", self.path("journal").display());
        if mit_notify {
            text.push_str(&format!("notify = {}\n", melder.display()));
        }
        let path = self.path("ferrite.conf");
        std::fs::write(&path, text).expect("Konfiguration schreiben");
        path
    }

    /// Wie oft der `notify`-Befehl gelaufen ist.
    fn meldungen(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("meldungen"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn journal(&self) -> String {
        std::fs::read_to_string(self.path("journal")).unwrap_or_default()
    }
}

impl Drop for Werkstatt {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn ferrite(arguments: &[&str]) -> Output {
    Command::new(FERRITE)
        .args(arguments)
        .output()
        .expect("ferrite starten")
}

fn code(output: &Output) -> i32 {
    output.status.code().expect("Rueckgabewert")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Ein Array aus zwei Data-Slots, P und Log.
struct Array {
    werkstatt: Werkstatt,
    paths: Vec<String>,
    config: PathBuf,
}

impl Array {
    fn new(name: &str, mit_notify: bool) -> Self {
        let werkstatt = Werkstatt::new(name);
        let paths: Vec<String> = ["a", "b", "p", "log"]
            .iter()
            .map(|n| werkstatt.disk(n).display().to_string())
            .collect();
        let config = werkstatt.config(mit_notify);

        let angelegt = ferrite(&[
            "create",
            "--data",
            &paths[0],
            "--data",
            &paths[1],
            "--parity-p",
            &paths[2],
            "--log",
            &paths[3],
            "--yes",
        ]);
        assert_eq!(code(&angelegt), 0, "create: {}", stdout(&angelegt));

        Array {
            werkstatt,
            paths,
            config,
        }
    }

    /// `ferrite check` ueber alle Geraete, die noch da sind.
    fn check(&self, ohne: Option<usize>) -> Output {
        let mut argumente: Vec<&str> = vec!["check"];
        for (index, path) in self.paths.iter().enumerate() {
            if Some(index) != ohne {
                argumente.push(path);
            }
        }
        let config = self.config.display().to_string();
        argumente.push("--config");
        argumente.push(&config);
        ferrite(&argumente)
    }

    /// Slot 1 gegen eine frische Platte tauschen — danach steht er `Stale`.
    fn ersetze_slot_1(&self) {
        let ersatz = self.werkstatt.disk("neu").display().to_string();
        let mut argumente: Vec<&str> = vec!["replace"];
        for (index, path) in self.paths.iter().enumerate() {
            if index != 1 {
                argumente.push(path);
            }
        }
        argumente.extend(["--slot", "1", "--with", &ersatz, "--yes"]);
        let output = ferrite(&argumente);
        assert_eq!(code(&output), 0, "replace: {}", stdout(&output));
    }

    fn paths_mit_ersatz(&self) -> Vec<String> {
        let mut paths = self.paths.clone();
        paths[1] = self.werkstatt.path("neu").display().to_string();
        paths
    }
}

#[test]
fn a_healthy_array_says_nothing_at_all() {
    // Der Normalfall, 364 Tage im Jahr. Ein Eintrag „Array in Ordnung" waere
    // der erste von 365 und machte das Tagebuch unlesbar.
    let array = Array::new("ruhig", true);

    for durchgang in 0..3 {
        let output = array.check(None);
        assert_eq!(code(&output), 0, "Durchgang {durchgang}");
        assert!(
            stdout(&output).contains("Unveraendert"),
            "Durchgang {durchgang}: {}",
            stdout(&output)
        );
    }

    assert!(
        array.werkstatt.meldungen().is_empty(),
        "ein gesundes Array hat gemeldet: {:?}",
        array.werkstatt.meldungen()
    );
    assert_eq!(
        array.werkstatt.journal(),
        "",
        "und hat etwas aufgeschrieben"
    );
}

#[test]
fn a_disk_that_drops_out_is_reported_once_and_not_again() {
    // Die eigentliche Zusage: einmal melden, dann schweigen. Ein Array, das
    // drei Wochen degradiert laeuft, schickt keine 21 Meldungen.
    let array = Array::new("ausfall", true);
    assert_eq!(code(&array.check(None)), 0);

    array.ersetze_slot_1();
    let paths = array.paths_mit_ersatz();
    let config = array.config.display().to_string();
    let mut argumente: Vec<&str> = vec!["check"];
    for path in &paths {
        argumente.push(path);
    }
    argumente.push("--config");
    argumente.push(&config);

    let erster = ferrite(&argumente);
    assert_eq!(code(&erster), 1, "degradiert ist Rueckgabewert 1");
    assert!(
        stdout(&erster).contains("aufgezeichnet und gemeldet"),
        "{}",
        stdout(&erster)
    );
    assert_eq!(
        array.werkstatt.meldungen().len(),
        1,
        "genau eine Meldung: {:?}",
        array.werkstatt.meldungen()
    );
    assert!(
        array.werkstatt.meldungen()[0].contains("degradiert"),
        "die Meldung sagt nicht, was los ist: {:?}",
        array.werkstatt.meldungen()
    );

    // Und jetzt drei Wochen lang dasselbe.
    for tag in 0..21 {
        let output = ferrite(&argumente);
        assert_eq!(code(&output), 1, "Tag {tag}");
        assert!(stdout(&output).contains("Unveraendert"), "Tag {tag}");
    }
    assert_eq!(
        array.werkstatt.meldungen().len(),
        1,
        "aus einem Ausfall wurden {} Meldungen",
        array.werkstatt.meldungen().len()
    );
}

#[test]
fn the_all_clear_is_a_message_too() {
    // Wer nach einem Rebuild keine Entwarnung bekommt, sieht so lange nach,
    // bis er aufhoert nachzusehen.
    let array = Array::new("entwarnung", true);
    array.ersetze_slot_1();

    let paths = array.paths_mit_ersatz();
    let config = array.config.display().to_string();
    let mut degradiert: Vec<&str> = vec!["check"];
    for path in &paths {
        degradiert.push(path);
    }
    degradiert.push("--config");
    degradiert.push(&config);

    assert_eq!(code(&ferrite(&degradiert)), 1);
    assert_eq!(array.werkstatt.meldungen().len(), 1);

    // Ein Rebuild macht den Slot wieder sauber.
    let mut rebuild: Vec<&str> = vec!["rebuild"];
    for path in &paths {
        rebuild.push(path);
    }
    rebuild.extend(["--slot", "1"]);
    let output = ferrite(&rebuild);
    assert_eq!(code(&output), 0, "rebuild: {}", stdout(&output));

    let entwarnung = ferrite(&degradiert);
    assert_eq!(code(&entwarnung), 0, "{}", stdout(&entwarnung));
    assert_eq!(
        array.werkstatt.meldungen().len(),
        2,
        "die Entwarnung kam nicht an: {:?}",
        array.werkstatt.meldungen()
    );
    assert!(
        array.werkstatt.meldungen()[1].contains("in Ordnung"),
        "die Entwarnung sagt nicht, dass wieder alles gut ist: {:?}",
        array.werkstatt.meldungen()
    );

    // Und danach wieder Ruhe.
    assert_eq!(code(&ferrite(&degradiert)), 0);
    assert_eq!(array.werkstatt.meldungen().len(), 2);
}

#[test]
fn without_a_journal_nothing_is_remembered_and_that_is_said() {
    // Ohne Tagebuch gibt es kein Gedaechtnis. Dann jeden Tag zu melden waere
    // die schlechtere Antwort als zu sagen, dass nichts verglichen werden
    // kann.
    let werkstatt = Werkstatt::new("ohne-tagebuch");
    let paths: Vec<String> = ["a", "b", "p", "log"]
        .iter()
        .map(|n| werkstatt.disk(n).display().to_string())
        .collect();
    std::fs::write(werkstatt.path("leer.conf"), "scan = /dev/null\n").expect("Konfiguration");

    let angelegt = ferrite(&[
        "create",
        "--data",
        &paths[0],
        "--data",
        &paths[1],
        "--parity-p",
        &paths[2],
        "--log",
        &paths[3],
        "--yes",
    ]);
    assert_eq!(code(&angelegt), 0);

    let config = werkstatt.path("leer.conf").display().to_string();
    let output = ferrite(&[
        "check", &paths[0], &paths[1], &paths[2], &paths[3], "--config", &config,
    ]);
    // Der Befund steht trotzdem da, und der Rueckgabewert stimmt.
    assert_eq!(code(&output), 0);
    assert!(stdout(&output).contains("Alle Members in Ordnung"));
    let gemeldet = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        gemeldet.contains("kein Tagebuch"),
        "der Grund fehlt: {gemeldet}"
    );
}

#[test]
fn the_journal_carries_the_change_and_the_summary_shows_it() {
    let array = Array::new("tagebuch", false);
    assert_eq!(code(&array.check(None)), 0);
    array.ersetze_slot_1();

    let paths = array.paths_mit_ersatz();
    let config = array.config.display().to_string();
    let mut argumente: Vec<&str> = vec!["check"];
    for path in &paths {
        argumente.push(path);
    }
    argumente.push("--config");
    argumente.push(&config);
    assert_eq!(code(&ferrite(&argumente)), 1);

    let journal = array.werkstatt.journal();
    assert!(
        journal.contains("zustand") && journal.contains("from=0 to=1"),
        "das Tagebuch traegt den Wechsel nicht: {journal}"
    );

    let summary = ferrite(&["journal", "--config", &config, "--json"]);
    let text = stdout(&summary);
    assert!(text.contains("\"health_changes\":1"), "{text}");
    assert!(text.contains("\"last_health\":1"), "{text}");
}

/// Die Gegenprobe zum Schweigen: Ohne `notify` in der Konfiguration darf auch
/// im Aenderungsfall nichts laufen — sonst wuerde ein Test, der nur zaehlt,
/// aus einem fehlenden Melder ein bestandenes Ergebnis machen.
#[test]
fn a_configuration_without_a_notify_command_reports_only_to_the_journal() {
    let array = Array::new("stumm", false);
    array.ersetze_slot_1();

    let paths = array.paths_mit_ersatz();
    let config = array.config.display().to_string();
    let mut argumente: Vec<&str> = vec!["check"];
    for path in &paths {
        argumente.push(path);
    }
    argumente.push("--config");
    argumente.push(&config);

    let output = ferrite(&argumente);
    assert_eq!(code(&output), 1);
    assert!(stdout(&output).contains("aufgezeichnet"));
    assert!(array.werkstatt.journal().contains("zustand"));
    assert!(
        !Path::new(&array.werkstatt.path("meldungen")).exists(),
        "ohne notify darf nichts gelaufen sein"
    );
}
