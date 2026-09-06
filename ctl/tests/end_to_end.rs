// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Das Werkzeug, wie ein Mensch es aufruft.
//!
//! Gestartet wird das echte Binary mit einer echten Kommandozeile, und
//! geprueft werden Rueckgabewert und Ausgabe. Die Unit-Tests pruefen die
//! Regeln; hier steht, ob daraus ein Werkzeug wird.
//!
//! **Ohne Root und ohne Kernel.** Die Geraete sind Dateien — `MemberDevice`
//! macht dabei keinen Unterschied, und der Superblock schon gar nicht. Damit
//! laeuft dieser Durchstich im normalen Testlauf mit und nicht nur in einem
//! Sonderjob, den man vergessen kann.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FERRITE: &str = env!("CARGO_BIN_EXE_ferrite");

/// Geraetedateien, die sich selbst wegraeumen.
struct Disks {
    directory: PathBuf,
}

impl Disks {
    fn new(name: &str) -> Self {
        let directory =
            std::env::temp_dir().join(format!("ferrite-ctl-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("Arbeitsverzeichnis");
        Disks { directory }
    }

    /// Eine Geraetedatei von 4 MiB.
    ///
    /// Gross genug fuer den Superblock bei 1 MiB, seine Sicherung am Ende und
    /// mehrere 64-KiB-Bloecke dazwischen.
    fn disk(&self, name: &str) -> PathBuf {
        let path = self.directory.join(name);
        let file = std::fs::File::create(&path).expect("Geraetedatei");
        file.set_len(4 << 20).expect("Groesse");
        path
    }
}

impl Drop for Disks {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn ferrite(arguments: &[&str]) -> Output {
    Command::new(FERRITE)
        .args(arguments)
        .output()
        .expect("ferrite starten")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn code(output: &Output) -> i32 {
    output.status.code().expect("Rueckgabewert")
}

/// Liegt an der Stelle, an der der Superblock steht, wirklich einer?
///
/// Kennung und Offset kommen aus dem Format und nicht aus diesem Test:
/// Abgeschriebene Konstanten pruefen irgendwann etwas anderes als das, was
/// dasteht.
fn has_superblock(path: &Path) -> bool {
    use ferrite_format::superblock::{SUPERBLOCK_MAGIC, SUPERBLOCK_PRIMARY_OFFSET};

    let bytes = std::fs::read(path).expect("Geraetedatei lesen");
    let at = SUPERBLOCK_PRIMARY_OFFSET as usize;
    bytes
        .get(at..at + SUPERBLOCK_MAGIC.len())
        .is_some_and(|found| found == SUPERBLOCK_MAGIC)
}

/// Ein Array aus zwei Data-Slots, P und Log, angelegt und bestaetigt.
fn created(disks: &Disks) -> Vec<PathBuf> {
    let paths: Vec<PathBuf> = ["a", "b", "p", "l"].iter().map(|n| disks.disk(n)).collect();
    let as_str: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();

    let output = ferrite(&[
        "create",
        "--data",
        &as_str[0],
        "--data",
        &as_str[1],
        "--parity-p",
        &as_str[2],
        "--log",
        &as_str[3],
        "--yes",
    ]);
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    paths
}

#[test]
fn create_writes_nothing_without_yes() {
    // Die wichtigste Eigenschaft dieses Kommandos. Wer sie kaputtmacht,
    // ueberschreibt beim ersten Tippfehler eine Platte mit Daten darauf.
    let disks = Disks::new("trockenlauf");
    let data = disks.disk("a");
    let parity = disks.disk("p");
    let log = disks.disk("l");

    let output = ferrite(&[
        "create",
        "--data",
        &data.display().to_string(),
        "--parity-p",
        &parity.display().to_string(),
        "--log",
        &log.display().to_string(),
    ]);

    assert_eq!(code(&output), 0, "ein Trockenlauf ist kein Fehler");
    assert!(stdout(&output).contains("nichts geschrieben"));
    for path in [&data, &parity, &log] {
        assert!(
            !has_superblock(path),
            "{} wurde beschrieben, obwohl --yes fehlte",
            path.display()
        );
    }
}

#[test]
fn the_dry_run_names_every_disk_it_would_touch() {
    let disks = Disks::new("plan");
    let data = disks.disk("a");
    let parity = disks.disk("p");
    let log = disks.disk("l");

    let output = ferrite(&[
        "create",
        "--data",
        &data.display().to_string(),
        "--parity-p",
        &parity.display().to_string(),
        "--log",
        &log.display().to_string(),
    ]);
    let text = stdout(&output);
    for path in [&data, &parity, &log] {
        assert!(
            text.contains(&path.display().to_string()),
            "{} fehlt im Plan",
            path.display()
        );
    }
    assert!(text.contains("4 MiB"), "die Groessen fehlen: {text}");
}

#[test]
fn create_with_yes_writes_the_superblocks() {
    let disks = Disks::new("anlegen");
    for path in created(&disks) {
        assert!(
            has_superblock(&path),
            "{} hat keinen Superblock",
            path.display()
        );
    }
}

#[test]
fn status_reads_back_what_create_wrote() {
    let disks = Disks::new("zustand");
    let paths = created(&disks);
    let as_str: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();

    let output = ferrite(&["status", &as_str[0], &as_str[1], &as_str[2], &as_str[3]]);
    let text = stdout(&output);
    assert_eq!(code(&output), 0, "{text}");
    assert!(text.contains("Slot 0"));
    assert!(text.contains("Slot 1"));
    assert!(text.contains("ParityP"));
    assert!(text.contains("Log"));
    assert!(text.contains("Alle Members in Ordnung"));
}

#[test]
fn a_missing_disk_is_reported_and_changes_the_exit_code() {
    // Der Fall, um dessentwillen es einen Rueckgabewert gibt: Ein Monitoring
    // liest keinen Text, es liest eine Zahl.
    let disks = Disks::new("fehlt");
    let paths = created(&disks);
    let as_str: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();

    let output = ferrite(&["status", &as_str[0], &as_str[2], &as_str[3]]);
    assert_eq!(code(&output), 2, "{}", stdout(&output));
    assert!(stdout(&output).contains("ergeben kein Array"));
}

#[test]
fn a_second_create_on_the_same_disks_is_refused() {
    // Der teuerste Aufruf ueberhaupt: Wer ein bestehendes Array versehentlich
    // neu anlegt, verliert alles darauf, und keine Paritaet hilft dagegen.
    let disks = Disks::new("zweimal");
    let paths = created(&disks);
    let as_str: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();

    let output = ferrite(&[
        "create",
        "--data",
        &as_str[0],
        "--data",
        &as_str[1],
        "--parity-p",
        &as_str[2],
        "--log",
        &as_str[3],
        "--yes",
    ]);
    assert_ne!(code(&output), 0);
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(
        message.contains("gehoert bereits zu Array"),
        "unerwartete Meldung: {message}"
    );
    assert!(message.contains("--force"), "der Ausweg muss dastehen");
}

#[test]
fn force_overwrites_an_existing_array() {
    let disks = Disks::new("force");
    let paths = created(&disks);
    let as_str: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();

    let before = stdout(&ferrite(&[
        "status", &as_str[0], &as_str[1], &as_str[2], &as_str[3],
    ]));

    let output = ferrite(&[
        "create",
        "--data",
        &as_str[0],
        "--data",
        &as_str[1],
        "--parity-p",
        &as_str[2],
        "--log",
        &as_str[3],
        "--yes",
        "--force",
    ]);
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let after = stdout(&ferrite(&[
        "status", &as_str[0], &as_str[1], &as_str[2], &as_str[3],
    ]));
    assert_ne!(
        before, after,
        "nach --force muss ein anderes Array dastehen — sonst hat es nichts getan"
    );
}

#[test]
fn a_disk_from_another_array_is_not_quietly_accepted() {
    let first = Disks::new("fremd-eins");
    let second = Disks::new("fremd-zwei");
    let mine = created(&first);
    let theirs = created(&second);

    let output = ferrite(&[
        "status",
        &mine[0].display().to_string(),
        &mine[1].display().to_string(),
        &mine[2].display().to_string(),
        &mine[3].display().to_string(),
        &theirs[0].display().to_string(),
    ]);
    assert_eq!(code(&output), 2);
    assert!(stdout(&output).contains("gehoert zu Array"));
}

#[test]
fn a_disk_without_a_superblock_is_named_and_not_ignored() {
    let disks = Disks::new("leer");
    let paths = created(&disks);
    let empty = disks.disk("leer");
    let mut line: Vec<String> = vec!["status".to_string()];
    line.extend(paths.iter().map(|p| p.display().to_string()));
    line.push(empty.display().to_string());
    let borrowed: Vec<&str> = line.iter().map(String::as_str).collect();

    let output = ferrite(&borrowed);
    assert_eq!(
        code(&output),
        0,
        "eine fremde Datei macht das Array nicht kaputt"
    );
    assert!(stdout(&output).contains("kein Ferrite-Superblock"));
}

#[test]
fn the_same_disk_in_two_roles_is_refused_before_anything_is_opened() {
    let disks = Disks::new("doppelrolle");
    let one = disks.disk("a").display().to_string();
    let log = disks.disk("l").display().to_string();

    let output = ferrite(&[
        "create",
        "--data",
        &one,
        "--parity-p",
        &one,
        "--log",
        &log,
        "--yes",
    ]);
    assert_ne!(code(&output), 0);
    assert!(String::from_utf8_lossy(&output.stderr).contains("mehrfach"));
    assert!(
        !has_superblock(Path::new(&one)),
        "es darf nichts geschrieben worden sein"
    );
}

#[test]
fn a_disk_that_is_too_small_stops_the_whole_create() {
    // Ein Array wird ganz oder gar nicht angelegt. Bliebe die Haelfte der
    // Superbloecke stehen, muesste sie jemand von Hand wegraeumen.
    let disks = Disks::new("zu-klein");
    let data = disks.disk("a").display().to_string();
    let parity = disks.disk("p").display().to_string();
    let tiny = disks.directory.join("winzig");
    std::fs::File::create(&tiny)
        .expect("Datei")
        .set_len(4096)
        .expect("Groesse");

    let output = ferrite(&[
        "create",
        "--data",
        &data,
        "--parity-p",
        &parity,
        "--log",
        &tiny.display().to_string(),
        "--yes",
    ]);
    assert_ne!(code(&output), 0);
    assert!(
        !has_superblock(Path::new(&data)),
        "die erste Platte wurde beschrieben, obwohl der Aufruf scheiterte"
    );
}

#[test]
fn help_and_version_answer_without_a_disk() {
    let help = ferrite(&["help"]);
    assert_eq!(code(&help), 0);
    assert!(stdout(&help).contains("ferrite create"));

    let version = ferrite(&["--version"]);
    assert_eq!(code(&version), 0);
    assert!(stdout(&version).starts_with("ferrite "));
}

#[test]
fn a_wrong_command_line_says_what_is_wrong_and_shows_the_help() {
    let output = ferrite(&["create", "--daten", "/dev/null"]);
    assert_eq!(code(&output), 64, "Bedienfehler haben ihren eigenen Wert");
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(message.contains("--daten"));
    assert!(message.contains("ferrite create"), "die Hilfe fehlt");
}
