// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Die Kommandozeile: Zerlegen und Pruefen, ohne ein Geraet anzufassen.
//!
//! # Warum von Hand
//!
//! Dieselbe Ueberlegung wie bei `io-uring` und `/dev/fuse`, nur andersherum:
//! Dort ging es nicht ohne fremden Code, hier geht es ohne. Ein
//! Argumentparser ist ein paar hundert Zeilen, seine Fehler zeigen sich sofort
//! beim ersten Aufruf, und die Fehlermeldungen gehoeren bei einem Werkzeug,
//! das Platten beschreibt, in dieselbe Hand wie der Rest.
//!
//! # Reines Modul
//!
//! Hier wird nichts geoeffnet und nichts geschrieben. Das ist der Grund,
//! warum sich jede Regel dieser Kommandozeile mit einem `&[&str]` pruefen
//! laesst — auch die, die sonst eine Platte braeuchte.

use std::path::PathBuf;

use ferrite_format::superblock::{MAX_PARITY_BLOCK_LOG2, MIN_PARITY_BLOCK_LOG2};

/// Voreingestellte Groesse eines Parity-Blocks: 64 KiB.
///
/// Dieselbe Zahl, die `Superblock::new` setzt. Sie steht hier noch einmal,
/// weil die Kommandozeile sie in ihrer Hilfe nennt — nicht als zweite
/// Wahrheit: Weicht sie ab, faellt es im Test `the_default_matches_the_format`
/// auf.
pub const DEFAULT_BLOCK_LOG2: u8 = 16;

/// Was der Aufrufer will.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Ein Array anlegen.
    Create(Box<CreatePlan>),
    /// Den Zustand eines Arrays zeigen.
    Status(StatusRequest),
    /// Das Array in Betrieb nehmen und dort bleiben.
    Run(Box<RunPlan>),
    /// Die Paritaet gegen den Inhalt der Data-Members pruefen.
    Scrub(ScrubRequest),
    /// Eine ausgefallene Platte durch eine neue ersetzen.
    Replace(Box<ReplacePlan>),
    /// Einen als unbrauchbar gemeldeten Member wiederherstellen.
    Rebuild(RebuildRequest),
    /// Fragen, ob das `FLUSH` eines Geraets ehrlich ist.
    CheckFlush(StatusRequest),
    /// Zeigen, welche Ferrite-Arrays angeschlossen sind.
    Discover(DiscoverRequest),
    /// Der taegliche Blick: nachsehen und bei einer Aenderung melden.
    Check(StatusRequest),
    /// Das Betriebstagebuch auswerten.
    Journal {
        config: Option<PathBuf>,
        json: bool,
    },
    Help,
    Version,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoverRequest {
    /// Wo gesucht wird. Leer heisst: was in der Konfiguration steht.
    pub scan: Vec<PathBuf>,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrubRequest {
    pub devices: Vec<PathBuf>,
    pub config: Option<PathBuf>,
    /// Eine nicht passende Paritaet neu bilden, statt sie nur zu melden.
    pub repair: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplacePlan {
    /// Die Geraete, die noch da sind — **ohne** das ausgefallene.
    pub devices: Vec<PathBuf>,
    pub slot_index: u16,
    /// Die neue Platte.
    pub replacement: PathBuf,
    pub confirmed: bool,
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuildRequest {
    pub devices: Vec<PathBuf>,
    pub config: Option<PathBuf>,
    pub slot_index: u16,
    /// Wieviele Bloecke je Durchgang.
    ///
    /// Zwischen zwei Durchgaengen wird der Fortschritt in den Superblock
    /// geschrieben. Klein heisst: nach einem Absturz weniger Arbeit doppelt.
    /// Gross heisst: weniger Schreibvorgaenge auf den Superblock.
    pub batch: u64,
}

/// Bloecke je Durchgang, wenn nichts anderes gesagt wird.
pub const DEFAULT_BATCH: u64 = 64;

/// Was `run` in Betrieb nehmen soll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPlan {
    /// Leer heisst: aus der Konfiguration suchen. Das ist der Fall, in dem
    /// eine systemd-Unit `ferrite run` ohne ein einziges Argument startet.
    pub devices: Vec<PathBuf>,
    /// Woher die Konfiguration kommt. `None` heisst: der uebliche Ort.
    pub config: Option<PathBuf>,
    /// Wo der vereinigte Pool eingehaengt wird. `None` heisst: nur die
    /// Blockgeraete bereitstellen, den Rest macht der Betreiber selbst.
    pub pool: Option<PathBuf>,
    /// Unter welchem Verzeichnis die einzelnen Members eingehaengt werden.
    pub state_dir: Option<PathBuf>,
    /// Welches Dateisystem auf den Members liegt.
    pub fstype: Option<String>,
}

/// Wo die Members eingehaengt werden, wenn nichts anderes gesagt wird.
///
/// Unter `/run`, weil es ein Laufzeitzustand ist: Nach einem Neustart soll
/// nichts davon uebrig sein.
pub const DEFAULT_STATE_DIR: &str = "/run/ferrite";

/// Ein geplantes Array, so wie es die Kommandozeile beschreibt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatePlan {
    pub data: Vec<PathBuf>,
    pub parity_p: PathBuf,
    pub parity_q: Option<PathBuf>,
    pub log: PathBuf,
    pub block_size_log2: u8,
    pub label: String,
    /// Ohne dies wird nur der Plan gezeigt und nichts geschrieben.
    pub confirmed: bool,
    /// Auch Geraete beschreiben, auf denen schon ein Ferrite-Superblock liegt.
    pub force: bool,
}

impl CreatePlan {
    /// Alle beteiligten Geraete in der Reihenfolge Data, P, Q, Log.
    pub fn devices(&self) -> Vec<PathBuf> {
        let mut devices = self.data.clone();
        devices.push(self.parity_p.clone());
        if let Some(parity_q) = &self.parity_q {
            devices.push(parity_q.clone());
        }
        devices.push(self.log.clone());
        devices
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusRequest {
    pub devices: Vec<PathBuf>,
    /// Woher die Konfiguration kommt, wenn die Liste leer ist.
    pub config: Option<PathBuf>,
    /// Maschinenlesbar statt gesetzt.
    ///
    /// Der Text ist fuer Menschen und darf sich aendern; wer ihn zerlegt,
    /// bricht beim ersten neuen Wort. Mit `--json` gibt es dieselben Zahlen
    /// in einer Form, auf die sich jemand verlassen darf.
    pub json: bool,
}

/// Warum eine Kommandozeile nicht benutzbar ist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgError {
    NoCommand,
    UnknownCommand(String),
    UnknownOption {
        command: &'static str,
        option: String,
    },
    MissingValue(String),
    MissingOption {
        command: &'static str,
        option: &'static str,
    },
    /// Dasselbe Geraet steht mehrfach in der Zeile.
    DuplicateDevice(PathBuf),
    BadBlockSize(String),
    BadNumber {
        option: &'static str,
        value: String,
    },
    NoDevices,
}

impl std::fmt::Display for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCommand => write!(f, "kein Kommando angegeben"),
            Self::UnknownCommand(name) => write!(f, "unbekanntes Kommando: {name}"),
            Self::UnknownOption { command, option } => {
                write!(f, "{command} kennt die Option {option} nicht")
            }
            Self::MissingValue(option) => write!(f, "{option} braucht einen Wert"),
            Self::MissingOption { command, option } => {
                write!(f, "{command} braucht {option}")
            }
            Self::DuplicateDevice(path) => write!(
                f,
                "{} steht mehrfach in der Zeile — ein Geraet kann nicht zwei Rollen tragen",
                path.display()
            ),
            Self::BadBlockSize(value) => write!(
                f,
                "unbrauchbare Blockgroesse {value}: erwartet eine Zweierpotenz zwischen 4K und 16M, etwa 64K"
            ),
            Self::BadNumber { option, value } => {
                write!(f, "{option} braucht eine Zahl, nicht {value}")
            }
            Self::NoDevices => write!(f, "keine Geraete angegeben"),
        }
    }
}

impl std::error::Error for ArgError {}

/// Zerlegt die Kommandozeile — **ohne** den Programmnamen.
pub fn parse(arguments: &[String]) -> Result<Command, ArgError> {
    let Some((command, rest)) = arguments.split_first() else {
        return Err(ArgError::NoCommand);
    };
    match command.as_str() {
        "create" => parse_create(rest).map(|plan| Command::Create(Box::new(plan))),
        "status" => parse_status(rest, "status").map(Command::Status),
        "check" => parse_status(rest, "check").map(Command::Check),
        "run" => parse_run(rest).map(|plan| Command::Run(Box::new(plan))),
        "scrub" => parse_scrub(rest),
        "replace" => parse_replace(rest).map(|plan| Command::Replace(Box::new(plan))),
        "rebuild" => parse_rebuild(rest),
        "check-flush" => {
            let devices: Vec<PathBuf> = rest.iter().map(PathBuf::from).collect();
            if devices.is_empty() {
                return Err(ArgError::NoDevices);
            }
            Ok(Command::CheckFlush(StatusRequest {
                devices,
                config: None,
                json: false,
            }))
        }
        "discover" => {
            let json = rest.iter().any(|argument| argument == "--json");
            let scan: Vec<PathBuf> = rest
                .iter()
                .filter(|argument| !argument.starts_with("--"))
                .map(PathBuf::from)
                .collect();
            if let Some(other) = rest
                .iter()
                .find(|argument| argument.starts_with("--") && *argument != "--json")
            {
                return Err(ArgError::UnknownOption {
                    command: "discover",
                    option: other.to_string(),
                });
            }
            Ok(Command::Discover(DiscoverRequest { scan, json }))
        }
        "journal" => {
            let mut config = None;
            let mut json = false;
            let mut index = 0;
            while index < rest.len() {
                match rest[index].as_str() {
                    "--config" => {
                        let value = rest
                            .get(index + 1)
                            .ok_or_else(|| ArgError::MissingValue("--config".to_string()))?;
                        config = Some(PathBuf::from(value));
                        index += 2;
                    }
                    "--json" => {
                        json = true;
                        index += 1;
                    }
                    other => {
                        return Err(ArgError::UnknownOption {
                            command: "journal",
                            option: other.to_string(),
                        })
                    }
                }
            }
            Ok(Command::Journal { config, json })
        }
        "help" | "--help" | "-h" => Ok(Command::Help),
        "version" | "--version" | "-V" => Ok(Command::Version),
        other => Err(ArgError::UnknownCommand(other.to_string())),
    }
}

fn parse_create(arguments: &[String]) -> Result<CreatePlan, ArgError> {
    let mut data: Vec<PathBuf> = Vec::new();
    let mut parity_p: Option<PathBuf> = None;
    let mut parity_q: Option<PathBuf> = None;
    let mut log: Option<PathBuf> = None;
    let mut block_size_log2 = DEFAULT_BLOCK_LOG2;
    let mut label = String::new();
    let mut confirmed = false;
    let mut force = false;

    let mut index = 0;
    while index < arguments.len() {
        let option = arguments[index].as_str();
        // `--yes` und `--force` stehen allein, alles andere braucht einen Wert.
        match option {
            "--yes" => {
                confirmed = true;
                index += 1;
                continue;
            }
            "--force" => {
                force = true;
                index += 1;
                continue;
            }
            _ => {}
        }

        let value = arguments
            .get(index + 1)
            .ok_or_else(|| ArgError::MissingValue(option.to_string()))?;
        match option {
            "--data" => data.push(PathBuf::from(value)),
            "--parity-p" => parity_p = Some(PathBuf::from(value)),
            "--parity-q" => parity_q = Some(PathBuf::from(value)),
            "--log" => log = Some(PathBuf::from(value)),
            "--label" => label = value.clone(),
            "--block-size" => block_size_log2 = parse_block_size(value)?,
            other => {
                return Err(ArgError::UnknownOption {
                    command: "create",
                    option: other.to_string(),
                })
            }
        }
        index += 2;
    }

    if data.is_empty() {
        return Err(ArgError::MissingOption {
            command: "create",
            option: "mindestens ein --data",
        });
    }
    let parity_p = parity_p.ok_or(ArgError::MissingOption {
        command: "create",
        option: "--parity-p",
    })?;
    // Das Log ist nicht optional, obwohl das Format ein Array ohne eines
    // kennt: Ohne Log gibt es kein Recovery nach Abschnitt 5.2, und ein Array,
    // das einen Stromausfall nicht ueberlebt, soll dieses Werkzeug nicht
    // anlegen koennen.
    let log = log.ok_or(ArgError::MissingOption {
        command: "create",
        option: "--log",
    })?;

    let plan = CreatePlan {
        data,
        parity_p,
        parity_q,
        log,
        block_size_log2,
        label,
        confirmed,
        force,
    };
    check_distinct(&plan.devices())?;
    Ok(plan)
}

/// `run` nimmt seine Geraete wie `status` als freie Argumente und dazu ein
/// paar Optionen. Welche Rolle jedes traegt, steht in seinem Superblock —
/// danach zu fragen waere eine zweite Wahrheit neben der auf der Platte.
fn parse_run(arguments: &[String]) -> Result<RunPlan, ArgError> {
    let mut devices: Vec<PathBuf> = Vec::new();
    let mut config: Option<PathBuf> = None;
    let mut pool: Option<PathBuf> = None;
    let mut state_dir: Option<PathBuf> = None;
    let mut fstype: Option<String> = None;

    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        if !argument.starts_with("--") {
            devices.push(PathBuf::from(argument));
            index += 1;
            continue;
        }

        let value = arguments
            .get(index + 1)
            .ok_or_else(|| ArgError::MissingValue(argument.to_string()))?;
        match argument {
            "--config" => config = Some(PathBuf::from(value)),
            "--pool" => pool = Some(PathBuf::from(value)),
            "--state-dir" => state_dir = Some(PathBuf::from(value)),
            "--fstype" => fstype = Some(value.clone()),
            other => {
                return Err(ArgError::UnknownOption {
                    command: "run",
                    option: other.to_string(),
                })
            }
        }
        index += 2;
    }

    // Keine Geraete ist **kein** Fehler: Dann kommen sie aus der
    // Konfiguration, und genau so startet die systemd-Unit.
    check_distinct(&devices)?;
    Ok(RunPlan {
        devices,
        config,
        pool,
        state_dir,
        fstype,
    })
}

fn parse_scrub(arguments: &[String]) -> Result<Command, ArgError> {
    let mut devices: Vec<PathBuf> = Vec::new();
    let mut config: Option<PathBuf> = None;
    let mut repair = false;

    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        match argument {
            "--repair" => {
                repair = true;
                index += 1;
            }
            "--config" => {
                let value = arguments
                    .get(index + 1)
                    .ok_or_else(|| ArgError::MissingValue(argument.to_string()))?;
                config = Some(PathBuf::from(value));
                index += 2;
            }
            other if other.starts_with("--") => {
                return Err(ArgError::UnknownOption {
                    command: "scrub",
                    option: other.to_string(),
                })
            }
            path => {
                devices.push(PathBuf::from(path));
                index += 1;
            }
        }
    }

    // Leer heisst suchen — der Timer ruft `ferrite scrub` ohne Argumente.
    check_distinct(&devices)?;
    Ok(Command::Scrub(ScrubRequest {
        devices,
        config,
        repair,
    }))
}

fn parse_replace(arguments: &[String]) -> Result<ReplacePlan, ArgError> {
    let mut devices: Vec<PathBuf> = Vec::new();
    let mut slot_index: Option<u16> = None;
    let mut replacement: Option<PathBuf> = None;
    let mut confirmed = false;
    let mut force = false;

    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        match argument {
            "--yes" => {
                confirmed = true;
                index += 1;
                continue;
            }
            "--force" => {
                force = true;
                index += 1;
                continue;
            }
            _ if !argument.starts_with("--") => {
                devices.push(PathBuf::from(argument));
                index += 1;
                continue;
            }
            _ => {}
        }

        let value = arguments
            .get(index + 1)
            .ok_or_else(|| ArgError::MissingValue(argument.to_string()))?;
        match argument {
            "--slot" => slot_index = Some(parse_slot(value)?),
            "--with" => replacement = Some(PathBuf::from(value)),
            other => {
                return Err(ArgError::UnknownOption {
                    command: "replace",
                    option: other.to_string(),
                })
            }
        }
        index += 2;
    }

    if devices.is_empty() {
        return Err(ArgError::NoDevices);
    }
    let slot_index = slot_index.ok_or(ArgError::MissingOption {
        command: "replace",
        option: "--slot",
    })?;
    let replacement = replacement.ok_or(ArgError::MissingOption {
        command: "replace",
        option: "--with",
    })?;

    // Die neue Platte darf nicht schon in der Liste stehen: Sonst wuerde sie
    // gleichzeitig als Ueberlebende gelesen und als Ersatz beschrieben.
    let mut all = devices.clone();
    all.push(replacement.clone());
    check_distinct(&all)?;

    Ok(ReplacePlan {
        devices,
        slot_index,
        replacement,
        confirmed,
        force,
    })
}

fn parse_rebuild(arguments: &[String]) -> Result<Command, ArgError> {
    let mut devices: Vec<PathBuf> = Vec::new();
    let mut config: Option<PathBuf> = None;
    let mut slot_index: Option<u16> = None;
    let mut batch = DEFAULT_BATCH;

    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        if !argument.starts_with("--") {
            devices.push(PathBuf::from(argument));
            index += 1;
            continue;
        }

        let value = arguments
            .get(index + 1)
            .ok_or_else(|| ArgError::MissingValue(argument.to_string()))?;
        match argument {
            "--slot" => slot_index = Some(parse_slot(value)?),
            "--config" => config = Some(PathBuf::from(value)),
            "--batch" => {
                batch = value
                    .parse::<u64>()
                    .ok()
                    .filter(|blocks| *blocks > 0)
                    .ok_or_else(|| ArgError::BadNumber {
                        option: "--batch",
                        value: value.clone(),
                    })?
            }
            other => {
                return Err(ArgError::UnknownOption {
                    command: "rebuild",
                    option: other.to_string(),
                })
            }
        }
        index += 2;
    }

    if devices.is_empty() {
        return Err(ArgError::NoDevices);
    }
    check_distinct(&devices)?;
    let slot_index = slot_index.ok_or(ArgError::MissingOption {
        command: "rebuild",
        option: "--slot",
    })?;
    Ok(Command::Rebuild(RebuildRequest {
        devices,
        config,
        slot_index,
        batch,
    }))
}

fn parse_slot(value: &str) -> Result<u16, ArgError> {
    value.parse::<u16>().map_err(|_| ArgError::BadNumber {
        option: "--slot",
        value: value.to_string(),
    })
}

/// `status` und `check` nehmen dieselben Argumente. Der Name kommt herein,
/// damit ein Bedienfehler bei `check` auch `check` sagt und nicht `status`.
fn parse_status(arguments: &[String], command: &'static str) -> Result<StatusRequest, ArgError> {
    // Auch hier heisst leer: suchen. Ein Ueberwachungsskript ruft `ferrite
    // status` ohne Argumente auf und will keine Geraeteliste pflegen.
    let mut devices: Vec<PathBuf> = Vec::new();
    let mut config: Option<PathBuf> = None;
    let mut json = false;

    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        if argument == "--config" {
            let value = arguments
                .get(index + 1)
                .ok_or_else(|| ArgError::MissingValue(argument.to_string()))?;
            config = Some(PathBuf::from(value));
            index += 2;
            continue;
        }
        if argument == "--json" {
            json = true;
            index += 1;
            continue;
        }
        if argument.starts_with("--") {
            return Err(ArgError::UnknownOption {
                command,
                option: argument.to_string(),
            });
        }
        devices.push(PathBuf::from(argument));
        index += 1;
    }
    Ok(StatusRequest {
        devices,
        config,
        json,
    })
}

/// Kein Geraet darf zweimal vorkommen.
///
/// Verglichen wird der Pfad, wie er dasteht — zwei Namen fuer dieselbe Platte
/// (ein Symlink unter `/dev/disk/by-id`) faende das nicht. Das ist die Grenze
/// einer reinen Pruefung; die zweite Haelfte macht `create`, indem es prueft,
/// ob auf dem Geraet schon ein Superblock liegt.
fn check_distinct(devices: &[PathBuf]) -> Result<(), ArgError> {
    for (position, device) in devices.iter().enumerate() {
        if devices[..position].contains(device) {
            return Err(ArgError::DuplicateDevice(device.clone()));
        }
    }
    Ok(())
}

/// `4K`, `64K`, `1M` — oder eine nackte Zahl in Bytes.
fn parse_block_size(value: &str) -> Result<u8, ArgError> {
    let bad = || ArgError::BadBlockSize(value.to_string());
    let (digits, factor) = match value.as_bytes().last() {
        Some(b'K' | b'k') => (&value[..value.len() - 1], 1024u64),
        Some(b'M' | b'm') => (&value[..value.len() - 1], 1024 * 1024),
        _ => (value, 1),
    };
    let bytes = digits
        .parse::<u64>()
        .ok()
        .and_then(|number| number.checked_mul(factor))
        .ok_or_else(bad)?;
    if !bytes.is_power_of_two() {
        return Err(bad());
    }
    let log2 = bytes.trailing_zeros() as u8;
    if !(MIN_PARITY_BLOCK_LOG2..=MAX_PARITY_BLOCK_LOG2).contains(&log2) {
        return Err(bad());
    }
    Ok(log2)
}

/// Der Hilfetext.
pub const HELP: &str = "\
ferrite — Werkzeug fuer ein Ferrite-Array

    ferrite create --data <GERAET> [--data <GERAET> ...]
                   --parity-p <GERAET> [--parity-q <GERAET>]
                   --log <GERAET>
                   [--block-size 64K] [--label <NAME>] [--force] [--yes]

        Legt ein Array an. **Zeigt standardmaessig nur den Plan** und
        schreibt nichts; erst --yes fuehrt ihn aus. Ein Geraet, auf dem
        bereits ein Ferrite-Superblock liegt, wird abgelehnt — --force
        ueberschreibt es.

        --parity-q verdoppelt die Redundanz: Damit ueberlebt das Array zwei
        gleichzeitig ausgefallene Datenplatten statt einer.

    ferrite status <GERAET> [<GERAET> ...] [--json]

        Liest die Superbloecke der angegebenen Geraete und zeigt, was sie
        zusammen ergeben. Beschreibt nichts.

        Rueckgabewert: 0 alles in Ordnung, 1 das Array laeuft degradiert,
        2 es laesst sich so nicht zusammensetzen.

        --json gibt dieselben Zahlen maschinenlesbar aus, Groessen in Bytes.
        Der Text daneben ist fuer Menschen gesetzt und darf sich aendern; wer
        ihn zerlegt, bricht beim ersten neuen Wort. Der Rueckgabewert ist in
        beiden Faellen derselbe.

    ferrite check [<GERAET> ...] [--config <DATEI>]

        Der taegliche Blick, fuer den Zeitplan gedacht und nicht fuer die
        Hand. Sieht nach wie `status` und **meldet sich, wenn sich etwas
        geaendert hat**: Eintrag ins Tagebuch, Aufruf des `notify`-Befehls
        aus der Konfiguration.

        Gemeldet wird nur eine Aenderung. Ein Array, das seit drei Wochen
        degradiert laeuft, schickt keine 21 Meldungen — es hat die eine
        geschickt, als es degradiert ist. Und die Entwarnung nach einem
        Rebuild zaehlt als Aenderung: Wer keine bekommt, sieht so lange
        nach, bis er aufhoert nachzusehen.

        Das Gedaechtnis steht im Tagebuch. Ohne `journal =` in der
        Konfiguration gibt es nichts zu vergleichen; dann wird der Befund
        gezeigt und nichts gemeldet.

        Der Timer `ferrite-check.timer` ruft das taeglich auf. Ohne ihn
        faellt eine ausgefallene Platte erst beim naechsten Scrub auf —
        also bis zu einen Monat spaeter.

        Rueckgabewert wie `status`.

    ferrite run <GERAET> [<GERAET> ...]
                [--pool <VERZEICHNIS>] [--state-dir /run/ferrite]
                [--fstype btrfs]

        Nimmt das Array in Betrieb und bleibt. Spielt zuerst das Log
        zurueck (Abschnitt 5.2) und stellt dann je Data-Slot ein
        Blockgeraet unter /dev/ublkbN bereit. Welche Rolle ein Geraet
        traegt, steht in seinem Superblock.

        Ohne --pool endet es dort: Was auf den Blockgeraeten liegt und wie
        es eingehaengt wird, entscheidet der Betreiber. Mit --pool haengt
        `ferrite` zusaetzlich jeden Data-Member unter --state-dir ein und
        darueber den vereinigten Pool. Ein Member ohne Dateisystem wird
        gemeldet, nicht formatiert.

        Beendet wird mit SIGINT oder SIGTERM. Das Aushaengen laeuft in
        umgekehrter Reihenfolge; solange ein Prozess noch im Pool steht,
        wartet es auf ihn.

    ferrite scrub <GERAET> [<GERAET> ...] [--repair]

        Prueft, ob die Paritaet zum Inhalt der Data-Members passt. Das ist
        die Probe, die ein Geraet auffliegen laesst, das seinen Flush
        belogen hat: Danach ist die Paritaet veraltet, und nur ein Scrub
        findet das, bevor es beim naechsten Ausfall auffaellt.

        Ohne --repair wird nur gemeldet. Mit --repair wird die Paritaet aus
        den Data-Members neu gebildet — die Daten gelten dabei als richtig.
        Laeuft das Array degradiert, wird die Reparatur abgelehnt: Eine
        Paritaet ueber einen unbrauchbaren Member zu bilden hiesse, die
        Rekonstruktion aufzugeben.

        Rueckgabewert: 0 stimmig, 1 Abweichungen gefunden, 2 nicht pruefbar.

    ferrite replace <GERAET> [<GERAET> ...] --slot <N> --with <GERAET>
                    [--force] [--yes]

        Nimmt eine neue Platte als Ersatz fuer den ausgefallenen Slot <N>
        auf. Angegeben werden die Geraete, die **noch da sind** — die
        ausgefallene fehlt in der Liste. Danach steht der Slot als
        unbrauchbar da und wartet auf `ferrite rebuild`.

        Wie `create` standardmaessig ein Trockenlauf; erst --yes schreibt.

    ferrite rebuild <GERAET> [<GERAET> ...] --slot <N> [--batch 64]

        Stellt den Inhalt von Slot <N> aus der Paritaet wieder her. Der
        Fortschritt steht im Superblock: Ein Abbruch kostet hoechstens
        einen Durchgang, danach geht es dort weiter, wo es aufgehoert hat.

    ferrite discover [<VERZEICHNIS> ...] [--json]

        Zeigt, welche Ferrite-Arrays angeschlossen sind. Ohne Angabe wird
        gesucht, wo die Konfiguration es sagt — voreingestellt in
        /dev/disk/by-id. Beschreibt nichts.

        Dieselbe Platte steht dort oft mehrfach (`ata-…` und `wwn-…`);
        entdoppelt wird ueber die Member-UUID aus dem Superblock.

    ferrite check-flush <GERAET> [<GERAET> ...]

        Fragt, ob das `FLUSH` eines Geraets ehrlich ist (Abschnitt 5.3).
        Davon haengt ab, ob Write-Back je erlaubt sein wird. Der Test kann
        nur `Refused`, `Undecidable` oder `Honest` sagen — und `Honest` nur
        auf einem echten Blockgeraet ohne fluechtigen Schreibcache auf
        einem nicht virtualisierten System.

        Schreibt nichts.

    ferrite journal [--config <DATEI>] [--json]

        Wertet das Betriebstagebuch aus: wieviele Stunden, wieviele Scrubs,
        wieviel Bit-Rot repariert — und die beiden Zahlen, um die es geht:
        wieviele Bereiche verloren und wieviele Reparaturen abgelehnt.

        Wohin geschrieben wird, sagt `journal =` in der Konfiguration. Ohne
        Angabe wird nichts aufgezeichnet, und dann ergibt ein Jahr Betrieb
        nichts, was sich vorzeigen liesse.

    ferrite help | version
";

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &[&str]) -> Vec<String> {
        line.iter().map(|part| (*part).to_string()).collect()
    }

    fn minimal() -> Vec<String> {
        args(&[
            "create",
            "--data",
            "/dev/a",
            "--parity-p",
            "/dev/p",
            "--log",
            "/dev/l",
        ])
    }

    fn plan_of(command: Command) -> CreatePlan {
        match command {
            Command::Create(plan) => *plan,
            other => panic!("erwartet war create, kam: {other:?}"),
        }
    }

    #[test]
    fn the_default_matches_the_format() {
        // Die Voreinstellung steht an zwei Stellen. Weicht sie ab, nennt die
        // Hilfe eine Zahl, die das Format nicht setzt.
        let superblock = ferrite_format::superblock::Superblock::new(
            ferrite_format::Uuid::from_random_bytes([0; 16]),
            ferrite_format::Uuid::from_random_bytes([1; 16]),
            ferrite_format::superblock::Role::Data,
            1,
            1 << 20,
        );
        assert_eq!(superblock.parity_block_size_log2, DEFAULT_BLOCK_LOG2);
    }

    #[test]
    fn a_minimal_create_is_understood() {
        let plan = plan_of(parse(&minimal()).unwrap());
        assert_eq!(plan.data, vec![PathBuf::from("/dev/a")]);
        assert_eq!(plan.parity_p, PathBuf::from("/dev/p"));
        assert_eq!(plan.parity_q, None);
        assert_eq!(plan.log, PathBuf::from("/dev/l"));
        assert_eq!(plan.block_size_log2, DEFAULT_BLOCK_LOG2);
    }

    #[test]
    fn create_writes_nothing_without_yes() {
        // Die wichtigste Voreinstellung des ganzen Werkzeugs: Ein Kommando,
        // das Platten beschreibt, tut es nicht, weil man es getippt hat.
        assert!(!plan_of(parse(&minimal()).unwrap()).confirmed);

        let mut line = minimal();
        line.push("--yes".to_string());
        assert!(plan_of(parse(&line).unwrap()).confirmed);
    }

    #[test]
    fn data_devices_add_up_in_order() {
        let line = args(&[
            "create",
            "--data",
            "/dev/a",
            "--data",
            "/dev/b",
            "--data",
            "/dev/c",
            "--parity-p",
            "/dev/p",
            "--log",
            "/dev/l",
        ]);
        let plan = plan_of(parse(&line).unwrap());
        assert_eq!(
            plan.data,
            vec![
                PathBuf::from("/dev/a"),
                PathBuf::from("/dev/b"),
                PathBuf::from("/dev/c")
            ],
            "die Reihenfolge ist der slot_index und darf sich nicht drehen"
        );
    }

    #[test]
    fn every_device_appears_in_the_plan_once() {
        let mut line = minimal();
        line.extend(args(&["--parity-q", "/dev/q"]));
        let plan = plan_of(parse(&line).unwrap());
        assert_eq!(
            plan.devices(),
            vec![
                PathBuf::from("/dev/a"),
                PathBuf::from("/dev/p"),
                PathBuf::from("/dev/q"),
                PathBuf::from("/dev/l")
            ]
        );
    }

    #[test]
    fn the_same_device_cannot_take_two_roles() {
        // Der teuerste Tippfehler dieses Werkzeugs: Wer P und Log auf dieselbe
        // Platte legt, hat ein Array, das beim ersten Ausfall beides verliert.
        let line = args(&[
            "create",
            "--data",
            "/dev/a",
            "--parity-p",
            "/dev/p",
            "--log",
            "/dev/p",
        ]);
        assert_eq!(
            parse(&line),
            Err(ArgError::DuplicateDevice(PathBuf::from("/dev/p")))
        );
    }

    #[test]
    fn the_same_data_device_twice_is_refused_too() {
        let line = args(&[
            "create",
            "--data",
            "/dev/a",
            "--data",
            "/dev/a",
            "--parity-p",
            "/dev/p",
            "--log",
            "/dev/l",
        ]);
        assert!(matches!(parse(&line), Err(ArgError::DuplicateDevice(_))));
    }

    #[test]
    fn a_create_without_parity_is_refused() {
        let line = args(&["create", "--data", "/dev/a", "--log", "/dev/l"]);
        assert_eq!(
            parse(&line),
            Err(ArgError::MissingOption {
                command: "create",
                option: "--parity-p"
            })
        );
    }

    #[test]
    fn a_create_without_a_log_is_refused() {
        // Ohne Log kein Recovery. Ein Array, das einen Stromausfall nicht
        // ueberlebt, soll dieses Werkzeug nicht anlegen koennen.
        let line = args(&["create", "--data", "/dev/a", "--parity-p", "/dev/p"]);
        assert_eq!(
            parse(&line),
            Err(ArgError::MissingOption {
                command: "create",
                option: "--log"
            })
        );
    }

    #[test]
    fn a_create_without_data_is_refused() {
        let line = args(&["create", "--parity-p", "/dev/p", "--log", "/dev/l"]);
        assert!(matches!(parse(&line), Err(ArgError::MissingOption { .. })));
    }

    #[test]
    fn an_option_without_its_value_is_refused() {
        // Sonst wanderte der naechste Schalter in den Pfad, und `--data
        // --parity-p` legte ein Geraet namens "--parity-p" an.
        let line = args(&["create", "--data"]);
        assert_eq!(
            parse(&line),
            Err(ArgError::MissingValue("--data".to_string()))
        );
    }

    #[test]
    fn block_sizes_are_read_with_their_unit() {
        assert_eq!(parse_block_size("4K"), Ok(12));
        assert_eq!(parse_block_size("64K"), Ok(16));
        assert_eq!(parse_block_size("1M"), Ok(20));
        assert_eq!(parse_block_size("16M"), Ok(24));
        assert_eq!(parse_block_size("65536"), Ok(16));
    }

    #[test]
    fn a_block_size_outside_the_format_is_refused() {
        // 2K ist zu klein, 32M zu gross — beides wuerde der Superblock beim
        // Schreiben ablehnen. Besser hier als nach der halben Platte.
        assert!(parse_block_size("2K").is_err());
        assert!(parse_block_size("32M").is_err());
    }

    #[test]
    fn a_block_size_that_is_not_a_power_of_two_is_refused() {
        assert!(parse_block_size("48K").is_err());
        assert!(parse_block_size("100000").is_err());
    }

    #[test]
    fn nonsense_as_a_block_size_is_refused() {
        assert!(parse_block_size("").is_err());
        assert!(parse_block_size("K").is_err());
        assert!(parse_block_size("-64K").is_err());
        assert!(parse_block_size("64G").is_err());
    }

    #[test]
    fn json_is_a_flag_on_every_command_that_reports() {
        // Eine Oberflaeche ruft genau diese drei auf. Faellt eines der
        // Flaggen weg, sieht sie einen Bedienfehler statt einer Antwort.
        for line in [
            vec!["status", "--json"],
            vec!["status", "--json", "/dev/a"],
            vec!["status", "/dev/a", "--json"],
            vec!["discover", "--json"],
            vec!["discover", "--json", "/dev/disk/by-id"],
            vec!["journal", "--json"],
            vec!["journal", "--json", "--config", "/etc/ferrite/ferrite.conf"],
        ] {
            let parsed = parse(&args(&line));
            assert!(parsed.is_ok(), "{line:?} wurde abgelehnt: {parsed:?}");
            let json = match parsed.expect("oben geprueft") {
                Command::Status(request) => request.json,
                Command::Discover(request) => request.json,
                Command::Journal { json, .. } => json,
                other => panic!("{line:?} ergab {other:?}"),
            };
            assert!(json, "{line:?} hat --json nicht gesetzt");
        }
    }

    #[test]
    fn without_the_flag_nothing_is_json() {
        for line in [vec!["status"], vec!["discover"], vec!["journal"]] {
            let json = match parse(&args(&line)).expect("gueltig") {
                Command::Status(request) => request.json,
                Command::Discover(request) => request.json,
                Command::Journal { json, .. } => json,
                other => panic!("{line:?} ergab {other:?}"),
            };
            assert!(!json, "{line:?} war ungefragt JSON");
        }
    }

    #[test]
    fn discover_still_rejects_an_option_it_does_not_know() {
        // Beim Umbau auf `--json` waere es leicht gewesen, jede Option
        // durchzulassen. Ein Tippfehler wuerde dann still ignoriert.
        assert_eq!(
            parse(&args(&["discover", "--jsonn"])),
            Err(ArgError::UnknownOption {
                command: "discover",
                option: "--jsonn".to_string(),
            })
        );
    }

    #[test]
    fn a_directory_named_after_a_flag_still_arrives() {
        // `--json` wird herausgefiltert, alles andere bleibt ein Verzeichnis.
        let parsed = parse(&args(&["discover", "/dev/disk/by-id", "--json"]));
        assert_eq!(
            parsed,
            Ok(Command::Discover(DiscoverRequest {
                scan: vec![PathBuf::from("/dev/disk/by-id")],
                json: true,
            }))
        );
    }

    #[test]
    fn status_takes_its_devices_as_plain_arguments() {
        let line = args(&["status", "/dev/a", "/dev/b"]);
        assert_eq!(
            parse(&line),
            Ok(Command::Status(StatusRequest {
                devices: vec![PathBuf::from("/dev/a"), PathBuf::from("/dev/b")],
                config: None,
                json: false,
            }))
        );
    }

    #[test]
    fn status_without_devices_means_search_instead_of_error() {
        // Ein Ueberwachungsskript ruft `ferrite status` ohne Argumente auf.
        // Wo die Platten stehen, sagt die Konfiguration.
        assert_eq!(
            parse(&args(&["status"])),
            Ok(Command::Status(StatusRequest {
                devices: Vec::new(),
                config: None,
                json: false,
            }))
        );
    }

    #[test]
    fn a_config_can_be_named_for_every_command_that_searches() {
        for command in ["status", "scrub", "run"] {
            let line = args(&[command, "--config", "/tmp/f.conf"]);
            assert!(parse(&line).is_ok(), "{command} nimmt --config nicht an");
        }
    }

    #[test]
    fn an_empty_line_asks_for_a_command() {
        assert_eq!(parse(&[]), Err(ArgError::NoCommand));
    }

    #[test]
    fn an_unknown_command_is_named_in_the_error() {
        assert_eq!(
            parse(&args(&["zerstoere"])),
            Err(ArgError::UnknownCommand("zerstoere".to_string()))
        );
    }

    #[test]
    fn an_unknown_option_is_named_in_the_error() {
        let line = args(&["create", "--daten", "/dev/a"]);
        assert_eq!(
            parse(&line),
            Err(ArgError::UnknownOption {
                command: "create",
                option: "--daten".to_string()
            })
        );
    }

    #[test]
    fn help_and_version_are_reachable_in_both_spellings() {
        for line in [["help"], ["--help"], ["-h"]] {
            assert_eq!(parse(&args(&line)), Ok(Command::Help));
        }
        for line in [["version"], ["--version"], ["-V"]] {
            assert_eq!(parse(&args(&line)), Ok(Command::Version));
        }
    }

    #[test]
    fn the_help_names_every_command_that_exists() {
        // Ein Werkzeug, dessen Hilfe ein Kommando verschweigt, hat es fuer
        // seine Nutzer nicht.
        for command in [
            "create",
            "status",
            "run",
            "scrub",
            "replace",
            "rebuild",
            "discover",
            "check-flush",
            "journal",
            "help",
            "version",
        ] {
            assert!(HELP.contains(command), "{command} fehlt in der Hilfe");
        }
    }

    fn run_plan(command: Command) -> RunPlan {
        match command {
            Command::Run(plan) => *plan,
            other => panic!("erwartet war run, kam: {other:?}"),
        }
    }

    #[test]
    fn run_takes_its_devices_as_plain_arguments() {
        let plan = run_plan(parse(&args(&["run", "/dev/a", "/dev/b"])).unwrap());
        assert_eq!(
            plan.devices,
            vec![PathBuf::from("/dev/a"), PathBuf::from("/dev/b")]
        );
        assert_eq!(plan.pool, None, "ohne --pool nur die Blockgeraete");
        assert_eq!(
            plan.state_dir, None,
            "die Voreinstellung kommt aus der Konfiguration"
        );
        assert_eq!(plan.fstype, None);
    }

    #[test]
    fn options_and_devices_may_be_mixed() {
        // Die Geraete kommen aus einer Shell-Expansion und stehen deshalb oft
        // mitten in der Zeile. Ein Parser, der sie nur am Ende erlaubt, ist im
        // Alltag laestig.
        let line = args(&[
            "run",
            "/dev/a",
            "--pool",
            "/mnt/pool",
            "/dev/b",
            "--fstype",
            "xfs",
            "/dev/c",
        ]);
        let plan = run_plan(parse(&line).unwrap());
        assert_eq!(
            plan.devices,
            vec![
                PathBuf::from("/dev/a"),
                PathBuf::from("/dev/b"),
                PathBuf::from("/dev/c")
            ]
        );
        assert_eq!(plan.pool, Some(PathBuf::from("/mnt/pool")));
        assert_eq!(plan.fstype, Some("xfs".to_string()));
    }

    #[test]
    fn run_without_devices_means_search_instead_of_error() {
        // Der Fall, fuer den die systemd-Unit gemacht ist: `ferrite run` ohne
        // ein einziges Argument. Welche Platten dazugehoeren, steht in ihren
        // Superbloecken; wo gesucht wird, in der Konfiguration.
        let plan = run_plan(parse(&args(&["run"])).unwrap());
        assert!(plan.devices.is_empty());
        assert_eq!(plan.config, None);

        let plan = run_plan(parse(&args(&["run", "--pool", "/mnt"])).unwrap());
        assert!(plan.devices.is_empty());
        assert_eq!(plan.pool, Some(PathBuf::from("/mnt")));
    }

    #[test]
    fn the_same_device_twice_is_refused_for_run_too() {
        let line = args(&["run", "/dev/a", "/dev/a"]);
        assert!(matches!(parse(&line), Err(ArgError::DuplicateDevice(_))));
    }

    #[test]
    fn an_option_of_run_without_its_value_is_refused() {
        assert_eq!(
            parse(&args(&["run", "/dev/a", "--pool"])),
            Err(ArgError::MissingValue("--pool".to_string()))
        );
    }
}
