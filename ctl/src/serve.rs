// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Der laufende Betrieb: `ferrite run`.
//!
//! # Die Reihenfolge
//!
//! ```text
//! oeffnen → assemble → Log zurueckspielen → ublk je Slot
//!         → [Members einhaengen → Pool einhaengen] → warten
//! ```
//!
//! und beim Beenden rueckwaerts. Tragend daran ist **eine** Grenze: Es muss
//! alles ausgehaengt sein, bevor die Blockgeraete abgebaut werden. Ein
//! ublk-Geraet, das noch jemand offen hat, laesst sich nicht abbauen — der
//! Kernel wartet darin ohne Zeitgrenze, und ein eingehaengtes Dateisystem
//! haelt es offen. Nachgemessen: Die umgekehrte Reihenfolge laeuft in die
//! Zeitgrenze des Tests statt sich zu beenden.
//!
//! Die Reihenfolge **innerhalb** der Unmounts — Pool vor Members — ist
//! dagegen nur Sorgfalt: `MNT_DETACH` loest sofort, auch wenn darueber noch
//! etwas haengt. Sie steht trotzdem so da, weil sie nichts kostet und weil
//! ein `umount` ohne `MNT_DETACH` eines Tages die richtige Wahl sein koennte.
//!
//! # Das Log kommt vor dem ersten Blockgeraet
//!
//! `recover` liest den Ringpuffer und rechnet die Paritaet fuer die Bereiche
//! neu, bei denen der Strom zwischen Data-Member und Paritaet ausfiel
//! (Abschnitt 5.2). Erst danach darf ein Gast lesen. Andersherum saehe er
//! einen Zustand, den das Recovery gleich darauf aendert.
//!
//! # Was hier bewusst nicht passiert
//!
//! **Formatiert wird nicht.** Ein Data-Member ohne Dateisystem wird gemeldet,
//! und dann entscheidet ein Mensch. Ein Werkzeug, das im Zweifel `mkfs`
//! aufruft, loescht irgendwann die Platte, die nur schlecht angeschlossen war.

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ferrite_engine::ublk::{ArraySlot, UblkDevice, UblkSpec, CONTROL_PATH};
use ferrite_engine::Member;
use ferrite_pool::fuse::{BranchRoot, Connection, MountOptions, PoolFs};
use ferrite_pool::{BranchId, SharePolicy};

use crate::args::RunPlan;
use crate::run::CtlError;

type Result<T> = std::result::Result<T, CtlError>;

/// Wurde ein Beendigungssignal empfangen?
///
/// Ein `AtomicBool` und sonst nichts: In einem Signalhandler ist fast alles
/// verboten, was ein Programm sonst tut — kein `malloc`, kein `println!`,
/// keine Sperre. Ein Flag zu setzen ist erlaubt, alles Weitere macht der
/// gewoehnliche Programmablauf.
static STOPPING: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_signal: libc::c_int) {
    STOPPING.store(true, Ordering::SeqCst);
}

fn catch_signals() {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        // SAFETY: `on_signal` beruehrt nur ein `AtomicBool`.
        unsafe { libc::signal(signal, on_signal as *const () as libc::sighandler_t) };
    }
}

/// Nimmt das Array in Betrieb und bleibt, bis ein Signal kommt.
///
/// Schreibt unterwegs, was es tut — wer `run` startet, will wissen, welche
/// Geraete entstanden sind, ohne sie suchen zu muessen.
pub fn run(plan: &RunPlan) -> Result<()> {
    if !Path::new(CONTROL_PATH).exists() {
        return Err(CtlError::Missing {
            what: "ublk_drv ist nicht geladen (/dev/ublk-control fehlt)",
        });
    }

    let settings = resolve(plan)?;
    let writer = crate::run::open_array(&settings.devices)?;

    // Je Slot seine **eigene** Groesse. Members duerfen verschieden gross
    // sein — das ist der Kern des Projekts, und ein Blockgeraet, das fuer
    // alle die Groesse des ersten meldete, waere entweder zu klein oder
    // zeigte auf einer kurzen Platte Bereiche, die es nicht gibt.
    let sizes: Vec<u64> = (0..u16::from(writer.data_slot_count()))
        .map(|slot| {
            writer
                .member(slot)
                .map(Member::payload_size)
                .map_err(CtlError::Engine)
        })
        .collect::<Result<_>>()?;

    // Erst ab hier Signale fangen. Vorher waere ein Abbruch ohnehin nur ein
    // Programmende ohne Zustand.
    catch_signals();

    let shared = Arc::new(Mutex::new(writer));
    let mut devices = Vec::new();

    for (slot, size) in sizes.iter().copied().enumerate() {
        let slot = slot as u16;
        let spec = UblkSpec {
            size,
            ..UblkSpec::default()
        };
        let target = ArraySlot::new(Arc::clone(&shared), slot);
        let device = UblkDevice::start(&spec, vec![target]).map_err(CtlError::Engine)?;
        let path = device.block_path();
        if !wait_for(&path) {
            // Ohne den Knoten kann niemand etwas damit anfangen. Die schon
            // gestarteten Geraete werden beim Fallenlassen abgebaut.
            stop_all(devices);
            return Err(CtlError::Missing {
                what: "der Geraeteknoten ist nicht aufgetaucht — laeuft udev?",
            });
        }
        println!("Slot {slot}: {path}");
        devices.push((slot, device, path));
    }

    let mounted = match &settings.pool {
        Some(mountpoint) => match mount_pool(&settings, mountpoint, &devices) {
            Ok(mounted) => Some(mounted),
            Err(error) => {
                // Was schon eingehaengt war, ist in `mount_pool` bereits
                // zurueckgenommen; die Blockgeraete stehen noch.
                stop_all(devices);
                return Err(error);
            }
        },
        None => {
            println!("Kein --pool angegeben: die Blockgeraete stehen bereit, mehr nicht.");
            None
        }
    };

    println!("Bereit. Beenden mit Strg-C oder SIGTERM.");
    wait_for_signal();
    println!("\nBeende.");

    // **Erst aushaengen, dann die Blockgeraete abbauen.** Andersherum wartet
    // `stop` im Kernel darauf, dass niemand mehr `/dev/ublkbN` offen hat — und
    // das eingehaengte Dateisystem haelt es offen. Nachgemessen: Die
    // umgekehrte Reihenfolge laeuft in die Zeitgrenze des Tests.
    if let Some(mounted) = mounted {
        mounted.stop();
    }
    stop_all(devices);
    println!("Alles abgebaut.");
    Ok(())
}

// --- Kommandozeile und Konfiguration zusammenlegen -------------------------

/// Was `run` wirklich tut.
#[derive(Debug, Clone)]
struct Settings {
    devices: Vec<PathBuf>,
    pool: Option<PathBuf>,
    state_dir: PathBuf,
    fstype: String,
    policy: SharePolicy,
}

/// Legt Kommandozeile und Konfiguration uebereinander.
///
/// **Die Kommandozeile gewinnt.** Wer beim Suchen eines Fehlers etwas von Hand
/// angibt, will nicht, dass eine Datei es ueberstimmt.
///
/// Die Geraeteliste ist der Sonderfall: Ist sie leer, wird gesucht, wo die
/// Konfiguration es sagt. Genau so startet die systemd-Unit — `ferrite run`
/// ohne ein einziges Argument.
fn resolve(plan: &RunPlan) -> Result<Settings> {
    let config = crate::run::load_config(plan.config.as_deref())?;
    let devices = crate::run::devices_or_search(&plan.devices, &config)?;
    if plan.devices.is_empty() {
        println!("{} Members gefunden.", devices.len());
    }

    Ok(Settings {
        devices,
        pool: plan.pool.clone().or(config.pool.clone()),
        state_dir: plan
            .state_dir
            .clone()
            .unwrap_or_else(|| config.state_dir.clone()),
        fstype: plan.fstype.clone().unwrap_or_else(|| config.fstype.clone()),
        policy: config.share_policy(),
    })
}

// --- Der Pool -------------------------------------------------------------

/// Was eingehaengt wurde und wieder abzubauen ist.
///
/// `mounts` steht in **Abbaureihenfolge**: zuerst der Pool, dann die Members
/// darunter. Der `Connection` des Pools liegt im Arbeitsthread und ist von
/// hier nicht mehr erreichbar — ausgehaengt wird deshalb ueber den Pfad. Das
/// ist kein Umweg: `umount` beendet die Schleife im Thread, weil ihr `read`
/// danach `ENODEV` liefert.
struct Mounted {
    worker: Option<std::thread::JoinHandle<()>>,
    mounts: Vec<PathBuf>,
}

impl Mounted {
    fn stop(mut self) {
        for mount in &self.mounts {
            let _ = unmount(mount);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn mount_pool(
    settings: &Settings,
    mountpoint: &Path,
    devices: &[(u16, UblkDevice, String)],
) -> Result<Mounted> {
    let mut members: Vec<PathBuf> = Vec::new();
    let mut branches = Vec::new();

    for (slot, _, block_path) in devices {
        let target = settings.state_dir.join(format!("slot{slot}"));
        std::fs::create_dir_all(&target).map_err(io_at(&target))?;

        if let Err(error) = mount(block_path, &target, &settings.fstype) {
            for done in &members {
                let _ = unmount(done);
            }
            return Err(match error.raw_os_error() {
                // Genau der Fall, in dem `mkfs` verlockend waere. Er wird
                // gemeldet, damit ein Mensch entscheidet.
                Some(libc::EINVAL) => CtlError::NoFilesystem {
                    device: block_path.clone(),
                    fstype: settings.fstype.clone(),
                },
                _ => CtlError::Mount {
                    path: target.clone(),
                    kind: error.kind(),
                    raw_os_error: error.raw_os_error(),
                },
            });
        }
        members.push(target.clone());
        branches.push(BranchRoot::new(BranchId(*slot), target));
    }

    std::fs::create_dir_all(mountpoint).map_err(io_at(mountpoint))?;
    let connection = match Connection::mount(mountpoint, &MountOptions::default()) {
        Ok(connection) => connection,
        Err(error) => {
            for done in &members {
                let _ = unmount(done);
            }
            return Err(CtlError::Pool(error));
        }
    };

    let policy = settings.policy.clone();
    // Die Schleife laeuft in einem eigenen Thread; der Hauptthread wartet auf
    // das Signal. Andersherum haette das Signal keinen, der es bemerkt.
    let worker = std::thread::spawn(move || {
        let mut filesystem = PoolFs::new(branches, policy);
        if let Err(error) = filesystem.run(&connection) {
            eprintln!("Der Pool ist gestolpert: {error}");
        }
    });

    println!("Pool eingehaengt unter {}", mountpoint.display());

    // Der Pool zuerst: Solange er steht, haelt er die Member-Mounts offen.
    let mut mounts = vec![mountpoint.to_path_buf()];
    mounts.extend(members);
    Ok(Mounted {
        worker: Some(worker),
        mounts,
    })
}

// --- Systemaufrufe --------------------------------------------------------

fn mount(source: &str, target: &Path, fstype: &str) -> std::io::Result<()> {
    let source = CString::new(source)?;
    let fstype = CString::new(fstype)?;
    let target = c_path(target)?;
    // `MS_NOSUID | MS_NODEV`: Auf einem Member liegen Nutzdaten, keine
    // Programme und keine Geraete.
    let flags = libc::MS_NOSUID | libc::MS_NODEV;
    let result = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            flags,
            std::ptr::null(),
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn unmount(target: &Path) -> std::io::Result<()> {
    let target = c_path(target)?;
    if unsafe { libc::umount2(target.as_ptr(), libc::MNT_DETACH) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn c_path(path: &Path) -> std::io::Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))
}

/// Wartet, bis udev den Geraeteknoten angelegt hat.
///
/// `START_DEV` kehrt zurueck, sobald das Geraet im Kernel lebt; den Knoten
/// unter `/dev` legt udev an, und das dauert einen Moment.
fn wait_for(path: &str) -> bool {
    for _ in 0..200 {
        if Path::new(path).exists() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    false
}

fn wait_for_signal() {
    while !STOPPING.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn stop_all(devices: Vec<(u16, UblkDevice, String)>) {
    for (slot, device, _) in devices {
        if let Err(error) = device.stop() {
            eprintln!("Slot {slot} liess sich nicht abbauen: {error}");
        }
    }
}

fn io_at(path: &Path) -> impl FnOnce(std::io::Error) -> CtlError + '_ {
    move |error| CtlError::Mount {
        path: path.to_path_buf(),
        kind: error.kind(),
        raw_os_error: error.raw_os_error(),
    }
}
