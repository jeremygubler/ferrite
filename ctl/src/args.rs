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
    Help,
    Version,
}

/// Was `run` in Betrieb nehmen soll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPlan {
    pub devices: Vec<PathBuf>,
    /// Wo der vereinigte Pool eingehaengt wird. `None` heisst: nur die
    /// Blockgeraete bereitstellen, den Rest macht der Betreiber selbst.
    pub pool: Option<PathBuf>,
    /// Unter welchem Verzeichnis die einzelnen Members eingehaengt werden.
    pub state_dir: PathBuf,
    /// Welches Dateisystem auf den Members liegt.
    pub fstype: String,
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
        "status" => parse_status(rest),
        "run" => parse_run(rest).map(|plan| Command::Run(Box::new(plan))),
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
    let mut pool: Option<PathBuf> = None;
    let mut state_dir = PathBuf::from(DEFAULT_STATE_DIR);
    let mut fstype = "btrfs".to_string();

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
            "--pool" => pool = Some(PathBuf::from(value)),
            "--state-dir" => state_dir = PathBuf::from(value),
            "--fstype" => fstype.clone_from(value),
            other => {
                return Err(ArgError::UnknownOption {
                    command: "run",
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
    Ok(RunPlan {
        devices,
        pool,
        state_dir,
        fstype,
    })
}

fn parse_status(arguments: &[String]) -> Result<Command, ArgError> {
    let devices: Vec<PathBuf> = arguments.iter().map(PathBuf::from).collect();
    if devices.is_empty() {
        return Err(ArgError::NoDevices);
    }
    Ok(Command::Status(StatusRequest { devices }))
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

    ferrite status <GERAET> [<GERAET> ...]

        Liest die Superbloecke der angegebenen Geraete und zeigt, was sie
        zusammen ergeben. Beschreibt nichts.

        Rueckgabewert: 0 alles in Ordnung, 1 das Array laeuft degradiert,
        2 es laesst sich so nicht zusammensetzen.

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
    fn status_takes_its_devices_as_plain_arguments() {
        let line = args(&["status", "/dev/a", "/dev/b"]);
        assert_eq!(
            parse(&line),
            Ok(Command::Status(StatusRequest {
                devices: vec![PathBuf::from("/dev/a"), PathBuf::from("/dev/b")]
            }))
        );
    }

    #[test]
    fn status_without_devices_is_refused() {
        assert_eq!(parse(&args(&["status"])), Err(ArgError::NoDevices));
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
        for command in ["create", "status", "run", "help", "version"] {
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
        assert_eq!(plan.state_dir, PathBuf::from(DEFAULT_STATE_DIR));
        assert_eq!(plan.fstype, "btrfs");
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
        assert_eq!(plan.fstype, "xfs");
    }

    #[test]
    fn run_without_devices_is_refused() {
        assert_eq!(parse(&args(&["run"])), Err(ArgError::NoDevices));
        assert_eq!(
            parse(&args(&["run", "--pool", "/mnt"])),
            Err(ArgError::NoDevices)
        );
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
