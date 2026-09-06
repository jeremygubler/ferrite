// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Selbstheilung, einmal echt: btrfs findet den Rost, Ferrite repariert ihn.
//!
//! # Was hier zusammenkommt
//!
//! Alle Schichten des Projekts auf einmal, ohne eine Attrappe dazwischen:
//!
//! 1. Ein Loop-Geraet je Member, darauf Superbloecke und ein Write-Log.
//! 2. Ein ublk-Geraet fuer Data-Slot 0, dahinter der Schreibpfad mit P und Q.
//! 3. Ein **echtes btrfs** auf diesem Geraet, mit einer Datei darin.
//! 4. Rost: Bytes auf der Platte kippen, am Schreibpfad vorbei — so, wie es
//!    eine alternde Platte tut.
//! 5. Ein **echter btrfs-Scrub**, der ihn findet und in den Kernel-Ringpuffer
//!    schreibt.
//! 6. Der Broker liest den Puffer, ordnet zu, rekonstruiert, prueft gegen und
//!    schreibt zurueck.
//! 7. Die Datei ist wieder lesbar, und ein zweiter Scrub findet nichts mehr.
//!
//! Das ist der Satz aus dem README — „Meldet btrfs einen korrupten Block,
//! rekonstruiert der Repair-Broker ihn aus der Paritaet und schreibt ihn
//! zurueck" — als Ablauf statt als Behauptung.
//!
//! # Warum er scheitern darf
//!
//! Der Parser in `ferrite_broker::btrfs` liest Text, den der Kernel
//! formuliert. Aendert eine Kernelversion die Formulierung, findet dieser Test
//! keine Meldung mehr und schlaegt fehl. Das ist erwuenscht: Genau dann
//! repariert der Broker im Betrieb naemlich auch nichts mehr, und ein Test,
//! der das gruen durchgehen liesse, waere schlimmer als keiner.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use ferrite_broker::kmsg::KmsgReader;
use ferrite_broker::{Classified, DamageReport, RepairBroker, SlotMap};
use ferrite_engine::ublk::{ArraySlot, UblkDevice, UblkSpec, CONTROL_PATH};
use ferrite_engine::{member_for, ArrayWriter, DeviceLog, Member, MemberDevice, Repair};
use ferrite_format::superblock::{Role, Superblock, DEFAULT_PAYLOAD_OFFSET};
use ferrite_format::Uuid;

/// btrfs will rund 100 MiB, sonst legt `mkfs.btrfs` nicht an.
const PAYLOAD: u64 = 300 << 20;
/// Reichlich: Der Ring traegt die Nutzdaten jedes Writes mit.
const LOG_PAYLOAD: u64 = 16 << 20;
const SLOTS: u16 = 2;

/// Die Datei im btrfs. Gross genug fuer einen eigenen Extent, damit btrfs sie
/// mit Pruefsummen ablegt statt sie in die Metadaten zu packen.
const FILE_SIZE: usize = 1 << 20;

// --- Voraussetzungen ------------------------------------------------------

fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim() == "0")
        .unwrap_or(false)
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
    if !Path::new(CONTROL_PATH).exists() {
        eprintln!("uebersprungen: {CONTROL_PATH} fehlt — ublk_drv nicht geladen");
        return None;
    }
    if !Path::new("/dev/kmsg").exists() {
        eprintln!("uebersprungen: /dev/kmsg fehlt");
        return None;
    }
    for program in ["mkfs.btrfs", "btrfs", "mount", "umount", "losetup"] {
        if !have(program) {
            eprintln!("uebersprungen: {program} fehlt");
            return None;
        }
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

/// Ein Loop-Geraet, das sich selbst wieder abbaut.
struct LoopDevice {
    device: PathBuf,
    backing: PathBuf,
}

impl LoopDevice {
    fn create(name: &str, payload: u64) -> Option<Self> {
        let backing = std::env::temp_dir().join(format!("ferrite-selbstheilung-{name}.img"));
        let _ = std::fs::remove_file(&backing);
        let file = std::fs::File::create(&backing).ok()?;
        file.set_len(DEFAULT_PAYLOAD_OFFSET + payload + 65_536)
            .ok()?;
        drop(file);

        let output = Command::new("losetup")
            .args(["--show", "--find"])
            .arg(&backing)
            .output()
            .ok()?;
        if !output.status.success() {
            eprintln!(
                "uebersprungen: losetup fehlgeschlagen: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            let _ = std::fs::remove_file(&backing);
            return None;
        }
        Some(LoopDevice {
            device: PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string()),
            backing,
        })
    }

    fn path(&self) -> &Path {
        &self.device
    }
}

impl Drop for LoopDevice {
    fn drop(&mut self) {
        let _ = Command::new("losetup").arg("-d").arg(&self.device).status();
        let _ = std::fs::remove_file(&self.backing);
    }
}

/// Wartet, bis udev den Geraeteknoten angelegt hat.
fn wait_for(path: &str) -> bool {
    for _ in 0..100 {
        if Path::new(path).exists() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

fn drop_caches() {
    let _ = Command::new("sync").status();
    let _ = std::fs::write("/proc/sys/vm/drop_caches", b"3");
}

fn superblock(role: Role, slot_index: u16, payload: u64) -> Superblock {
    let mut superblock = Superblock::new(
        Uuid::from_random_bytes([0x7B; 16]),
        Uuid::from_random_bytes([role as u8 * 16 + slot_index as u8 + 1; 16]),
        role,
        u32::from(SLOTS),
        payload,
    );
    superblock.slot_index = slot_index;
    superblock
}

/// Der Inhalt der Datei. Erkennbar und ohne kurze Wiederholung, damit er sich
/// auf der Platte eindeutig wiederfinden laesst.
fn file_content() -> Vec<u8> {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    (0..FILE_SIZE)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u8
        })
        .collect()
}

/// Sucht einen Abschnitt im Payload eines Members.
///
/// Wo btrfs die Datei ablegt, entscheidet btrfs — also wird nachgesehen statt
/// gerechnet. Ein Test, der die Stelle vorherzusagen versuchte, pruefte
/// hinterher die Vorhersage und nicht die Reparatur.
fn find_in_payload(device: &MemberDevice, needle: &[u8], payload: u64) -> Option<u64> {
    const CHUNK: usize = 4 << 20;
    let mut buffer = vec![0u8; CHUNK + needle.len()];
    let mut position = 0u64;

    while position < payload {
        let len = (CHUNK + needle.len()).min((payload - position) as usize);
        device
            .read_at(DEFAULT_PAYLOAD_OFFSET + position, &mut buffer[..len])
            .expect("Payload lesen");
        // Erst das erste Byte suchen und nur bei einem Treffer vergleichen.
        // Ein `windows().position()` ueber 300 MiB waere quadratisch in der
        // Nadellaenge und liefe minutenlang.
        let mut index = 0;
        while let Some(hit) = buffer[index..len]
            .iter()
            .position(|byte| *byte == needle[0])
        {
            let start = index + hit;
            if start + needle.len() <= len && &buffer[start..start + needle.len()] == needle {
                return Some(position + start as u64);
            }
            index = start + 1;
        }
        position += CHUNK as u64;
    }
    None
}

#[test]
#[ignore = "braucht Linux, Root, ublk_drv und btrfs-progs"]
fn btrfs_finds_the_rot_and_ferrite_repairs_it() {
    if prerequisites().is_none() {
        return;
    }

    // --- 1. Das Array ----------------------------------------------------
    let mut loops = Vec::new();
    for (name, payload) in [
        ("d0", PAYLOAD),
        ("d1", PAYLOAD),
        ("p", PAYLOAD),
        ("q", PAYLOAD),
        ("log", LOG_PAYLOAD),
    ] {
        let Some(loop_device) = LoopDevice::create(name, payload) else {
            return;
        };
        loops.push(loop_device);
    }
    let path_of = |nth: usize| loops[nth].path().to_path_buf();

    let log = DeviceLog::initialize(
        MemberDevice::open(path_of(4)).expect("Log-Geraet"),
        &superblock(Role::Log, 0, LOG_PAYLOAD),
    )
    .expect("Log anlegen");

    let data: Vec<Member> = (0..SLOTS)
        .map(|slot| {
            member_for(
                MemberDevice::open(path_of(usize::from(slot))).expect("Data-Geraet"),
                &superblock(Role::Data, slot, PAYLOAD),
                Role::Data,
            )
            .expect("Data-Member")
        })
        .collect();
    let parity_p = member_for(
        MemberDevice::open(path_of(2)).expect("P-Geraet"),
        &superblock(Role::ParityP, 0, PAYLOAD),
        Role::ParityP,
    )
    .expect("ParityP");
    let parity_q = member_for(
        MemberDevice::open(path_of(3)).expect("Q-Geraet"),
        &superblock(Role::ParityQ, 0, PAYLOAD),
        Role::ParityQ,
    )
    .expect("ParityQ");

    let writer = Arc::new(Mutex::new(
        ArrayWriter::new(log, data, parity_p, Some(parity_q)).expect("ArrayWriter"),
    ));

    // --- 2. Das Blockgeraet fuer Slot 0 ----------------------------------
    let spec = UblkSpec {
        size: PAYLOAD,
        queue_depth: 16,
        max_io_buf_bytes: 256 * 1024,
        ..Default::default()
    };
    let device = UblkDevice::start(&spec, vec![ArraySlot::new(Arc::clone(&writer), 0)])
        .expect("ublk-Geraet starten");
    let block_path = device.block_path();
    assert!(wait_for(&block_path), "{block_path} ist nicht aufgetaucht");

    // --- 3. btrfs darauf --------------------------------------------------
    let mount_point = std::env::temp_dir().join("ferrite-selbstheilung-mount");
    std::fs::create_dir_all(&mount_point).expect("Einhaengepunkt anlegen");
    let mount_str = mount_point.to_string_lossy().to_string();
    let file_path = mount_point.join("nutzdaten.bin");
    let content = file_content();

    must_run("mkfs.btrfs", &["-q", "-f", &block_path]);
    must_run("mount", &[&block_path, &mount_str]);
    std::fs::write(&file_path, &content).expect("Datei schreiben");
    must_run("umount", &[&mount_str]);

    // --- 4. Rost ----------------------------------------------------------
    // Direkt auf der Platte, am Schreibpfad vorbei — die Paritaet weiss
    // nichts davon und traegt weiter den richtigen Inhalt.
    let member = MemberDevice::open(path_of(0)).expect("Data-Member oeffnen");
    let needle = &content[..256];
    let rot_at =
        find_in_payload(&member, needle, PAYLOAD).expect("Dateiinhalt auf der Platte gefunden");

    let mut bytes = vec![0u8; 4096];
    member
        .read_at(DEFAULT_PAYLOAD_OFFSET + rot_at, &mut bytes)
        .expect("lesen");
    for byte in bytes.iter_mut() {
        *byte ^= 0x5A;
    }
    member
        .write_at(DEFAULT_PAYLOAD_OFFSET + rot_at, &bytes)
        .expect("schreiben");
    member.flush().expect("flushen");
    drop(member);
    drop_caches();

    // --- 5. Der Scrub -----------------------------------------------------
    // Erst hier den Ringpuffer aufmachen: Was vorher darin stand, geht diesen
    // Test nichts an.
    let mut kmsg = KmsgReader::open().expect("/dev/kmsg oeffnen");

    must_run("mount", &[&block_path, &mount_str]);
    // `-B` wartet auf das Ende, `-R` waere nur Statistik. Der Rueckgabewert
    // ist ungleich null, wenn Fehler gefunden wurden — genau das erwarten wir
    // hier, also kein `must_run`.
    let scrub = Command::new("btrfs")
        .args(["scrub", "start", "-B", &mount_str])
        .output()
        .expect("btrfs scrub starten");
    eprintln!(
        "Scrub: {}{}",
        String::from_utf8_lossy(&scrub.stdout),
        String::from_utf8_lossy(&scrub.stderr)
    );
    must_run("umount", &[&mount_str]);

    // --- 6. Der Broker ----------------------------------------------------
    let mut slots = SlotMap::new();
    slots.insert(block_path.clone(), 0);
    let mut broker = RepairBroker::new(Arc::clone(&writer), slots, 4096);

    let messages = kmsg.drain(4096).expect("Kernel-Ringpuffer lesen");
    assert_eq!(
        kmsg.overrun(),
        0,
        "der Ringpuffer ist uebergelaufen, der Befund kann darin gewesen sein"
    );

    let mut reports: Vec<DamageReport> = messages
        .iter()
        .filter_map(|line| match broker.classify(line) {
            Classified::Damage(report) => Some(report),
            _ => None,
        })
        .collect();

    assert!(
        !reports.is_empty(),
        "kein Befund im Ringpuffer. Entweder hat der Scrub nichts gefunden, oder der \
         Kernel formuliert seine Meldungen anders als der Parser sie liest. \
         Gesehene btrfs-Zeilen:\n{}",
        messages
            .iter()
            .filter(|line| line.contains("BTRFS"))
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );

    let outcomes = broker.repair_all(&mut reports);
    for (report, outcome) in &outcomes {
        eprintln!(
            "Slot {} bei {} ueber {}: {:?}",
            report.slot_index, report.offset, report.len, outcome
        );
    }
    assert!(
        outcomes
            .iter()
            .any(|(_, outcome)| matches!(outcome, Ok(Repair::Written { .. }))),
        "der Broker hat nichts zurueckgeschrieben"
    );
    assert!(
        !broker.stats().needs_attention(),
        "eine Rekonstruktion war nicht eindeutig — dann war mehr als eine Quelle beschaedigt"
    );

    // --- 7. Die Probe -----------------------------------------------------
    drop_caches();
    must_run("mount", &[&block_path, &mount_str]);
    let read_back = std::fs::read(&file_path).expect("Datei nach der Reparatur lesen");

    // Ein zweiter Scrub muss sauber durchgehen. Er ist die eigentliche Probe:
    // Er prueft jede Pruefsumme des Dateisystems, nicht nur die eine Stelle,
    // die dieser Test kennt.
    let again = Command::new("btrfs")
        .args(["scrub", "start", "-B", &mount_str])
        .output()
        .expect("btrfs scrub starten");
    let again_output = format!(
        "{}{}",
        String::from_utf8_lossy(&again.stdout),
        String::from_utf8_lossy(&again.stderr)
    );
    must_run("umount", &[&mount_str]);

    device.stop().expect("ublk-Geraet stoppen");

    assert_eq!(read_back, content, "die Datei ist nicht wiederhergestellt");
    assert!(
        again.status.success(),
        "der zweite Scrub findet noch Fehler:\n{again_output}"
    );

    let _ = std::fs::remove_dir(&mount_point);
}
