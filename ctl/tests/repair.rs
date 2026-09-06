// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Der Reparaturablauf, wie ein Betreiber ihn geht.
//!
//! Eine Platte faellt aus, eine neue kommt hinein, der Rebuild fuellt sie, und
//! der Scrub bestaetigt, dass danach alles zusammenpasst. Ueber das echte
//! Binary, mit echten Superbloecken — und **ohne Root und ohne Kernel**, weil
//! keiner dieser Schritte ein Blockgeraet braucht.
//!
//! # Woher die Daten kommen
//!
//! Der Inhalt wird direkt in die Payload-Region eines Data-Members
//! geschrieben, am Schreibpfad vorbei. Danach ist die Paritaet veraltet —
//! genau die Lage, die ein Geraet hinterlaesst, das seinen Flush belogen hat
//! (Abschnitt 5.3). `ferrite scrub --repair` bildet sie neu, und erst dann ist
//! der Rebuild ueberhaupt in der Lage, den Inhalt wiederzufinden. Der Ablauf
//! prueft also beide Kommandos gegeneinander.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

use ferrite_format::superblock::{
    DEFAULT_PAYLOAD_OFFSET, SUPERBLOCK_MAGIC, SUPERBLOCK_PRIMARY_OFFSET,
};

const FERRITE: &str = env!("CARGO_BIN_EXE_ferrite");
/// Gross genug fuer mehrere 64-KiB-Bloecke, klein genug fuer einen schnellen
/// Test.
const DISK: u64 = 8 << 20;

struct Disks(PathBuf);

impl Disks {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("ferrite-repair-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("Arbeitsverzeichnis");
        Disks(path)
    }

    fn disk(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        let file = std::fs::File::create(&path).expect("Geraetedatei");
        file.set_len(DISK).expect("Groesse");
        path
    }
}

impl Drop for Disks {
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

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Ein Array aus zwei Data-Slots, P, Q und Log.
struct Array {
    disks: Disks,
    paths: Vec<String>,
}

impl Array {
    fn new(name: &str) -> Self {
        let disks = Disks::new(name);
        let paths: Vec<String> = ["a", "b", "p", "q", "log"]
            .iter()
            .map(|n| disks.disk(n).display().to_string())
            .collect();

        let output = ferrite(&[
            "create",
            "--data",
            &paths[0],
            "--data",
            &paths[1],
            "--parity-p",
            &paths[2],
            "--parity-q",
            &paths[3],
            "--log",
            &paths[4],
            "--yes",
        ]);
        assert_eq!(code(&output), 0, "{}", stderr(&output));
        Array { disks, paths }
    }

    /// Die Geraete des Arrays als Argumente.
    fn devices(&self) -> Vec<&str> {
        self.paths.iter().map(String::as_str).collect()
    }

    /// Dieselbe Liste ohne den angegebenen Data-Slot.
    fn without(&self, slot: usize) -> Vec<&str> {
        self.paths
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != slot)
            .map(|(_, path)| path.as_str())
            .collect()
    }
}

/// Schreibt Bytes direkt in die Payload-Region — am Schreibpfad vorbei.
fn put(path: &str, offset: u64, bytes: &[u8]) {
    use std::io::{Seek, SeekFrom, Write};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("Geraetedatei oeffnen");
    file.seek(SeekFrom::Start(DEFAULT_PAYLOAD_OFFSET + offset))
        .expect("positionieren");
    file.write_all(bytes).expect("schreiben");
}

/// Liest Bytes direkt aus der Payload-Region.
fn get(path: &str, offset: u64, len: usize) -> Vec<u8> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).expect("Geraetedatei oeffnen");
    file.seek(SeekFrom::Start(DEFAULT_PAYLOAD_OFFSET + offset))
        .expect("positionieren");
    let mut bytes = vec![0u8; len];
    file.read_exact(&mut bytes).expect("lesen");
    bytes
}

fn pattern(marker: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| (index as u8).wrapping_mul(37) ^ marker)
        .collect()
}

// --- Scrub ----------------------------------------------------------------

#[test]
fn a_fresh_array_scrubs_clean() {
    let array = Array::new("frisch");
    let mut line = vec!["scrub"];
    line.extend(array.devices());

    let output = ferrite(&line);
    assert_eq!(code(&output), 0, "{}", stdout(&output));
    assert!(stdout(&output).contains("passt zu allen Data-Members"));
}

#[test]
fn a_stale_parity_is_found_and_named_block_by_block() {
    // Genau die Lage, die ein Geraet hinterlaesst, das seinen Flush belogen
    // hat: Auf dem Data-Member steht der neue Inhalt, in der Paritaet der
    // alte.
    let array = Array::new("veraltet");
    put(&array.paths[0], 0, &pattern(1, 4096));

    let mut line = vec!["scrub"];
    line.extend(array.devices());
    let output = ferrite(&line);

    assert_eq!(code(&output), 1, "ein Befund ist kein Erfolg");
    let text = stdout(&output);
    assert!(text.contains("passen nicht"), "{text}");
    assert!(
        text.contains("\n  0\n") || text.contains("\n  0 "),
        "der betroffene Block muss genannt werden: {text}"
    );
    assert!(text.contains("nichts geaendert"));
}

#[test]
fn scrub_without_repair_changes_nothing() {
    let array = Array::new("nur-melden");
    put(&array.paths[0], 0, &pattern(1, 4096));
    let before = get(&array.paths[2], 0, 4096);

    let mut line = vec!["scrub"];
    line.extend(array.devices());
    ferrite(&line);

    assert_eq!(
        get(&array.paths[2], 0, 4096),
        before,
        "ohne --repair darf die Paritaet unberuehrt bleiben"
    );
}

#[test]
fn scrub_with_repair_makes_the_array_consistent_again() {
    let array = Array::new("reparieren");
    put(&array.paths[0], 0, &pattern(1, 4096));

    let mut repair = vec!["scrub"];
    repair.extend(array.devices());
    repair.push("--repair");
    let output = ferrite(&repair);
    assert_eq!(code(&output), 1, "der Befund bleibt ein Befund");
    assert!(stdout(&output).contains("neu gebildet"));

    // Und danach ist es sauber.
    let mut check = vec!["scrub"];
    check.extend(array.devices());
    let output = ferrite(&check);
    assert_eq!(code(&output), 0, "{}", stdout(&output));
}

// --- Der ganze Ablauf ------------------------------------------------------

#[test]
fn a_replaced_disk_gets_its_content_back() {
    // Der Ablauf, um den es geht: Platte weg, neue rein, Rebuild, und der
    // Inhalt ist wieder da. Gelesen wird zum Schluss **direkt von der
    // Platte**, nicht ueber die Rekonstruktion — sonst saehe der Test
    // dasselbe, auch wenn der Rebuild nichts geschrieben haette.
    let array = Array::new("ersatz");
    let content = pattern(0xC3, 8192);
    put(&array.paths[0], 0, &content);

    let mut repair = vec!["scrub"];
    repair.extend(array.devices());
    repair.push("--repair");
    assert_eq!(code(&ferrite(&repair)), 1);

    // Slot 0 faellt aus. Die neue Platte ist leer.
    let fresh = array.disks.disk("neu").display().to_string();
    let mut line = vec!["replace"];
    line.extend(array.without(0));
    line.extend(["--slot", "0", "--with", &fresh, "--yes"]);
    let output = ferrite(&line);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(stdout(&output).contains("Aufgenommen"));

    // Jetzt laeuft das Array degradiert.
    let mut status = vec!["status"];
    status.extend(array.without(0));
    status.push(&fresh);
    let output = ferrite(&status);
    assert_eq!(code(&output), 1, "{}", stdout(&output));
    assert!(stdout(&output).contains("wartet auf Rebuild"));

    // Der Rebuild.
    let mut line = vec!["rebuild"];
    line.extend(array.without(0));
    line.push(&fresh);
    line.extend(["--slot", "0"]);
    let output = ferrite(&line);
    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(stdout(&output).contains("wiederhergestellt"));

    // Und der Inhalt steht wieder auf der Platte — roh gelesen.
    assert_eq!(
        get(&fresh, 0, content.len()),
        content,
        "der Rebuild hat den Inhalt nicht zurueckgeholt"
    );

    // Danach ist alles wieder heil.
    let output = ferrite(&status);
    assert_eq!(code(&output), 0, "{}", stdout(&output));
}

#[test]
fn a_rebuild_without_a_replacement_has_nothing_to_do() {
    let array = Array::new("nichts-zu-tun");
    let mut line = vec!["rebuild"];
    line.extend(array.devices());
    line.extend(["--slot", "0"]);

    let output = ferrite(&line);
    assert_eq!(code(&output), 0);
    assert!(stdout(&output).contains("nichts wiederherzustellen"));
}

// --- Was `replace` ablehnt -------------------------------------------------

#[test]
fn replace_writes_nothing_without_yes() {
    let array = Array::new("trockenlauf");
    let fresh = array.disks.disk("neu").display().to_string();

    let mut line = vec!["replace"];
    line.extend(array.without(0));
    line.extend(["--slot", "0", "--with", &fresh]);
    let output = ferrite(&line);

    assert_eq!(code(&output), 0);
    assert!(stdout(&output).contains("nichts geschrieben"));
    let bytes = std::fs::read(&fresh).expect("lesen");
    let at = SUPERBLOCK_PRIMARY_OFFSET as usize;
    assert_ne!(
        &bytes[at..at + SUPERBLOCK_MAGIC.len()],
        SUPERBLOCK_MAGIC,
        "ohne --yes darf nichts geschrieben werden"
    );
}

#[test]
fn replace_refuses_a_slot_that_is_still_there() {
    // Wer die alte Platte in der Liste laesst, will vermutlich etwas anderes
    // — und bekaeme sonst ein Array mit zwei Members fuer denselben Slot.
    let array = Array::new("noch-besetzt");
    let fresh = array.disks.disk("neu").display().to_string();

    let mut line = vec!["replace"];
    line.extend(array.devices());
    line.extend(["--slot", "0", "--with", &fresh, "--yes"]);
    let output = ferrite(&line);

    assert_ne!(code(&output), 0);
    assert!(
        stderr(&output).contains("noch besetzt"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn replace_refuses_a_disk_that_belongs_to_another_array() {
    let mine = Array::new("meins");
    let theirs = Array::new("fremd");

    let mut line = vec!["replace"];
    line.extend(mine.without(0));
    line.extend(["--slot", "0", "--with", &theirs.paths[0], "--yes"]);
    let output = ferrite(&line);

    assert_ne!(code(&output), 0);
    assert!(stderr(&output).contains("gehoert bereits zu Array"));
    assert!(stderr(&output).contains("--force"));
}

#[test]
fn replace_refuses_a_slot_the_array_does_not_have() {
    let array = Array::new("kein-slot");
    let fresh = array.disks.disk("neu").display().to_string();

    let mut line = vec!["replace"];
    line.extend(array.without(0));
    line.extend(["--slot", "7", "--with", &fresh, "--yes"]);
    let output = ferrite(&line);

    assert_ne!(code(&output), 0);
    assert!(stderr(&output).contains("gibt es in dem Array nicht"));
}

#[test]
fn a_bigger_replacement_is_taken_but_capped_at_the_parity() {
    // Eine passende Platte zu finden ist Jahre spaeter schwer. Eine groessere
    // wird deshalb angenommen — ihr Ueberhang bleibt nur ungenutzt, denn
    // jenseits von ParityP gaebe es keine Redundanz.
    let array = Array::new("groesser");
    let big = array.disks.0.join("gross");
    let file = std::fs::File::create(&big).expect("Datei");
    file.set_len(DISK * 4).expect("Groesse");
    let big = big.display().to_string();

    let mut line = vec!["replace"];
    line.extend(array.without(0));
    line.extend(["--slot", "0", "--with", &big, "--yes"]);
    let output = ferrite(&line);

    assert_eq!(code(&output), 0, "{}", stderr(&output));
    assert!(
        stdout(&output).contains("ParityP ist kuerzer"),
        "der ungenutzte Rest muss genannt werden: {}",
        stdout(&output)
    );

    // Und das Ergebnis ist ein gueltiges Array.
    let mut status = vec!["status"];
    status.extend(array.without(0));
    status.push(&big);
    assert_eq!(
        code(&ferrite(&status)),
        1,
        "degradiert, aber zusammensetzbar"
    );
}

#[test]
fn scrub_refuses_to_repair_while_a_member_is_missing() {
    // Eine Paritaet ueber einen unbrauchbaren Member zu bilden hiesse, die
    // Rekonstruktion aufzugeben — danach waere der Rebuild unmoeglich.
    let array = Array::new("degradiert");

    // Zuerst eine Abweichung erzeugen — ein degradiertes Array, das stimmig
    // ist, hat nichts zu reparieren, und die Ablehnung kaeme nie dran.
    put(&array.paths[1], 0, &pattern(0x5A, 4096));

    let fresh = array.disks.disk("neu").display().to_string();
    let mut line = vec!["replace"];
    line.extend(array.without(0));
    line.extend(["--slot", "0", "--with", &fresh, "--yes"]);
    assert_eq!(code(&ferrite(&line)), 0);

    let mut scrub = vec!["scrub"];
    scrub.extend(array.without(0));
    scrub.push(&fresh);
    scrub.push("--repair");
    let output = ferrite(&scrub);

    assert_eq!(
        code(&output),
        2,
        "die Reparatur muss abgelehnt werden: {}",
        stdout(&output)
    );
    assert!(
        stdout(&output).contains("passen nicht"),
        "die Abweichung muss trotzdem gemeldet werden: {}",
        stdout(&output)
    );
}

#[test]
fn a_degraded_array_can_still_be_checked() {
    // Pruefen geht auch mit einem fehlenden Member: Der wird aus P
    // rekonstruiert und das Ergebnis gegen Q gehalten. Nur reparieren geht
    // nicht.
    let array = Array::new("degradiert-pruefen");
    let fresh = array.disks.disk("neu").display().to_string();

    let mut line = vec!["replace"];
    line.extend(array.without(0));
    line.extend(["--slot", "0", "--with", &fresh, "--yes"]);
    assert_eq!(code(&ferrite(&line)), 0);

    let mut scrub = vec!["scrub"];
    scrub.extend(array.without(0));
    scrub.push(&fresh);
    let output = ferrite(&scrub);
    assert_eq!(
        code(&output),
        0,
        "ein degradiertes, aber stimmiges Array ist stimmig: {}",
        stdout(&output)
    );
}

// --- Konfiguration und Suche ----------------------------------------------

#[test]
fn status_finds_the_array_through_the_configuration() {
    // Der Fall, fuer den die systemd-Unit gemacht ist: kein Argument ausser
    // der Konfiguration. Die Platten stehen zweimal im Suchverzeichnis, wie
    // unter /dev/disk/by-id.
    let array = Array::new("suchen");
    let by_id = array.disks.0.join("by-id");
    std::fs::create_dir_all(&by_id).expect("Verzeichnis");
    for (index, path) in array.paths.iter().enumerate() {
        std::os::unix::fs::symlink(path, by_id.join(format!("ata-{index}"))).expect("Symlink");
        std::os::unix::fs::symlink(path, by_id.join(format!("wwn-{index}"))).expect("Symlink");
    }

    let config = array.disks.0.join("ferrite.conf");
    std::fs::write(&config, format!("scan = {}\n", by_id.display())).expect("Konfiguration");
    let config = config.display().to_string();

    let output = ferrite(&["status", "--config", &config]);
    assert_eq!(code(&output), 0, "{}{}", stdout(&output), stderr(&output));
    assert!(stdout(&output).contains("Alle Members in Ordnung"));
    // Fuenf Members, nicht zehn: Die zweiten Namen sind entdoppelt.
    assert_eq!(
        stdout(&output).matches("/by-id/").count(),
        5,
        "die doppelten Namen wurden nicht entdoppelt: {}",
        stdout(&output)
    );
}

#[test]
fn scrub_finds_the_array_through_the_configuration() {
    let array = Array::new("suchen-scrub");
    let by_id = array.disks.0.join("by-id");
    std::fs::create_dir_all(&by_id).expect("Verzeichnis");
    for (index, path) in array.paths.iter().enumerate() {
        std::os::unix::fs::symlink(path, by_id.join(format!("ata-{index}"))).expect("Symlink");
    }
    let config = array.disks.0.join("ferrite.conf");
    std::fs::write(&config, format!("scan = {}\n", by_id.display())).expect("Konfiguration");
    let config = config.display().to_string();

    let output = ferrite(&["scrub", "--config", &config]);
    assert_eq!(code(&output), 0, "{}{}", stdout(&output), stderr(&output));
    assert!(stdout(&output).contains("passt zu allen Data-Members"));
}

#[test]
fn a_typo_in_the_configuration_stops_the_command() {
    // Ein stillschweigend ignorierter Schluessel laesst den Betreiber
    // glauben, seine Einstellung gelte.
    let array = Array::new("tippfehler");
    let config = array.disks.0.join("ferrite.conf");
    std::fs::write(&config, "scaan = /dev\n").expect("Konfiguration");
    let config = config.display().to_string();

    let output = ferrite(&["status", "--config", &config]);
    assert_ne!(code(&output), 0);
    let message = format!("{}{}", stdout(&output), stderr(&output));
    assert!(message.contains("scaan"), "{message}");
    assert!(message.contains("Zeile 1"), "{message}");
}

#[test]
fn a_named_configuration_that_is_missing_is_an_error() {
    // Eine fehlende Datei am ueblichen Ort ist keiner. Eine ausdruecklich
    // benannte schon: Wer `--config` schreibt, meint eine bestimmte.
    let output = ferrite(&["status", "--config", "/gibt/es/nicht.conf"]);
    assert_ne!(code(&output), 0);
}

#[test]
fn two_arrays_without_a_name_are_refused_instead_of_guessed() {
    // Eines zu waehlen hiesse raten, und die falsche Wahl haenge ein fremdes
    // Array ein.
    let first = Array::new("zwei-a");
    let second = Array::new("zwei-b");
    let by_id = first.disks.0.join("by-id");
    std::fs::create_dir_all(&by_id).expect("Verzeichnis");
    for (index, path) in first.paths.iter().chain(second.paths.iter()).enumerate() {
        std::os::unix::fs::symlink(path, by_id.join(format!("d-{index}"))).expect("Symlink");
    }
    let config = first.disks.0.join("ferrite.conf");
    std::fs::write(&config, format!("scan = {}\n", by_id.display())).expect("Konfiguration");
    let config = config.display().to_string();

    let output = ferrite(&["status", "--config", &config]);
    assert_ne!(code(&output), 0);
    let message = format!("{}{}", stdout(&output), stderr(&output));
    assert!(message.contains("mehrere Arrays"), "{message}");
    assert!(
        message.contains("array = "),
        "der Ausweg muss dastehen: {message}"
    );
}

#[test]
fn check_flush_is_honest_about_a_file() {
    // Eine gewoehnliche Datei kann die Frage nicht beantworten, und der Test
    // sagt das — statt „ja" zu raten.
    let array = Array::new("flush");
    let output = ferrite(&["check-flush", &array.paths[4]]);

    assert_eq!(code(&output), 1, "kein Honest fuer eine Datei");
    let text = stdout(&output);
    assert!(text.contains("RegularFile"), "{text}");
    assert!(text.contains("Undecidable"), "{text}");
    assert!(text.contains("Write-Through"), "{text}");
}
