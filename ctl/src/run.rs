// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Die Haelfte, die Platten anfasst.
//!
//! Entschieden wird hier nichts: Was auf die Geraete gehoert, sagt
//! [`ferrite_engine::create_array`], was ein Array taugt, sagt
//! [`ferrite_format::assemble`], und wie es sich liest, sagt
//! [`crate::report`]. Dieses Modul oeffnet, liest, schreibt — mehr nicht.

use std::fmt;
use std::path::{Path, PathBuf};

use ferrite_engine::{
    create_array, max_payload_size, read_superblock, ArraySpec, DeviceLog, EngineError,
    MemberDevice, MemberSpec,
};
use ferrite_format::superblock::Role;
use ferrite_format::Uuid;

use crate::args::CreatePlan;
use crate::report::{self, Planned, Report, Seen};

/// Warum ein Aufruf nicht durchging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CtlError {
    /// Ein Fehler an einem bestimmten Geraet.
    ///
    /// Der Pfad steht mit dabei, und das ist der ganze Zweck dieser Variante:
    /// `EngineError` weiss nur, **was** versucht wurde, nicht **woran**. Bei
    /// sechs Platten in einer Zeile ist das der Unterschied zwischen einer
    /// brauchbaren und einer nutzlosen Fehlermeldung.
    Device {
        path: PathBuf,
        source: EngineError,
    },
    /// Auf dem Geraet liegt bereits ein Ferrite-Superblock.
    AlreadyMember {
        path: PathBuf,
        array: String,
    },
    Engine(EngineError),
    /// Der Zufallsgenerator des Betriebssystems war nicht zu erreichen.
    NoRandomness(std::io::ErrorKind),
    /// Eine Voraussetzung des laufenden Betriebs fehlt.
    Missing {
        what: &'static str,
    },
    /// Auf dem Blockgeraet liegt kein Dateisystem der erwarteten Art.
    ///
    /// Eigene Variante und keine `Mount`-Meldung mit `EINVAL`: Das ist der
    /// Fall, in dem ein `mkfs` verlockend waere, und er gehoert so deutlich
    /// benannt, dass niemand ihn mit einem defekten Geraet verwechselt.
    NoFilesystem {
        device: String,
        fstype: String,
    },
    Mount {
        path: PathBuf,
        kind: std::io::ErrorKind,
        raw_os_error: Option<i32>,
    },
    Pool(ferrite_pool::PoolError),
}

impl fmt::Display for CtlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Device { path, source } => write!(f, "{}: {source}", path.display()),
            Self::AlreadyMember { path, array } => write!(
                f,
                "{} gehoert bereits zu Array {array}.\n\
                 Mit --force ueberschreiben — der Inhalt dieses Arrays ist danach weg.",
                path.display()
            ),
            Self::Engine(error) => write!(f, "{error}"),
            Self::NoRandomness(kind) => write!(
                f,
                "/dev/urandom nicht lesbar ({kind:?}) — ohne Zufall keine eindeutigen UUIDs"
            ),
            Self::Missing { what } => write!(f, "{what}"),
            Self::NoFilesystem { device, fstype } => write!(
                f,
                "auf {device} liegt kein {fstype}.\n\
                 Ferrite formatiert nicht von selbst — wer das Geraet neu anlegen will, ruft\n\
                 `mkfs.{fstype} {device}` auf. Wer es nicht will, hat vielleicht die falsche\n\
                 Platte erwischt."
            ),
            Self::Mount {
                path,
                kind,
                raw_os_error,
            } => match raw_os_error {
                Some(code) => write!(f, "{}: {kind:?} (errno {code})", path.display()),
                None => write!(f, "{}: {kind:?}", path.display()),
            },
            Self::Pool(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for CtlError {}

type Result<T> = std::result::Result<T, CtlError>;

fn at(path: &Path) -> impl FnOnce(EngineError) -> CtlError + '_ {
    move |source| CtlError::Device {
        path: path.to_path_buf(),
        source,
    }
}

// --- create ---------------------------------------------------------------

/// Legt ein Array an — oder zeigt nur, was es taete.
///
/// Gibt den Text zurueck, den der Aufrufer ausgibt, und ob geschrieben wurde.
/// Der Text wird **immer** gebaut, auch beim Trockenlauf: Er ist der eigentliche
/// Zweck des Trockenlaufs.
pub fn create(plan: &CreatePlan) -> Result<(String, bool)> {
    let devices = plan.devices();
    let roles = roles_of(plan);

    // Erst alles oeffnen und ansehen, dann erst urteilen. Ein Abbruch nach der
    // dritten Platte liesse die ersten beiden beschrieben zurueck.
    let mut opened = Vec::with_capacity(devices.len());
    for path in &devices {
        let device = MemberDevice::open(path).map_err(at(path))?;
        let occupied = read_superblock(&device).ok();
        opened.push((path.clone(), device, occupied));
    }

    if !plan.force {
        for (path, _, occupied) in &opened {
            if let Some(superblock) = occupied {
                return Err(CtlError::AlreadyMember {
                    path: path.clone(),
                    array: superblock.array_uuid.to_string(),
                });
            }
        }
    }

    let entries: Vec<Planned> = opened
        .iter()
        .zip(&roles)
        .map(|((path, device, occupied), (role, slot))| {
            Ok(Planned {
                device: path.display().to_string(),
                role: *role,
                slot_index: *slot,
                device_size: device.size(),
                payload_size: max_payload_size(device.size(), plan.block_size_log2)
                    .map_err(at(path))?,
                occupied: occupied.is_some(),
            })
        })
        .collect::<Result<_>>()?;

    let mut text = report::plan(&entries, plan.block_size_log2, plan.confirmed);
    if !plan.confirmed {
        return Ok((text, false));
    }

    let specs: Vec<MemberSpec> = roles
        .iter()
        .map(|(role, slot)| {
            Ok(MemberSpec {
                member_uuid: random_uuid()?,
                role: *role,
                slot_index: *slot,
                label: plan.label.clone(),
            })
        })
        .collect::<Result<_>>()?;

    let array = ArraySpec {
        array_uuid: random_uuid()?,
        parity_block_size_log2: plan.block_size_log2,
        created_unix: now_unix(),
    };

    let handles: Vec<MemberDevice> = opened.into_iter().map(|(_, device, _)| device).collect();
    let superblocks = create_array(&handles, &specs, &array).map_err(CtlError::Engine)?;

    // Die Log-Region nullen. Ohne das faende der erste Scan, was auf dem
    // Geraet vorher stand — und deutete es als Records.
    let log_superblock = superblocks
        .iter()
        .find(|superblock| superblock.role == Role::Log)
        .ok_or(CtlError::Engine(EngineError::CannotRebuild {
            role: Role::Log,
        }))?;
    let log_device = MemberDevice::open(&plan.log).map_err(at(&plan.log))?;
    DeviceLog::initialize(log_device, log_superblock).map_err(at(&plan.log))?;

    text.push_str(&format!("\nArray {} angelegt.\n", array.array_uuid));
    Ok((text, true))
}

/// Rolle und Slot je Geraet, in der Reihenfolge von [`CreatePlan::devices`].
fn roles_of(plan: &CreatePlan) -> Vec<(Role, u16)> {
    let mut roles: Vec<(Role, u16)> = (0..plan.data.len())
        .map(|slot| (Role::Data, slot as u16))
        .collect();
    roles.push((Role::ParityP, 0));
    if plan.parity_q.is_some() {
        roles.push((Role::ParityQ, 0));
    }
    roles.push((Role::Log, 0));
    roles
}

// --- status ---------------------------------------------------------------

/// Sieht sich die Geraete an, ohne eines zu beschreiben.
///
/// Ein Geraet, das sich nicht oeffnen laesst, bricht den Aufruf **nicht** ab:
/// Genau dann will man den Bericht ja sehen. Es erscheint darin als „nicht
/// lesbar" und macht das Array unvollstaendig — was `assemble` dann auch sagt.
pub fn status(devices: &[PathBuf]) -> Report {
    let seen: Vec<Seen> = devices
        .iter()
        .map(|path| Seen {
            device: path.display().to_string(),
            superblock: MemberDevice::open_read_only(path)
                .ok()
                .and_then(|device| read_superblock(&device).ok()),
        })
        .collect();
    report::status(&seen)
}

// --- Umgebung -------------------------------------------------------------

/// Sechzehn Bytes aus dem Zufallsgenerator des Betriebssystems.
///
/// Aus `/dev/urandom` und nicht aus Uhrzeit und Prozessnummer: Zwei
/// Rechner, die aus demselben Abbild starten und in derselben Sekunde ein
/// Array anlegen, bekaemen sonst dieselbe UUID — und zwei Arrays mit
/// derselben UUID setzen sich gegenseitig zusammen.
fn random_uuid() -> Result<Uuid> {
    use std::io::Read;

    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| CtlError::NoRandomness(error.kind()))?;
    Ok(Uuid::from_random_bytes(bytes))
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        // Eine Uhr vor 1970 macht ein Array nicht unbrauchbar; das Feld ist
        // Information, keine Gueltigkeitsregel.
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_for(data: &[&str], with_q: bool) -> CreatePlan {
        CreatePlan {
            data: data.iter().map(PathBuf::from).collect(),
            parity_p: PathBuf::from("/dev/p"),
            parity_q: with_q.then(|| PathBuf::from("/dev/q")),
            log: PathBuf::from("/dev/l"),
            block_size_log2: 16,
            label: String::new(),
            confirmed: false,
            force: false,
        }
    }

    #[test]
    fn the_roles_follow_the_order_of_the_devices() {
        // Die Zuordnung von Geraet zu Rolle entsteht an zwei Stellen —
        // `devices()` und `roles_of()`. Laufen sie auseinander, bekommt eine
        // Platte die Rolle einer anderen, und das faellt erst beim ersten
        // Ausfall auf.
        let plan = plan_for(&["/dev/a", "/dev/b", "/dev/c"], true);
        let devices = plan.devices();
        let roles = roles_of(&plan);

        assert_eq!(devices.len(), roles.len());
        assert_eq!(roles[0], (Role::Data, 0));
        assert_eq!(roles[1], (Role::Data, 1));
        assert_eq!(roles[2], (Role::Data, 2));
        assert_eq!(roles[3], (Role::ParityP, 0));
        assert_eq!(roles[4], (Role::ParityQ, 0));
        assert_eq!(roles[5], (Role::Log, 0));
        assert_eq!(devices[5], PathBuf::from("/dev/l"));
    }

    #[test]
    fn without_q_the_log_moves_up_one_place() {
        let plan = plan_for(&["/dev/a"], false);
        let devices = plan.devices();
        let roles = roles_of(&plan);
        assert_eq!(devices.len(), roles.len());
        assert_eq!(roles.last(), Some(&(Role::Log, 0)));
        assert_eq!(devices.last(), Some(&PathBuf::from("/dev/l")));
    }

    #[test]
    fn the_slot_indices_have_no_gaps() {
        let plan = plan_for(&["/dev/a", "/dev/b", "/dev/c", "/dev/d"], true);
        let slots: Vec<u16> = roles_of(&plan)
            .into_iter()
            .filter(|(role, _)| *role == Role::Data)
            .map(|(_, slot)| slot)
            .collect();
        assert_eq!(slots, vec![0, 1, 2, 3]);
    }

    #[test]
    fn two_uuids_in_a_row_are_different() {
        // Kein Beweis fuer Zufall, aber der Nachweis, dass hier nicht
        // versehentlich eine Konstante steht.
        let first = random_uuid().expect("/dev/urandom");
        let second = random_uuid().expect("/dev/urandom");
        assert_ne!(first, second);
    }
}
