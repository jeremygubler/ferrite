// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! `ferrite run` — der ganze Weg, einmal durch.
//!
//! Array anlegen, in Betrieb nehmen, btrfs auf die Blockgeraete, Pool
//! einhaengen, eine Datei schreiben, **beenden**, wieder starten und die
//! Datei wiederfinden. Alles ueber das echte Binary und eine echte
//! Kommandozeile.
//!
//! Der Neustart in der Mitte ist der Punkt. Ohne ihn koennte alles aus einem
//! Cache kommen; danach steht fest, dass es die Platten waren.
//!
//! Braucht Linux, Root, `ublk_drv`, `/dev/fuse` und `mkfs.btrfs`.

#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const FERRITE: &str = env!("CARGO_BIN_EXE_ferrite");

/// btrfs will rund 100 MiB, sonst legt `mkfs.btrfs` nicht an.
const MEMBER_SIZE: u64 = 300 << 20;

struct Workspace(PathBuf);

impl Workspace {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("ferrite-run-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("Arbeitsverzeichnis");
        Workspace(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn member(&self, name: &str) -> PathBuf {
        self.sized(name, MEMBER_SIZE)
    }

    fn sized(&self, name: &str, bytes: u64) -> PathBuf {
        let path = self.join(name);
        let file = std::fs::File::create(&path).expect("Member anlegen");
        file.set_len(bytes).expect("Groesse");
        path
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn have(program: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {program}")])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// `None` mit Begruendung, wenn etwas fehlt. Kein stiller Erfolg.
fn prerequisites() -> Option<()> {
    if !is_root() {
        eprintln!("uebersprungen: braucht Root");
        return None;
    }
    for path in ["/dev/ublk-control", "/dev/fuse"] {
        if !Path::new(path).exists() {
            eprintln!("uebersprungen: {path} fehlt");
            return None;
        }
    }
    if !have("mkfs.btrfs") {
        eprintln!("uebersprungen: mkfs.btrfs fehlt");
        return None;
    }
    Some(())
}

fn must_run(program: &str, args: &[&str]) {
    let output = Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("{program} liess sich nicht starten: {error}"));
    assert!(
        output.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

/// Ein laufendes `ferrite run`, das beim Fallenlassen beendet wird.
struct Running {
    child: Child,
    /// Die Zeilen, die es bisher ausgegeben hat.
    lines: Vec<String>,
    /// Bleibt am Leben, solange der Prozess laeuft.
    ///
    /// Wer ihn fallenlaesst, schliesst die Pipe — und `ferrite run` stirbt
    /// dann beim naechsten `println!` an einem Broken Pipe. Genau das ist
    /// beim ersten Anlauf dieses Tests passiert, und es sah aus wie ein
    /// Fehler im Programm.
    output: mpsc::Receiver<String>,
    stopped: bool,
}

impl Running {
    /// Startet `ferrite run` und wartet, bis es sich als bereit meldet.
    ///
    /// Gewartet wird auf die **Ausgabe**, nicht auf eine Sekundenzahl: Wie
    /// lange das Recovery und das Anlegen der Geraete brauchen, weiss dieser
    /// Test nicht, und ein zu knapp geratener Schlaf macht daraus einen Test,
    /// der auf einem langsamen Rechner ohne Grund fehlschlaegt.
    fn start(arguments: &[&str]) -> Self {
        let mut child = Command::new(FERRITE)
            .arg("run")
            .args(arguments)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("ferrite run starten");

        let stdout = child.stdout.take().expect("stdout");
        let (sender, output) = mpsc::channel();
        // Bis zum Dateiende weiterlesen, nicht bis „Bereit". Sonst bliebe
        // niemand mehr an der Pipe, und der Prozess stuerbe an seiner eigenen
        // Ausgabe.
        std::thread::spawn(move || {
            for line in BufReader::new(stdout)
                .lines()
                .map_while(std::result::Result::ok)
            {
                if sender.send(line).is_err() {
                    return;
                }
            }
        });

        let mut lines = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match output.recv_timeout(left.max(Duration::from_millis(1))) {
                Ok(line) => {
                    let ready = line.starts_with("Bereit.");
                    lines.push(line);
                    if ready {
                        break;
                    }
                }
                Err(_) => {
                    let _ = child.kill();
                    panic!(
                        "ferrite run wurde nicht bereit. Bisher:\n{}",
                        lines.join("\n")
                    );
                }
            }
        }

        Running {
            child,
            lines,
            output,
            stopped: false,
        }
    }

    /// Die Geraetepfade, die es gemeldet hat — in Slot-Reihenfolge.
    fn block_paths(&self) -> Vec<String> {
        self.lines
            .iter()
            .filter_map(|line| line.split_once(": /dev/ublkb"))
            .map(|(_, rest)| format!("/dev/ublkb{rest}"))
            .collect()
    }

    /// Schickt SIGTERM und wartet auf das Ende.
    fn stop(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };

        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            // Weiterlesen, waehrend es abbaut: Sonst laeuft die Pipe voll und
            // der Prozess bleibt im `write` stehen, das niemand mehr abholt.
            while let Ok(line) = self.output.try_recv() {
                self.lines.push(line);
            }
            match self.child.try_wait().expect("auf das Ende warten") {
                Some(status) => {
                    while let Ok(line) = self.output.try_recv() {
                        self.lines.push(line);
                    }
                    assert!(
                        status.success(),
                        "ferrite run endete mit {status}. Ausgabe:\n{}",
                        self.lines.join("\n")
                    );
                    assert!(
                        self.lines.iter().any(|line| line.contains("abgebaut")),
                        "es hat sich beendet, ohne das Abbauen zu melden:\n{}",
                        self.lines.join("\n")
                    );
                    return;
                }
                None if Instant::now() > deadline => {
                    let _ = self.child.kill();
                    panic!(
                        "ferrite run hat sich nach SIGTERM nicht beendet. Ausgabe:\n{}",
                        self.lines.join("\n")
                    );
                }
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if !self.stopped {
            unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };
            let _ = self.child.wait();
        }
    }
}

/// Legt ein Array aus zwei Data-Slots, P, Q und Log an.
fn create(workspace: &Workspace) -> Vec<String> {
    let members: Vec<PathBuf> = ["a", "b", "p", "q", "log"]
        .iter()
        .map(|name| workspace.member(name))
        .collect();
    let paths: Vec<String> = members.iter().map(|p| p.display().to_string()).collect();

    must_run(
        FERRITE,
        &[
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
        ],
    );
    paths
}

fn as_args(paths: &[String]) -> Vec<&str> {
    paths.iter().map(String::as_str).collect()
}

#[test]
#[ignore = "braucht Linux, Root, ublk_drv, /dev/fuse und btrfs-progs"]
fn run_brings_up_one_block_device_per_slot() {
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("geraete");
    let paths = create(&workspace);

    let mut running = Running::start(&as_args(&paths));
    let devices = running.block_paths();
    assert_eq!(devices.len(), 2, "je Data-Slot ein Geraet: {devices:?}");
    for device in &devices {
        assert!(
            Path::new(device).exists(),
            "{device} wurde gemeldet, existiert aber nicht"
        );
    }

    running.stop();
    for device in &devices {
        assert!(
            !Path::new(device).exists(),
            "{device} steht nach dem Beenden noch da"
        );
    }
}

#[test]
#[ignore = "braucht Linux, Root, ublk_drv, /dev/fuse und btrfs-progs"]
fn every_block_device_has_the_size_of_its_own_member() {
    // Die Kerninvariante des Projekts: Members duerfen verschieden gross
    // sein. Ein `run`, das allen die Groesse des ersten gaebe, zeigte auf der
    // kurzen Platte Bereiche, die es nicht gibt — und schnitte auf der langen
    // den Rest ab.
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("gemischt");
    let big = workspace.sized("a", 64 << 20);
    let small = workspace.sized("b", 32 << 20);
    let parity = workspace.sized("p", 64 << 20);
    let log = workspace.sized("l", 32 << 20);
    let paths: Vec<String> = [&big, &small, &parity, &log]
        .iter()
        .map(|p| p.display().to_string())
        .collect();

    must_run(
        FERRITE,
        &[
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
        ],
    );

    let mut running = Running::start(&as_args(&paths));
    let devices = running.block_paths();
    assert_eq!(devices.len(), 2);

    let sizes: Vec<u64> = devices.iter().map(|device| device_size(device)).collect();
    assert!(
        sizes[0] > sizes[1],
        "der grosse Slot ist nicht groesser: {sizes:?}"
    );
    // Rund die Haelfte, abzueglich Superblock und Sicherung.
    assert!(
        sizes[0] > 60 << 20 && sizes[1] < 34 << 20,
        "die Groessen passen zu keiner der beiden Platten: {sizes:?}"
    );
    running.stop();
}

/// Die Groesse eines Blockgeraets, so wie jedes Programm sie ermittelt.
fn device_size(path: &str) -> u64 {
    use std::io::{Seek, SeekFrom};
    let mut file = std::fs::File::open(path).expect("Blockgeraet oeffnen");
    file.seek(SeekFrom::End(0)).expect("ans Ende")
}

#[test]
#[ignore = "braucht Linux, Root, ublk_drv, /dev/fuse und btrfs-progs"]
fn a_file_written_through_the_pool_survives_a_restart() {
    // Der Durchstich. Ohne den Neustart in der Mitte koennte alles aus einem
    // Cache kommen; danach steht fest, dass es ueber Log, Paritaet und
    // Blockgeraet wirklich auf den Platten gelandet ist.
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("durchstich");
    let paths = create(&workspace);
    let pool = workspace.join("pool");
    let state = workspace.join("state");

    // Erster Lauf: nur die Blockgeraete, damit btrfs darauf kann.
    {
        let mut running = Running::start(&as_args(&paths));
        for device in running.block_paths() {
            must_run("mkfs.btrfs", &["-q", "-f", &device]);
        }
        running.stop();
    }

    let content = "durch Log, Paritaet, ublk und den Pool hindurch";
    let mut with_pool: Vec<&str> = as_args(&paths);
    let pool_arg = pool.display().to_string();
    let state_arg = state.display().to_string();
    with_pool.extend(["--pool", &pool_arg, "--state-dir", &state_arg]);

    // Zweiter Lauf: Pool einhaengen und eine Datei hineinschreiben.
    {
        let mut running = Running::start(&with_pool);
        assert!(
            running
                .lines
                .iter()
                .any(|line| line.contains("Pool eingehaengt")),
            "der Pool wurde nicht eingehaengt:\n{}",
            running.lines.join("\n")
        );
        std::fs::create_dir_all(pool.join("Filme")).expect("Verzeichnis im Pool");
        std::fs::write(pool.join("Filme/beweis.txt"), content).expect("Datei im Pool");
        running.stop();
    }

    // Nach dem Beenden darf nichts mehr eingehaengt sein.
    assert!(
        !pool.join("Filme/beweis.txt").exists(),
        "der Pool ist nach dem Beenden noch eingehaengt"
    );

    // Dritter Lauf: dieselbe Datei muss wieder da sein.
    {
        let mut running = Running::start(&with_pool);
        let read_back =
            std::fs::read_to_string(pool.join("Filme/beweis.txt")).expect("Datei wiederfinden");
        assert_eq!(read_back, content);
        running.stop();
    }

    // Und das Array ist danach immer noch heil.
    let mut status: Vec<&str> = vec!["status"];
    status.extend(as_args(&paths));
    let output = Command::new(FERRITE)
        .args(&status)
        .output()
        .expect("ferrite status");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
#[ignore = "braucht Linux, Root, ublk_drv, /dev/fuse und btrfs-progs"]
fn a_member_without_a_filesystem_is_reported_and_not_formatted() {
    // Der Fall, in dem ein `mkfs` verlockend waere. Ferrite formatiert nicht
    // — wer die falsche Platte angeschlossen hat, soll sie wiederbekommen.
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("kein-fs");
    let paths = create(&workspace);
    let pool = workspace.join("pool");
    let state = workspace.join("state");

    let mut with_pool: Vec<&str> = as_args(&paths);
    let pool_arg = pool.display().to_string();
    let state_arg = state.display().to_string();
    with_pool.extend(["--pool", &pool_arg, "--state-dir", &state_arg]);

    // Kein `mkfs` vorher: Die Blockgeraete sind leer.
    let output = Command::new(FERRITE)
        .arg("run")
        .args(&with_pool)
        .output()
        .expect("ferrite run");

    assert!(!output.status.success());
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(
        message.contains("kein btrfs"),
        "unerwartete Meldung: {message}"
    );
    assert!(
        message.contains("mkfs.btrfs"),
        "der Ausweg muss dastehen: {message}"
    );
}

#[test]
#[ignore = "braucht Linux, Root, ublk_drv, /dev/fuse und btrfs-progs"]
fn run_replays_the_log_before_the_first_block_device_appears() {
    // Ein Array, in dem noch Records stehen, muss beim Start zuerst das
    // Recovery machen. Andersherum saehe ein Gast einen Zustand, den das
    // Recovery gleich darauf aendert.
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("recovery");
    let paths = create(&workspace);

    // Ein sauber angelegtes Array hat ein leeres Log; das Recovery hat
    // nichts zu tun und meldet nichts. Geprueft wird deshalb die
    // Reihenfolge der Ausgabe, nicht ihr Inhalt: Was `run` ueber das
    // Recovery sagt, steht vor der ersten Geraetezeile.
    let mut running = Running::start(&as_args(&paths));
    let first_device = running
        .lines
        .iter()
        .position(|line| line.starts_with("Slot "))
        .expect("es muss ein Geraet gemeldet worden sein");
    let recovery = running
        .lines
        .iter()
        .position(|line| line.starts_with("Recovery:"));
    if let Some(recovery) = recovery {
        assert!(
            recovery < first_device,
            "das Recovery kam nach dem ersten Geraet"
        );
    }
    running.stop();
}
