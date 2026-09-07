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
use ferrite_format::superblock::{Role, Superblock};
use ferrite_format::Uuid;

use crate::args::CreatePlan;
use crate::report::{self, Planned, Report, Seen};
use ferrite_engine::{member_for, ArrayWriter, Member};
use ferrite_format::assemble;

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
    /// Die Konfigurationsdatei ist nicht benutzbar.
    Config {
        path: PathBuf,
        reason: String,
    },
    /// Aus dem Suchlauf wurde kein Array.
    Discover(String),
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
            Self::Config { path, reason } => write!(f, "{}: {reason}", path.display()),
            Self::Discover(reason) => write!(f, "{reason}"),
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
pub fn status(devices: &[PathBuf], named: Option<&Path>) -> Report {
    // Leere Liste heisst suchen. Scheitert die Suche, wird das gemeldet und
    // nicht als „nichts gefunden" ausgegeben — der Unterschied zwischen
    // „keine Platte da" und „zwei Arrays, sag welches" ist der ganze Punkt.
    let config = match load_config(named) {
        Ok(config) => config,
        Err(error) => {
            return Report {
                text: format!(
                    "{error}
"
                ),
                health: crate::report::Health::Broken,
            }
        }
    };
    let devices = match devices_or_search(devices, &config) {
        Ok(devices) => devices,
        Err(error) => {
            return Report {
                text: format!(
                    "{error}
"
                ),
                health: crate::report::Health::Broken,
            }
        }
    };

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

/// Derselbe Zustand als JSON, samt der Bewertung fuer den Rueckgabewert.
///
/// # Warum die Fehlerfaelle auch JSON sind
///
/// Eine Oberflaeche, die bei „Konfiguration nicht lesbar" statt einer Antwort
/// einen deutschen Satz bekommt, zeigt eine leere Seite und sagt nicht warum.
/// Deshalb wird auch das Scheitern als Objekt geliefert — mit `error` gefuellt
/// und `health` auf `broken`.
pub fn status_json(devices: &[PathBuf], named: Option<&Path>) -> (String, crate::report::Health) {
    use crate::json::Value;
    use crate::report::Health;

    let broken = |error: String| {
        (
            Value::object(vec![
                ("array", Value::Null),
                ("health", Value::text("broken")),
                (
                    "exit_code",
                    Value::Number(u64::from(Health::Broken.exit_code())),
                ),
                ("members", Value::List(Vec::new())),
                ("error", Value::text(error)),
            ])
            .render(),
            Health::Broken,
        )
    };

    let config = match load_config(named) {
        Ok(config) => config,
        Err(error) => return broken(error.to_string()),
    };
    let devices = match devices_or_search(devices, &config) {
        Ok(devices) => devices,
        Err(error) => return broken(error.to_string()),
    };

    let seen: Vec<Seen> = devices
        .iter()
        .map(|path| Seen {
            device: path.display().to_string(),
            superblock: MemberDevice::open_read_only(path)
                .ok()
                .and_then(|device| read_superblock(&device).ok()),
        })
        .collect();
    (report::status_json(&seen), report::survey(&seen).health)
}

/// Was der taegliche Blick ergeben hat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub report: Report,
    /// Der zuletzt aufgezeichnete Zustand, falls je einer aufgezeichnet wurde.
    pub previous: Option<u8>,
    /// Wurde ein Eintrag geschrieben — und damit gemeldet?
    pub reported: bool,
    /// Warum nicht, falls nicht.
    pub trouble: Option<String>,
}

/// Sieht nach, wie es dem Array geht, und meldet sich bei einer Aenderung.
///
/// # Warum das ein eigener Aufruf ist und kein `status --notify`
///
/// `status` ist der Blick eines Menschen und darf nichts tun. Dies hier ist
/// der Blick eines Zeitplans: Er schreibt ins Tagebuch und ruft den
/// `notify`-Befehl. Beides gehoert nicht in einen Aufruf, den jemand
/// beilaeufig auf der Kommandozeile macht.
///
/// # Die Regel, an der alles haengt
///
/// **Gemeldet wird nur eine Aenderung.** Ein Array, das seit drei Wochen
/// degradiert laeuft, schickt keine 21 Mails; es hat die eine geschickt, als
/// es degradiert ist. Eine Meldung, die jeden Tag dasselbe sagt, wird nach
/// einer Woche nicht mehr gelesen — und dann wird auch die uebersehen, die
/// etwas Neues sagt.
///
/// Und die Entwarnung zaehlt als Aenderung. Wer nach einem Rebuild keine
/// bekommt, sieht so lange nach, bis er aufhoert nachzusehen.
///
/// # Woher das Gedaechtnis kommt
///
/// Aus dem Tagebuch. Ohne `journal =` in der Konfiguration gibt es keines,
/// und dann kann dieser Aufruf nichts vergleichen — er sagt das und meldet
/// nichts, statt jeden Tag dieselbe Mail zu schicken.
pub fn check(devices: &[PathBuf], named: Option<&Path>) -> Check {
    use crate::journal::{summarize, Event};

    let report = status(devices, named);
    let health = report.health.exit_code();

    let config = match load_config(named) {
        Ok(config) => config,
        Err(error) => {
            return Check {
                report,
                previous: None,
                reported: false,
                trouble: Some(error.to_string()),
            }
        }
    };
    let Some(path) = &config.journal else {
        return Check {
            report,
            previous: None,
            reported: false,
            trouble: Some(
                "Es wird kein Tagebuch gefuehrt — ohne eines gibt es nichts zu vergleichen. \
                 `journal = /var/lib/ferrite/journal` in die Konfiguration eintragen."
                    .to_string(),
            ),
        };
    };

    // Eine Datei, die es noch nicht gibt, ist ein leeres Tagebuch und kein
    // Fehler: Beim allerersten Lauf ist genau das der Normalfall.
    let previous = summarize(&std::fs::read_to_string(path).unwrap_or_default()).last_health;

    // Nichts aufgezeichnet und alles in Ordnung: Dann gibt es nichts zu
    // sagen. Schweigen ist hier die richtige Antwort, und ein Eintrag
    // „Array in Ordnung" waere der erste von 365.
    if previous.is_none() && health == 0 {
        return Check {
            report,
            previous,
            reported: false,
            trouble: None,
        };
    }
    if previous == Some(health) {
        return Check {
            report,
            previous,
            reported: false,
            trouble: None,
        };
    }

    let event = Event::HealthChanged {
        from: previous.unwrap_or(0),
        to: health,
    };
    match crate::journal::record(&config, &event) {
        Ok(()) => Check {
            report,
            previous,
            reported: true,
            trouble: None,
        },
        // Der Befund selbst bleibt stehen, auch wenn das Melden schiefging.
        // Ein Aufruf, der wegen einer kaputten Tagebuchdatei den Ausfall
        // verschweigt, waere die schlechteste aller Antworten.
        Err(error) => Check {
            report,
            previous,
            reported: false,
            trouble: Some(error.to_string()),
        },
    }
}

// --- Umgebung -------------------------------------------------------------

/// Sechzehn Bytes aus dem Zufallsgenerator des Betriebssystems.
///
/// Aus `/dev/urandom` und nicht aus Uhrzeit und Prozessnummer: Zwei
/// Rechner, die aus demselben Abbild starten und in derselben Sekunde ein
/// Array anlegen, bekaemen sonst dieselbe UUID — und zwei Arrays mit
/// derselben UUID setzen sich gegenseitig zusammen.
pub(crate) fn random_uuid() -> Result<Uuid> {
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

// --- Konfiguration und Suche ----------------------------------------------

/// Liest die Konfiguration, oder nimmt die Voreinstellungen.
///
/// Eine fehlende Datei am ueblichen Ort ist kein Fehler — dann arbeitet
/// Ferrite mit den Voreinstellungen, und wer sie nicht braucht, muss keine
/// anlegen. Eine **ausdruecklich benannte** Datei, die fehlt, ist einer: Wer
/// `--config` schreibt, meint eine bestimmte.
pub fn load_config(named: Option<&Path>) -> Result<crate::config::Config> {
    let path = named
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(crate::config::DEFAULT_PATH));

    match std::fs::read_to_string(&path) {
        Ok(text) => crate::config::Config::parse(&text).map_err(|error| CtlError::Config {
            path: path.clone(),
            reason: error.to_string(),
        }),
        Err(_) if named.is_none() => Ok(crate::config::Config::default()),
        Err(error) => Err(CtlError::Config {
            path,
            reason: error.to_string(),
        }),
    }
}

/// Die Geraeteliste — von der Kommandozeile oder aus dem Suchlauf.
///
/// Eine leere Liste ist die Aufforderung zu suchen und kein Fehler. Genau so
/// rufen die systemd-Units auf: `ferrite run`, `ferrite scrub` — ohne ein
/// einziges Argument.
pub fn devices_or_search(
    explicit: &[PathBuf],
    config: &crate::config::Config,
) -> Result<Vec<PathBuf>> {
    if !explicit.is_empty() {
        return Ok(explicit.to_vec());
    }

    let scan = crate::discover::scan(&config.scan);
    let members = scan
        .pick(config.array.as_deref())
        .map_err(|error| CtlError::Discover(error.to_string()))?;
    Ok(crate::discover::paths(members))
}

// --- Das Array oeffnen ----------------------------------------------------

/// Oeffnet die Geraete, prueft sie mit `assemble` und spielt das Log zurueck.
///
/// Steht hier und nicht in `serve`, weil `scrub`, `rebuild` und der laufende
/// Betrieb dasselbe brauchen — und weil die ersten beiden ohne ublk und ohne
/// Root auskommen. Ein Array laesst sich auf Dateien pruefen und
/// wiederherstellen; nur der Betrieb braucht Blockgeraete.
pub fn open_array(devices: &[PathBuf]) -> Result<ArrayWriter> {
    let mut superblocks = Vec::with_capacity(devices.len());
    for path in devices {
        let device = MemberDevice::open_read_only(path).map_err(at(path))?;
        superblocks.push(read_superblock(&device).map_err(at(path))?);
    }

    // Dieselbe Pruefung wie ueberall. Sie sagt auch, welches Geraet welche
    // Rolle traegt — gefragt wird danach nicht.
    let layout = assemble(&superblocks)
        .map_err(EngineError::Format)
        .map_err(CtlError::Engine)?;

    let data: Vec<Member> = (0..layout.data_slot_count() as u16)
        .map(|slot| {
            let position = layout.data_position(slot).ok_or(CtlError::Missing {
                what: "assemble hat einen Data-Slot ohne Member durchgelassen",
            })?;
            open_member(&devices[position], &superblocks[position], Role::Data)
        })
        .collect::<Result<_>>()?;

    let p = layout.parity_p_position();
    let parity_p = open_member(&devices[p], &superblocks[p], Role::ParityP)?;
    let parity_q = match layout.parity_q_position() {
        Some(q) => Some(open_member(&devices[q], &superblocks[q], Role::ParityQ)?),
        None => None,
    };

    let log_position = layout.log_position().ok_or(CtlError::Missing {
        what: "das Array hat kein Log — ohne eines gibt es kein Recovery",
    })?;
    let log_device =
        MemberDevice::open(&devices[log_position]).map_err(at(&devices[log_position]))?;
    let (log, recovery) = DeviceLog::open(log_device, &superblocks[log_position])
        .map_err(at(&devices[log_position]))?;

    let mut writer = ArrayWriter::new(log, data, parity_p, parity_q).map_err(CtlError::Engine)?;

    // Vor dem ersten Blockgeraet, nicht danach.
    let recovered = writer.recover(&recovery).map_err(CtlError::Engine)?;
    // Auch ein Recovery ohne Arbeit gehoert ins Tagebuch: Nach einem Jahr
    // ist die Zahl der sauberen Starts genauso eine Aussage wie die der
    // unsauberen.
    crate::repair::note(
        &load_config(None).unwrap_or_default(),
        &crate::journal::Event::Recovered {
            applied: recovered.applied,
            lost: recovered.lost.len(),
        },
    );
    if recovered.applied > 0 {
        println!(
            "Recovery: {} Writes aus dem Log angewendet.",
            recovered.applied
        );
    }
    for lost in &recovered.lost {
        // Das ist der Fall aus Meilenstein 3: Absturz im degradierten
        // Betrieb. Er wird genannt, nicht verschwiegen.
        println!(
            "  verloren: Slot {} bei Offset {} ueber {} Bytes",
            lost.slot_index, lost.offset, lost.len
        );
    }
    Ok(writer)
}

fn open_member(path: &Path, superblock: &Superblock, role: Role) -> Result<Member> {
    let device = MemberDevice::open(path).map_err(at(path))?;
    member_for(device, superblock, role).map_err(at(path))
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
