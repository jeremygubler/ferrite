// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Das ublk-Target auf einem laufenden Kernel.
//!
//! **Braucht Linux, Root und `ublk_drv`.** Diese Tests sind `#[ignore]` und
//! laufen mit:
//!
//! ```text
//! sudo -E cargo test -p ferrite-engine --test ublk -- --ignored --test-threads=1
//! ```
//!
//! Ein Mock waere hier besonders wertlos: Was geprueft wird, ist genau das
//! Verhalten des Treibers. Fehlt eine Voraussetzung, sagen die Tests das und
//! laufen nicht weiter.
//!
//! `--test-threads=1` ist keine Bequemlichkeit: Jeder Test legt ein Geraet im
//! Kernel an, und zwei davon gleichzeitig konkurrieren um Geraetenummern und
//! Loop-Geraete.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::Command;

use ferrite_engine::ublk::{Passthrough, UblkControl, UblkDevice, UblkSpec, CONTROL_PATH};
use ferrite_engine::MemberDevice;

const PAYLOAD_OFFSET: u64 = 1 << 20;
const PAYLOAD_SIZE: u64 = 8 << 20;
const DEVICE_SIZE: u64 = PAYLOAD_OFFSET + PAYLOAD_SIZE + 65_536;

// --- Voraussetzungen ------------------------------------------------------

fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim() == "0")
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
    Some(())
}

/// Ein Loop-Geraet, das sich selbst wieder abbaut.
struct LoopDevice {
    device: PathBuf,
    backing: PathBuf,
}

impl LoopDevice {
    fn create(name: &str) -> Option<Self> {
        let backing = std::env::temp_dir().join(format!("ferrite-ublk-{name}.img"));
        let _ = std::fs::remove_file(&backing);
        let file = std::fs::File::create(&backing).ok()?;
        file.set_len(DEVICE_SIZE).ok()?;
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

fn spec() -> UblkSpec {
    UblkSpec {
        size: PAYLOAD_SIZE,
        queue_depth: 16,
        max_io_buf_bytes: 256 * 1024,
        ..Default::default()
    }
}

fn passthrough(loop_device: &LoopDevice) -> Passthrough {
    Passthrough::new(
        MemberDevice::open(loop_device.path()).expect("Member oeffnen"),
        PAYLOAD_OFFSET,
        PAYLOAD_SIZE,
    )
}

/// Wartet, bis der Kernel den Geraeteknoten angelegt hat.
///
/// `START_DEV` kehrt zurueck, sobald das Geraet lebt; den Knoten unter `/dev`
/// legt udev an, und das dauert einen Moment. Ohne dieses Warten scheitert der
/// Test an einer Wettlaufsituation und nicht an dem, was er pruefen soll.
fn wait_for(path: &str) -> bool {
    for _ in 0..100 {
        if Path::new(path).exists() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

fn pattern(marker: u8, len: usize) -> Vec<u8> {
    (0..len).map(|index| (index as u8) ^ marker).collect()
}

// --- Die Tests ------------------------------------------------------------

#[test]
#[ignore = "braucht Linux, Root und ublk_drv"]
fn the_kernel_reports_what_it_can() {
    // Ohne `UBLK_F_USER_COPY` muesste der Schreibpfad ueber
    // `NEED_GET_DATA` gehen — ein zweiter Zustandsautomat, den hier niemand
    // ausfuehren koennte. Deshalb wird das Feature verlangt und nicht
    // umgangen.
    if prerequisites().is_none() {
        return;
    }
    let control = UblkControl::open().expect("ublk-control oeffnen");
    eprintln!("Features: {:#x}", control.features());
    assert!(
        control.supports_user_copy(),
        "Kernel kann kein UBLK_F_USER_COPY — der Rest der Tests haette keine Grundlage"
    );
}

#[test]
#[ignore = "braucht Linux, Root und ublk_drv"]
fn a_device_appears_and_disappears_again() {
    if prerequisites().is_none() {
        return;
    }
    let Some(loop_device) = LoopDevice::create("lebenszyklus") else {
        return;
    };

    let device = UblkDevice::start(&spec(), vec![passthrough(&loop_device)]).expect("starten");
    let path = device.block_path();
    assert!(wait_for(&path), "{path} ist nicht aufgetaucht");

    // Der Kernel meldet die Groesse, die wir gesetzt haben.
    let size = MemberDevice::open_read_only(&path).expect("oeffnen").size();
    assert_eq!(size, PAYLOAD_SIZE);

    device.stop().expect("stoppen");
    assert!(!Path::new(&path).exists(), "{path} ist geblieben");
}

#[test]
#[ignore = "braucht Linux, Root und ublk_drv"]
fn what_the_guest_writes_lands_in_the_payload_region() {
    // Der eigentliche Durchstich: Ein Write auf `/dev/ublkbN` muss auf der
    // Platte bei `payload_offset` ankommen und nicht bei null. Landete er bei
    // null, ueberschriebe das erste Dateisystem den Superblock.
    if prerequisites().is_none() {
        return;
    }
    let Some(loop_device) = LoopDevice::create("durchstich") else {
        return;
    };

    let device = UblkDevice::start(&spec(), vec![passthrough(&loop_device)]).expect("starten");
    let path = device.block_path();
    assert!(wait_for(&path), "{path} ist nicht aufgetaucht");

    let data = pattern(0x5B, 16 * 1024);
    {
        let guest = MemberDevice::open(&path).expect("Gast oeffnen");
        guest.write_at(65_536, &data).expect("schreiben");
        guest.flush().expect("flushen");

        let mut read_back = vec![0u8; data.len()];
        guest.read_at(65_536, &mut read_back).expect("lesen");
        assert_eq!(read_back, data, "der Gast liest, was er geschrieben hat");
    }
    device.stop().expect("stoppen");

    // Und jetzt von der rohen Platte, an der Stelle, an der es liegen muss.
    let raw = MemberDevice::open_read_only(loop_device.path()).expect("Platte oeffnen");
    let mut from_disk = vec![0u8; data.len()];
    raw.read_at(PAYLOAD_OFFSET + 65_536, &mut from_disk)
        .expect("von der Platte lesen");
    assert_eq!(from_disk, data, "die Daten liegen ab payload_offset");

    // Der Bereich davor bleibt unberuehrt — dort steht spaeter der Superblock.
    let mut before = vec![0xFFu8; 4096];
    raw.read_at(65_536, &mut before).expect("davor lesen");
    assert!(
        before.iter().all(|&byte| byte == 0),
        "in den Superblock-Bereich wurde geschrieben"
    );
}

#[test]
#[ignore = "braucht Linux, Root und ublk_drv"]
fn data_written_through_the_guest_survives_the_device() {
    // Nach `stop` ist das ublk-Geraet weg. Was der Gast geschrieben hat, muss
    // auf der Platte bleiben — sonst haette der Umweg nichts bewirkt.
    if prerequisites().is_none() {
        return;
    }
    let Some(loop_device) = LoopDevice::create("dauerhaft") else {
        return;
    };
    let data = pattern(0xC7, 8192);

    {
        let device = UblkDevice::start(&spec(), vec![passthrough(&loop_device)]).expect("starten");
        let path = device.block_path();
        assert!(wait_for(&path), "{path} ist nicht aufgetaucht");

        let guest = MemberDevice::open(&path).expect("Gast oeffnen");
        guest.write_at(0, &data).expect("schreiben");
        guest.flush().expect("flushen");
        drop(guest);
        device.stop().expect("stoppen");
    }

    // Dasselbe Geraet noch einmal hochfahren und nachsehen.
    let device = UblkDevice::start(&spec(), vec![passthrough(&loop_device)]).expect("neu starten");
    let path = device.block_path();
    assert!(wait_for(&path), "{path} ist nicht aufgetaucht");

    let guest = MemberDevice::open_read_only(&path).expect("Gast oeffnen");
    let mut read_back = vec![0u8; data.len()];
    guest.read_at(0, &mut read_back).expect("lesen");
    assert_eq!(read_back, data);
    drop(guest);
    device.stop().expect("stoppen");
}

#[test]
#[ignore = "braucht Linux, Root und ublk_drv"]
fn a_read_beyond_the_payload_region_is_refused() {
    // Der Gast kennt nur die Groesse, die wir gemeldet haben. Fragt er
    // trotzdem dahinter, darf das Target nicht in den Backup-Superblock
    // greifen — der Kernel faengt das hier schon ab, und genau das soll er.
    if prerequisites().is_none() {
        return;
    }
    let Some(loop_device) = LoopDevice::create("jenseits") else {
        return;
    };

    let device = UblkDevice::start(&spec(), vec![passthrough(&loop_device)]).expect("starten");
    let path = device.block_path();
    assert!(wait_for(&path), "{path} ist nicht aufgetaucht");

    {
        let guest = MemberDevice::open_read_only(&path).expect("Gast oeffnen");
        assert_eq!(guest.size(), PAYLOAD_SIZE);

        let mut buffer = vec![0u8; 4096];
        assert!(
            guest.read_at(PAYLOAD_SIZE - 1, &mut buffer).is_err(),
            "ein Read ueber das Ende hinaus wurde angenommen"
        );
    }
    device.stop().expect("stoppen");
}

#[test]
#[ignore = "braucht Linux, Root und ublk_drv"]
fn a_target_per_queue_is_required() {
    if prerequisites().is_none() {
        return;
    }
    let Some(loop_device) = LoopDevice::create("anzahl") else {
        return;
    };
    let mut spec = spec();
    spec.nr_hw_queues = 2;

    // Nur ein Target fuer zwei Queues: abgelehnt, und zwar bevor im Kernel
    // etwas angelegt wird.
    assert!(UblkDevice::start(&spec, vec![passthrough(&loop_device)]).is_err());
}

// --- Der Zweck der Uebung -------------------------------------------------

/// Fuehrt ein Kommando aus. Schlaegt es fehl, faellt der Test — nicht der Test
/// still durch.
///
/// Ein Testschritt, der bei jedem Fehler kommentarlos ueberspringt, meldet am
/// Ende Erfolg fuer etwas, das nie gelaufen ist. Ob die Voraussetzungen da
/// sind, wird **einmal vorher** entschieden; danach muss jeder Schritt klappen.
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

fn have(program: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {program}")])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

#[test]
#[ignore = "braucht Linux, Root, ublk_drv und btrfs-progs"]
fn btrfs_lives_on_a_ferrite_block_device() {
    // Wofuer das Ganze da ist: Jeder Data-Member traegt ein eigenes btrfs, und
    // btrfs schreibt durch Ferrite hindurch. Dieser Test macht genau das —
    // Dateisystem anlegen, Datei schreiben, aushaengen, wieder einhaengen.
    //
    // Er prueft ausserdem, wo das alles landet: Wer eine Platte spaeter direkt
    // mountet, braucht den Offset `payload_offset`. Finge btrfs stattdessen
    // bei null an, laege es ueber dem Superblock.
    if prerequisites().is_none() {
        return;
    }
    for program in ["mkfs.btrfs", "mount", "umount", "losetup", "truncate"] {
        if !have(program) {
            eprintln!("uebersprungen: {program} fehlt");
            return;
        }
    }

    // btrfs will mindestens rund 100 MiB. Der Rest der Tests kommt mit
    // weniger aus, dieser nicht — also ein eigenes, groesseres Loop-Geraet.
    const BTRFS_PAYLOAD: u64 = 300 << 20;
    let backing = std::env::temp_dir().join("ferrite-btrfs-backing.img");
    let _ = std::fs::remove_file(&backing);
    let file = std::fs::File::create(&backing).expect("Hintergrunddatei anlegen");
    file.set_len(PAYLOAD_OFFSET + BTRFS_PAYLOAD + 65_536)
        .expect("Groesse setzen");
    drop(file);

    let output = Command::new("losetup")
        .args(["--show", "--find"])
        .arg(&backing)
        .output()
        .expect("losetup starten");
    assert!(
        output.status.success(),
        "losetup: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let loop_path = String::from_utf8_lossy(&output.stdout).trim().to_string();

    let spec = UblkSpec {
        size: BTRFS_PAYLOAD,
        queue_depth: 16,
        max_io_buf_bytes: 256 * 1024,
        ..Default::default()
    };
    let target = Passthrough::new(
        MemberDevice::open(&loop_path).expect("Member oeffnen"),
        PAYLOAD_OFFSET,
        BTRFS_PAYLOAD,
    );

    let device = UblkDevice::start(&spec, vec![target]).expect("starten");
    let path = device.block_path();
    assert!(wait_for(&path), "{path} ist nicht aufgetaucht");

    let mount_point = std::env::temp_dir().join("ferrite-btrfs-mount");
    std::fs::create_dir_all(&mount_point).expect("Einhaengepunkt anlegen");
    let mount_str = mount_point.to_string_lossy().to_string();
    let file_path = mount_point.join("beweis.txt");
    let content = "btrfs laeuft durch Ferrite hindurch";

    must_run("mkfs.btrfs", &["-q", "-f", &path]);
    must_run("mount", &[&path, &mount_str]);
    std::fs::write(&file_path, content).expect("Datei schreiben");
    must_run("umount", &[&mount_str]);

    // Neu einhaengen — was jetzt noch da ist, kam wirklich von der Platte und
    // nicht aus einem Cache, den derselbe Mount noch hielt.
    must_run("mount", &[&path, &mount_str]);
    let read_back = std::fs::read_to_string(&file_path).expect("Datei lesen");
    must_run("umount", &[&mount_str]);

    device.stop().expect("stoppen");
    assert_eq!(read_back, content);

    // btrfs setzt seinen Superblock 64 KiB nach dem Anfang seines Geraets.
    // Sein Geraet beginnt bei `payload_offset` — dort muss die Signatur
    // liegen, und bei null darf keine sein.
    let raw = MemberDevice::open_read_only(&loop_path).expect("Platte oeffnen");
    let mut magic = [0u8; 8];
    raw.read_at(PAYLOAD_OFFSET + 65_536 + 64, &mut magic)
        .expect("Signatur lesen");
    assert_eq!(
        &magic, b"_BHRfS_M",
        "keine btrfs-Signatur bei payload_offset + 64 KiB"
    );

    let mut at_start = [0u8; 8];
    raw.read_at(65_536 + 64, &mut at_start)
        .expect("am Anfang lesen");
    assert_ne!(
        &at_start, b"_BHRfS_M",
        "btrfs hat in den Superblock-Bereich geschrieben"
    );
    drop(raw);

    let _ = Command::new("losetup").args(["-d", &loop_path]).status();
    let _ = std::fs::remove_file(&backing);
    let _ = std::fs::remove_dir(&mount_point);
}

// --- Der Schreibpfad hinter dem Geraet ------------------------------------

#[test]
#[ignore = "braucht Linux, Root und ublk_drv"]
fn writes_through_the_guest_keep_the_parity_correct() {
    // Der volle Weg: Gast schreibt auf `/dev/ublkbN`, der Schreibpfad legt
    // einen Log-Record an, schreibt den Data-Member, zieht P und Q nach und
    // setzt einen Checkpoint — und danach muss die Paritaet zum Inhalt der
    // Data-Members passen.
    use std::sync::{Arc, Mutex};

    use ferrite_engine::ublk::ArraySlot;
    use ferrite_engine::{member_for, ArrayWriter, DeviceLog, Member};
    use ferrite_format::superblock::{Role, Superblock};
    use ferrite_format::Uuid;

    if prerequisites().is_none() {
        return;
    }

    const SLOT_PAYLOAD: u64 = 4 << 20;
    const LOG_PAYLOAD: u64 = 1 << 20;
    const SLOTS: u16 = 3;

    fn sb(role: Role, slot_index: u16, payload: u64) -> Superblock {
        let mut superblock = Superblock::new(
            Uuid::from_random_bytes([0x44; 16]),
            Uuid::from_random_bytes([slot_index as u8 + 0x50; 16]),
            role,
            SLOTS as u32,
            payload,
        );
        superblock.slot_index = slot_index;
        superblock
    }

    // Ein Loop-Geraet je Member: drei Data, ein ParityP, ein ParityQ, ein Log.
    let mut loops = Vec::new();
    for name in ["d0", "d1", "d2", "p", "q", "log"] {
        let Some(loop_device) = LoopDevice::create(name) else {
            return;
        };
        loops.push(loop_device);
    }
    let path_of = |nth: usize| loops[nth].path().to_path_buf();

    let log = DeviceLog::initialize(
        MemberDevice::open(path_of(5)).expect("Log-Geraet"),
        &sb(Role::Log, 0, LOG_PAYLOAD),
    )
    .expect("Log anlegen");

    let data: Vec<Member> = (0..SLOTS)
        .map(|slot| {
            member_for(
                MemberDevice::open(path_of(usize::from(slot))).expect("Data-Geraet"),
                &sb(Role::Data, slot, SLOT_PAYLOAD),
                Role::Data,
            )
            .expect("Data-Member")
        })
        .collect();
    let parity_p = member_for(
        MemberDevice::open(path_of(3)).expect("P-Geraet"),
        &sb(Role::ParityP, 0, SLOT_PAYLOAD),
        Role::ParityP,
    )
    .expect("ParityP");
    let parity_q = member_for(
        MemberDevice::open(path_of(4)).expect("Q-Geraet"),
        &sb(Role::ParityQ, 0, SLOT_PAYLOAD),
        Role::ParityQ,
    )
    .expect("ParityQ");

    let writer = Arc::new(Mutex::new(
        ArrayWriter::new(log, data, parity_p, Some(parity_q)).expect("ArrayWriter"),
    ));

    // Je Data-Slot ein ublk-Geraet, alle auf denselben Schreibpfad.
    let spec = UblkSpec {
        size: SLOT_PAYLOAD,
        queue_depth: 16,
        max_io_buf_bytes: 256 * 1024,
        ..Default::default()
    };
    let mut devices = Vec::new();
    for slot in 0..SLOTS {
        let device = UblkDevice::start(&spec, vec![ArraySlot::new(writer.clone(), slot)])
            .expect("ublk-Geraet starten");
        assert!(wait_for(&device.block_path()), "Geraet nicht aufgetaucht");
        devices.push(device);
    }

    // Der Gast schreibt auf jedes Geraet, an verschiedenen Stellen.
    for (slot, device) in devices.iter().enumerate() {
        let guest = MemberDevice::open(device.block_path()).expect("Gast oeffnen");
        for round in 0..4u8 {
            let data = pattern(slot as u8 * 16 + round, 8192);
            let offset = u64::from(round) * 16384 + slot as u64 * 4096;
            guest.write_at(offset, &data).expect("schreiben");
        }
        guest.flush().expect("flushen");
    }

    for device in devices {
        device.stop().expect("stoppen");
    }

    // Und jetzt die Frage, um die es geht.
    let writer = writer.lock().expect("Sperre");
    assert!(
        writer.verify_parity(0, 128 * 1024).unwrap(),
        "die Paritaet passt nicht zu dem, was der Gast geschrieben hat"
    );

    // Der Gast liest zurueck, was er geschrieben hat — ueber den Schreibpfad,
    // nicht ueber die rohe Platte.
    let mut read_back = vec![0u8; 8192];
    writer.read(1, 4096, &mut read_back).unwrap();
    assert_eq!(read_back, pattern(16, 8192));
}

#[test]
#[ignore = "braucht Linux, Root und ublk_drv"]
fn a_failed_disk_is_rebuilt_while_the_guest_keeps_reading() {
    // Wofuer die ganze Paritaet da ist: Eine Platte faellt aus, der Gast liest
    // weiter — rekonstruiert, ohne es zu merken —, der Rebuild holt sie zurueck,
    // und danach steht wieder alles auf der Platte.
    use std::sync::{Arc, Mutex};

    use ferrite_engine::ublk::ArraySlot;
    use ferrite_engine::{member_for, ArrayWriter, DeviceLog, DiskRebuild, Member};
    use ferrite_format::superblock::{MemberState, Role, Superblock};
    use ferrite_format::Uuid;

    if prerequisites().is_none() {
        return;
    }

    const SLOT_PAYLOAD: u64 = 4 << 20;
    const LOG_PAYLOAD: u64 = 1 << 20;
    const SLOTS: u16 = 3;
    const BLOCK: u64 = 64 * 1024;

    fn sb(role: Role, slot_index: u16, payload: u64) -> Superblock {
        let mut superblock = Superblock::new(
            Uuid::from_random_bytes([0x66; 16]),
            Uuid::from_random_bytes([slot_index as u8 + 0x70; 16]),
            role,
            SLOTS as u32,
            payload,
        );
        superblock.slot_index = slot_index;
        superblock
    }

    let mut loops = Vec::new();
    for name in ["rd0", "rd1", "rd2", "rp", "rq", "rlog"] {
        let Some(loop_device) = LoopDevice::create(name) else {
            return;
        };
        loops.push(loop_device);
    }
    let path_of = |nth: usize| loops[nth].path().to_path_buf();

    let log = DeviceLog::initialize(
        MemberDevice::open(path_of(5)).expect("Log-Geraet"),
        &sb(Role::Log, 0, LOG_PAYLOAD),
    )
    .expect("Log anlegen");

    let data: Vec<Member> = (0..SLOTS)
        .map(|slot| {
            member_for(
                MemberDevice::open(path_of(usize::from(slot))).expect("Data-Geraet"),
                &sb(Role::Data, slot, SLOT_PAYLOAD),
                Role::Data,
            )
            .expect("Data-Member")
        })
        .collect();
    let parity_p = member_for(
        MemberDevice::open(path_of(3)).expect("P-Geraet"),
        &sb(Role::ParityP, 0, SLOT_PAYLOAD),
        Role::ParityP,
    )
    .expect("ParityP");
    let parity_q = member_for(
        MemberDevice::open(path_of(4)).expect("Q-Geraet"),
        &sb(Role::ParityQ, 0, SLOT_PAYLOAD),
        Role::ParityQ,
    )
    .expect("ParityQ");

    let writer = Arc::new(Mutex::new(
        ArrayWriter::new(log, data, parity_p, Some(parity_q)).expect("ArrayWriter"),
    ));

    let spec = UblkSpec {
        size: SLOT_PAYLOAD,
        queue_depth: 16,
        max_io_buf_bytes: 256 * 1024,
        ..Default::default()
    };

    // Der Gast schreibt ueber das Blockgeraet auf Slot 1.
    let mut written = Vec::new();
    {
        let device = UblkDevice::start(&spec, vec![ArraySlot::new(writer.clone(), 1)])
            .expect("Geraet starten");
        assert!(wait_for(&device.block_path()), "Geraet nicht aufgetaucht");

        let guest = MemberDevice::open(device.block_path()).expect("Gast oeffnen");
        for block in 0..8u64 {
            let data = pattern(block as u8 + 0x80, 16384);
            guest.write_at(block * BLOCK, &data).expect("schreiben");
            written.push(data);
        }
        guest.flush().expect("flushen");
        // Erst schliessen, dann stoppen: `DEL_DEV` wartet darauf, dass niemand
        // mehr `/dev/ublkbN` offen haelt.
        drop(guest);
        device.stop().expect("stoppen");
    }

    // Die Platte faellt aus: als `Stale` melden und ihren Inhalt zerstoeren.
    {
        let mut guard = writer.lock().expect("Sperre");
        guard
            .mark_member(1, MemberState::Stale, 0)
            .expect("Ausfall melden");
        let member = guard.member(1).expect("Member");
        member
            .device()
            .write_at(
                member.superblock().payload_offset,
                &vec![0x5Au8; SLOT_PAYLOAD as usize],
            )
            .expect("Inhalt zerstoeren");
        member.device().flush().expect("flushen");
    }

    // Der Gast liest weiter — und bekommt seine Daten, rekonstruiert aus der
    // Paritaet. Auf der Platte darunter steht in diesem Moment Muell.
    {
        let device = UblkDevice::start(&spec, vec![ArraySlot::new(writer.clone(), 1)])
            .expect("Geraet im degradierten Betrieb starten");
        assert!(wait_for(&device.block_path()), "Geraet nicht aufgetaucht");

        let guest = MemberDevice::open_read_only(device.block_path()).expect("Gast oeffnen");
        for (block, expected) in written.iter().enumerate() {
            let mut read_back = vec![0u8; expected.len()];
            guest
                .read_at(block as u64 * BLOCK, &mut read_back)
                .expect("lesen");
            assert_eq!(
                &read_back, expected,
                "Block {block} wurde im degradierten Betrieb nicht rekonstruiert"
            );
        }
        drop(guest);
        device.stop().expect("stoppen");
    }

    // Rebuild.
    {
        let mut guard = writer.lock().expect("Sperre");
        let mut rebuild = DiskRebuild::resume(&guard, 1).expect("Rebuild aufsetzen");
        assert_eq!(rebuild.remaining_blocks(), SLOT_PAYLOAD / BLOCK);
        rebuild.run(&mut guard, 8).expect("Rebuild durchfuehren");
        assert_eq!(
            guard.member(1).unwrap().superblock().member_state,
            MemberState::Clean
        );
        assert!(guard
            .verify_parity(0, (8 * BLOCK) as usize)
            .expect("Paritaet pruefen"));
    }

    // Und jetzt liegt der Inhalt wirklich auf der Platte — roh gelesen, ohne
    // den Schreibpfad und ohne Rekonstruktion.
    let payload_offset = writer
        .lock()
        .expect("Sperre")
        .member(1)
        .unwrap()
        .superblock()
        .payload_offset;
    let raw = MemberDevice::open_read_only(path_of(1)).expect("Platte oeffnen");
    for (block, expected) in written.iter().enumerate() {
        let mut from_disk = vec![0u8; expected.len()];
        raw.read_at(payload_offset + block as u64 * BLOCK, &mut from_disk)
            .expect("roh lesen");
        assert_eq!(
            &from_disk, expected,
            "Block {block} steht nach dem Rebuild nicht auf der Platte"
        );
    }
}
