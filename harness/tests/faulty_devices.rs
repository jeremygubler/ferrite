// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Die zweite Haelfte von Meilenstein 3: Geraete, die sich schlecht benehmen.
//!
//! Der Absturztest in `crash.rs` bricht **zwischen** zwei I/O-Operationen ab.
//! Was er nicht abdeckt, deckt der Kernel selbst ab, wenn man ihn darum bittet:
//!
//! - **`dm-dust`** liefert `EIO` fuer bestimmte Bloecke. Das ist die kaputte
//!   Platte, die noch da ist und trotzdem nichts mehr hergibt.
//! - **`dm-flakey` mit `drop_writes`** nimmt Writes an und wirft sie weg. Das
//!   ist das Geraet, das seinen Flush belogen hat — genau der Fall aus
//!   Abschnitt 5.3, diesmal nicht als Vermutung, sondern erzwungen.
//! - **`dm-flakey` mit `corrupt_bio_byte`** aendert beim Lesen ein Byte. Das
//!   ist Bit-Rot.
//!
//! **Braucht Linux, Root und die Module `dm_dust`/`dm_flakey`.** Deshalb
//! `#[ignore]`:
//!
//! ```text
//! sudo modprobe dm_dust dm_flakey
//! sudo -E cargo test -p ferrite-harness --test faulty_devices -- --ignored --test-threads=1
//! ```
//!
//! Fehlt eine Voraussetzung, sagen die Tests das und laufen nicht auf einer
//! Attrappe weiter.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::Command;

use ferrite_format::superblock::MemberState;
use ferrite_harness::{device_size, LOG_PAYLOAD, PAYLOAD, SLOTS};

/// Blockgroesse, mit der `dm-dust` seine Bloecke zaehlt.
const DUST_BLOCK: u64 = 4096;

// --- Voraussetzungen ------------------------------------------------------

fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim() == "0")
        .unwrap_or(false)
}

/// Ist dieses dm-Target im laufenden Kernel vorhanden?
fn has_target(name: &str) -> bool {
    Command::new("dmsetup")
        .arg("targets")
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).contains(name))
        .unwrap_or(false)
}

fn prerequisites(target: &str) -> Option<()> {
    if !is_root() {
        eprintln!("uebersprungen: braucht Root");
        return None;
    }
    // Die Module sind nach einem Neustart der Maschine nicht automatisch da.
    let _ = Command::new("modprobe").arg("dm_dust").status();
    let _ = Command::new("modprobe").arg("dm_flakey").status();
    if !has_target(target) {
        eprintln!("uebersprungen: dm-Target `{target}` fehlt im Kernel");
        return None;
    }
    Some(())
}

// --- Geraete --------------------------------------------------------------

/// Ein Loop-Geraet ueber einer Datei, das sich selbst abbaut.
struct LoopDevice {
    device: PathBuf,
    backing: PathBuf,
}

impl LoopDevice {
    fn create(name: &str, size: u64) -> Option<Self> {
        let backing = std::env::temp_dir().join(format!("ferrite-faulty-{name}.img"));
        let _ = std::fs::remove_file(&backing);
        let file = std::fs::File::create(&backing).ok()?;
        file.set_len(size).ok()?;
        drop(file);

        let output = Command::new("losetup")
            .args(["--show", "--find"])
            .arg(&backing)
            .output()
            .ok()?;
        if !output.status.success() {
            eprintln!(
                "uebersprungen: losetup: {}",
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

/// Ein device-mapper-Geraet, das sich selbst abbaut.
struct DmDevice {
    name: String,
}

impl DmDevice {
    /// Legt ein dm-Geraet mit der angegebenen Tabelle an.
    ///
    /// `table` ist alles hinter `0 <sektoren>` — die Sektorenzahl kommt vom
    /// Traegergeraet und wird hier ergaenzt.
    fn create(name: &str, backing: &Path, table: &str) -> Option<Self> {
        let name = format!("ferrite-{name}");
        // Reste eines abgebrochenen Laufs wegraeumen. Die Fehlermeldung, wenn
        // es keine gibt, interessiert nicht — deshalb `output` statt `status`.
        let _ = Command::new("dmsetup").args(["remove", &name]).output();

        let sectors = Command::new("blockdev")
            .arg("--getsz")
            .arg(backing)
            .output()
            .ok()?;
        let sectors = String::from_utf8_lossy(&sectors.stdout).trim().to_string();

        let full = format!("0 {sectors} {table}");
        let output = Command::new("dmsetup")
            .args(["create", &name, "--table", &full])
            .output()
            .ok()?;
        if !output.status.success() {
            eprintln!(
                "uebersprungen: dmsetup create `{full}`: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            return None;
        }
        Some(DmDevice { name })
    }

    fn path(&self) -> PathBuf {
        PathBuf::from(format!("/dev/mapper/{}", self.name))
    }

    /// Schickt eine Nachricht an das Target, etwa `addbadblock 42`.
    fn message(&self, message: &str) -> bool {
        let args: Vec<&str> = message.split_whitespace().collect();
        let mut command = Command::new("dmsetup");
        command.arg("message").arg(&self.name).arg("0").args(&args);
        command
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    /// Tauscht die Tabelle aus, ohne das Geraet abzubauen.
    ///
    /// So laesst sich ein Geraet mitten im Betrieb boesartig machen und danach
    /// wieder gutmuetig — die Pfade darueber bleiben gueltig.
    fn reload(&self, backing: &Path, table: &str) -> bool {
        let Ok(sectors) = Command::new("blockdev")
            .arg("--getsz")
            .arg(backing)
            .output()
        else {
            return false;
        };
        let sectors = String::from_utf8_lossy(&sectors.stdout).trim().to_string();
        let full = format!("0 {sectors} {table}");

        let ok = Command::new("dmsetup")
            .args(["suspend", &self.name])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
            && Command::new("dmsetup")
                .args(["reload", &self.name, "--table", &full])
                .status()
                .map(|status| status.success())
                .unwrap_or(false);
        let resumed = Command::new("dmsetup")
            .args(["resume", &self.name])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        ok && resumed
    }
}

impl Drop for DmDevice {
    fn drop(&mut self) {
        let _ = Command::new("dmsetup")
            .args(["remove", &self.name])
            .output();
    }
}

/// Sechs Loop-Geraete in der Reihenfolge, die `ferrite_harness` erwartet.
fn loop_set(prefix: &str) -> Option<Vec<LoopDevice>> {
    let mut devices = Vec::new();
    for index in 0..usize::from(SLOTS) + 3 {
        let payload = if index == usize::from(SLOTS) + 2 {
            LOG_PAYLOAD
        } else {
            PAYLOAD
        };
        devices.push(LoopDevice::create(
            &format!("{prefix}{index}"),
            device_size(payload),
        )?);
    }
    Some(devices)
}

/// Leert den Seitencache des Kernels.
///
/// Ohne das prueft ein Lesefehler-Test womoeglich nur den Cache: Ein Read, der
/// von dort bedient wird, erzeugt gar kein Bio, und `dm-dust` bekommt ihn nie
/// zu sehen. Der Test waere dann gruen, ohne etwas gezeigt zu haben.
fn drop_caches() {
    let _ = std::process::Command::new("sync").status();
    let _ = std::fs::write("/proc/sys/vm/drop_caches", b"3");
}

fn pattern(marker: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| (index as u8).wrapping_mul(53) ^ marker)
        .collect()
}

// --- Lesefehler -----------------------------------------------------------

#[test]
#[ignore = "braucht Linux, Root und dm_dust"]
fn a_read_error_on_a_data_member_is_answered_from_the_parity() {
    // Der Fall, fuer den die Redundanz da ist: Die Platte ist noch da, aber
    // ein Block gibt nichts mehr her. Ferrite muss ihn aus der Paritaet
    // beantworten, statt den Fehler an den Gast durchzureichen.
    if prerequisites("dust").is_none() {
        return;
    }
    let Some(loops) = loop_set("dust") else {
        return;
    };

    // Data-Slot 0 liegt hinter dm-dust, alles andere direkt auf dem Loop.
    let table = format!("dust {} 0 {DUST_BLOCK}", loops[0].path().display());
    let Some(dust) = DmDevice::create("dust0", loops[0].path(), &table) else {
        return;
    };

    let mut paths: Vec<PathBuf> = loops
        .iter()
        .map(|device| device.path().to_path_buf())
        .collect();
    paths[0] = dust.path();

    ferrite_harness::create_on(&paths).expect("Array anlegen");

    let expected = pattern(0x6B, 4096);
    {
        let (mut writer, _) = ferrite_harness::open_on(&paths).expect("oeffnen");
        writer.write(0, 0, &expected).expect("schreiben");
        // Und etwas auf die anderen Slots, damit die Paritaet nicht trivial ist.
        writer.write(1, 0, &pattern(0x1C, 4096)).expect("schreiben");
        writer.write(2, 0, &pattern(0x2D, 4096)).expect("schreiben");
    }

    // Der Payload beginnt bei 1 MiB. Der erste Payload-Block ist damit
    // dm-dust-Block 256.
    let payload_block = 1_048_576 / DUST_BLOCK;
    assert!(
        dust.message(&format!("addbadblock {payload_block}")),
        "addbadblock fehlgeschlagen"
    );
    assert!(dust.message("enable"), "dust enable fehlgeschlagen");
    drop_caches();

    // Der Kernel liefert jetzt EIO fuer diesen Block. Nachweis, dass die
    // Vorbereitung wirkt — sonst prueft der Test darunter nichts.
    let direct = std::fs::File::open(dust.path()).expect("dust oeffnen");
    let mut probe = vec![0u8; 4096];
    let failed = {
        use std::os::unix::fs::FileExt;
        direct.read_exact_at(&mut probe, 1_048_576).is_err()
    };
    assert!(
        failed,
        "dm-dust liefert keinen Lesefehler — der Test haette nichts geprueft"
    );

    let (writer, _) = ferrite_harness::open_on(&paths).expect("nach dem Fehler oeffnen");
    let mut read_back = vec![0u8; expected.len()];
    let outcome = writer.read(0, 0, &mut read_back);

    assert!(
        outcome.is_ok(),
        "der Lesefehler wurde durchgereicht statt rekonstruiert: {:?}",
        outcome.unwrap_err()
    );
    assert_eq!(
        read_back, expected,
        "aus der Paritaet kam nicht der urspruengliche Inhalt"
    );
}

// --- Ein Geraet, das seinen Flush belogen hat -----------------------------

#[test]
#[ignore = "braucht Linux, Root und dm_flakey"]
fn a_device_that_swallows_writes_leaves_the_parity_stale_and_the_scrub_finds_it() {
    // Abschnitt 5.3, nicht als Vermutung, sondern erzwungen: Das Geraet nimmt
    // Writes an, bestaetigt sie und wirft sie weg.
    //
    // Ferrite kann das **nicht verhindern** — wer belogen wird, merkt es im
    // Moment der Luege nicht. Was es kann, ist die Abweichung finden, sobald
    // jemand hinsieht. Genau das prueft dieser Test, und er tut ausdruecklich
    // nicht so, als waere der Fall behandelbar.
    if prerequisites("flakey").is_none() {
        return;
    }
    let Some(loops) = loop_set("drop") else {
        return;
    };

    // Der ParityP-Member liegt hinter flakey. Anfangs gutmuetig: `0 0 1` heisst
    // up=0, down=1, aber ohne Feature gibt es nichts zu tun.
    let p_index = usize::from(SLOTS);
    let honest = format!("flakey {} 0 1 0", loops[p_index].path().display());
    let lying = format!(
        "flakey {} 0 0 1 1 drop_writes",
        loops[p_index].path().display()
    );

    let Some(flakey) = DmDevice::create("dropp", loops[p_index].path(), &honest) else {
        return;
    };

    let mut paths: Vec<PathBuf> = loops
        .iter()
        .map(|device| device.path().to_path_buf())
        .collect();
    paths[p_index] = flakey.path();

    ferrite_harness::create_on(&paths).expect("Array anlegen");
    {
        let (mut writer, _) = ferrite_harness::open_on(&paths).expect("oeffnen");
        writer.write(0, 0, &pattern(0x31, 4096)).expect("schreiben");
        assert!(
            writer.verify_parity(0, 4096).expect("pruefen"),
            "die Paritaet stimmt schon vor der Luege nicht"
        );
    }

    // Ab jetzt verschluckt der Parity-Member alles.
    assert!(
        flakey.reload(loops[p_index].path(), &lying),
        "Tabelle konnte nicht getauscht werden"
    );

    {
        let (mut writer, _) = ferrite_harness::open_on(&paths).expect("oeffnen");
        // Der Schreibpfad meldet Erfolg — er hat keine Moeglichkeit, es besser
        // zu wissen.
        writer
            .write(1, 0, &pattern(0x42, 4096))
            .expect("der Schreibpfad merkt die Luege nicht, und das ist erwartet");
    }

    // Wieder ehrlich, damit sich lesen laesst, was wirklich dasteht.
    assert!(flakey.reload(loops[p_index].path(), &honest));

    let (writer, _) = ferrite_harness::open_on(&paths).expect("oeffnen");
    assert!(
        !writer.verify_parity(0, 4096).expect("pruefen"),
        "der Scrub hat die veraltete Paritaet nicht bemerkt — \
         genau dieser Fall waere sonst ein stiller Datenverlust"
    );

    // Und er laesst sich beheben, ohne dass Daten verlorengehen: Die
    // Data-Members sind unversehrt, also wird die Paritaet neu gebildet.
    let mut repaired = writer;
    repaired
        .rebuild_parity(0, 4096)
        .expect("Paritaet neu bilden");
    assert!(
        repaired.verify_parity(0, 4096).expect("pruefen"),
        "nach dem Neubilden stimmt die Paritaet immer noch nicht"
    );
}

#[test]
#[ignore = "braucht Linux, Root und dm_flakey"]
fn a_corrupted_superblock_is_caught_by_its_own_checksum() {
    // `corrupt_bio_byte` verfaelscht **jedes** gelesene Bio, also auch den
    // Superblock. Das war beim Bauen dieses Tests eine Ueberraschung und ist
    // ein Ergebnis fuer sich: Ein verfaelschter Superblock wird nicht
    // stillschweigend hingenommen, sondern von seiner Pruefsumme gefangen.
    //
    // Beide Kopien liegen auf demselben Geraet und werden gleich verfaelscht —
    // deshalb hilft hier auch der Backup nicht, und das Oeffnen scheitert. Das
    // ist die richtige Antwort: Wer den Superblock nicht lesen kann, weiss
    // nicht, wohin die Nutzdaten gehoeren.
    if prerequisites("flakey").is_none() {
        return;
    }
    let Some(loops) = loop_set("sbrot") else {
        return;
    };

    let honest = format!("flakey {} 0 1 0", loops[1].path().display());
    let rotten = format!(
        "flakey {} 0 0 1 5 corrupt_bio_byte 40 r 153 0",
        loops[1].path().display()
    );

    let Some(flakey) = DmDevice::create("sbrot1", loops[1].path(), &honest) else {
        return;
    };
    let mut paths: Vec<PathBuf> = loops
        .iter()
        .map(|device| device.path().to_path_buf())
        .collect();
    paths[1] = flakey.path();

    ferrite_harness::create_on(&paths).expect("Array anlegen");
    assert!(flakey.reload(loops[1].path(), &rotten));

    let outcome = ferrite_harness::open_on(&paths);
    let Err(error) = outcome else {
        panic!("ein verfaelschter Superblock wurde stillschweigend angenommen");
    };
    let message = format!("{error}");
    assert!(
        message.contains("Pruefsumme")
            || message.contains("checksum")
            || message.contains("Checksum"),
        "der Fehler kam nicht von der Pruefsumme: {message}"
    );
}

// --- Bit-Rot --------------------------------------------------------------

#[test]
#[ignore = "braucht Linux und Root"]
fn silent_corruption_is_found_by_the_scrub_and_repaired_from_the_parity() {
    // Die Kopplung, die das README verspricht: Pruefsummen ohne Redundanz
    // koennen nur melden, Paritaet ohne Pruefsummen merkt nichts — erst
    // zusammen reparieren sie.
    //
    // Bit-Rot heisst: Das Bit auf der Platte kippt, ohne dass jemand geschrieben
    // haette. Genau so wird es hier erzeugt — durch ein veraendertes Byte auf
    // dem Geraet, nicht durch einen luegenden Lesepfad. `corrupt_bio_byte`
    // waere das andere, und es trifft auch den Superblock; das steht im Test
    // darueber.
    //
    // Ferrite haelt nach Regel 7 keine eigenen Pruefsummen ueber Nutzdaten. Im
    // Betrieb meldet btrfs den korrupten Block; hier uebernimmt der Scrub die
    // Rolle des Melders, und danach kommt die Redundanz zum Zug.
    if !is_root() {
        eprintln!("uebersprungen: braucht Root");
        return;
    }
    let Some(loops) = loop_set("rot") else {
        return;
    };
    let paths: Vec<PathBuf> = loops
        .iter()
        .map(|device| device.path().to_path_buf())
        .collect();

    ferrite_harness::create_on(&paths).expect("Array anlegen");
    let expected = pattern(0x77, 4096);
    {
        let (mut writer, _) = ferrite_harness::open_on(&paths).expect("oeffnen");
        writer.write(0, 0, &pattern(0x11, 4096)).expect("schreiben");
        writer.write(1, 0, &expected).expect("schreiben");
        writer.write(2, 0, &pattern(0x33, 4096)).expect("schreiben");
        assert!(writer.verify_parity(0, 4096).expect("pruefen"));
    }

    // Ein Bit kippt im Payload von Slot 1 — an Ferrite vorbei.
    {
        let device = ferrite_engine::MemberDevice::open(&paths[1]).expect("Geraet oeffnen");
        let mut byte = [0u8; 1];
        device.read_at(1_048_576 + 40, &mut byte).expect("lesen");
        byte[0] ^= 0x40;
        device.write_at(1_048_576 + 40, &byte).expect("kippen");
        device.flush().expect("flushen");
    }

    let mut writer = ferrite_harness::open_on(&paths).expect("oeffnen").0;

    // Der gewoehnliche Read merkt nichts — er hat nichts, woran er es merken
    // koennte. Das ist Regel 7 und kein Versaeumnis.
    let mut rotten_read = vec![0u8; expected.len()];
    writer.read(1, 0, &mut rotten_read).expect("lesen");
    assert_ne!(
        rotten_read, expected,
        "die Korruption ist gar nicht angekommen — der Test haette nichts geprueft"
    );

    // Der Scrub dagegen findet sie: Die Paritaet passt nicht mehr zu dem, was
    // die Data-Members hergeben.
    assert!(
        !writer.verify_parity(0, 4096).expect("pruefen"),
        "der Scrub hat die stille Korruption nicht bemerkt"
    );

    // Und jetzt die Reparatur. Wer meldet, dass dieser Member an dieser Stelle
    // nichts Brauchbares liefert, bekommt seinen Inhalt aus der Paritaet
    // zurueck — genau das tut spaeter der Repair-Broker, wenn btrfs meckert.
    writer
        .mark_member(1, MemberState::Stale, 0)
        .expect("Member melden");
    let mut repaired = vec![0u8; expected.len()];
    writer
        .read(1, 0, &mut repaired)
        .expect("rekonstruiert lesen");
    assert_eq!(
        repaired, expected,
        "aus der Paritaet kam nicht der urspruengliche Inhalt"
    );

    // Der Rebuild schreibt ihn zurueck, und danach steht er wieder roh auf der
    // Platte — die Selbstheilung ist damit vollstaendig.
    let mut rebuild = ferrite_engine::DiskRebuild::resume(&writer, 1).expect("Rebuild");
    rebuild.run(&mut writer, 4).expect("Rebuild durchfuehren");

    let device = ferrite_engine::MemberDevice::open_read_only(&paths[1]).expect("Geraet");
    let mut from_disk = vec![0u8; expected.len()];
    device
        .read_at(1_048_576, &mut from_disk)
        .expect("roh lesen");
    assert_eq!(
        from_disk, expected,
        "nach dem Rebuild steht der Inhalt nicht wieder auf der Platte"
    );
}
// --- Lesefehler auf dem Log ----------------------------------------------

#[test]
#[ignore = "braucht Linux, Root und dm_dust"]
fn a_read_error_in_the_log_region_does_not_take_the_array_down() {
    // Die Log-Region ist von keiner Paritaet gedeckt (Abschnitt 4.2). Ein
    // Lesefehler dort laesst sich also nicht rekonstruieren — aber er darf das
    // Array nicht mitnehmen. Die Data-Members sind unversehrt, und genau das
    // ist die Eigenschaft, die dieses Projekt definiert.
    if prerequisites("dust").is_none() {
        return;
    }
    let Some(loops) = loop_set("logdust") else {
        return;
    };

    let log_index = usize::from(SLOTS) + 2;
    let table = format!("dust {} 0 {DUST_BLOCK}", loops[log_index].path().display());
    let Some(dust) = DmDevice::create("logdust", loops[log_index].path(), &table) else {
        return;
    };

    let mut paths: Vec<PathBuf> = loops
        .iter()
        .map(|device| device.path().to_path_buf())
        .collect();
    paths[log_index] = dust.path();

    ferrite_harness::create_on(&paths).expect("Array anlegen");
    let expected = pattern(0x5F, 4096);
    {
        let (mut writer, _) = ferrite_harness::open_on(&paths).expect("oeffnen");
        writer.write(0, 0, &expected).expect("schreiben");
    }

    // Einen Block der Log-Region unlesbar machen.
    let payload_block = 1_048_576 / DUST_BLOCK;
    assert!(dust.message(&format!("addbadblock {payload_block}")));
    assert!(dust.message("enable"));
    drop_caches();

    // Das Oeffnen liest die ganze Log-Region und scheitert deshalb. Das ist
    // ehrlich: Ohne Log ist der Absturzpfad nicht zu beurteilen.
    let outcome = ferrite_harness::open_on(&paths);
    assert!(
        outcome.is_err(),
        "ein unlesbares Log wurde stillschweigend hingenommen"
    );

    // Die Data-Members bleiben trotzdem lesbar — mit einem frischen Log ist
    // das Array wieder da, und die Nutzdaten sind unversehrt.
    let (fresh_log, _) = (paths[log_index].clone(), ());
    let device = ferrite_engine::MemberDevice::open(&fresh_log).expect("Log-Geraet");
    let superblock = ferrite_engine::read_superblock(&device).expect("Log-Superblock");
    assert!(dust.message("disable"), "dust wieder abschalten");
    ferrite_engine::DeviceLog::initialize(device, &superblock).expect("Log neu anlegen");

    let (writer, _) = ferrite_harness::open_on(&paths).expect("mit frischem Log oeffnen");
    let mut read_back = vec![0u8; expected.len()];
    writer.read(0, 0, &mut read_back).expect("lesen");
    assert_eq!(
        read_back, expected,
        "die Nutzdaten haben den Log-Ausfall nicht ueberlebt"
    );
}
