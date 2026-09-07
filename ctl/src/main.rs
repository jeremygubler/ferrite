// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! `ferrite` — das Werkzeug.
//!
//! Hier steht nur, wie ein Ergebnis zu einem Rueckgabewert und einer Zeile
//! Ausgabe wird. Alles andere liegt in der Bibliothek und ist dort geprueft.

use std::process::ExitCode;

use ferrite_ctl::args::{self, Command};

/// Fehler in der Bedienung — falsche Option, fehlendes Argument.
const EXIT_USAGE: u8 = 64;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();

    let command = match args::parse(&arguments) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("{error}\n\n{}", args::HELP);
            return ExitCode::from(EXIT_USAGE);
        }
    };

    match command {
        Command::Help => {
            print!("{}", args::HELP);
            ExitCode::SUCCESS
        }
        Command::Version => {
            println!("ferrite {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Command::Create(plan) => execute_create(&plan),
        Command::Status(request) => execute_status(&request),
        Command::Check(request) => execute_check(&request),
        Command::Run(plan) => execute_run(&plan),
        Command::Scrub(request) => execute_scrub(&request),
        Command::Replace(plan) => execute_replace(&plan),
        Command::Rebuild(request) => execute_rebuild(&request),
        Command::CheckFlush(request) => execute_check_flush(&request.devices),
        Command::Discover(request) => execute_discover(&request),
        Command::Journal { config, json } => execute_journal(config.as_deref(), json),
    }
}

#[cfg(unix)]
fn execute_create(plan: &ferrite_ctl::CreatePlan) -> ExitCode {
    match ferrite_ctl::run::create(plan) {
        Ok((text, written)) => {
            print!("{text}");
            // Der Trockenlauf ist kein Fehler: Er hat getan, was er sollte.
            // Ein Rueckgabewert ungleich null liesse ein Skript abbrechen, das
            // erst den Plan zeigen und dann fragen will.
            let _ = written;
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(unix)]
fn execute_status(request: &ferrite_ctl::args::StatusRequest) -> ExitCode {
    // Der Rueckgabewert haengt nicht an der Darstellung: Ein Ueberwachungs-
    // skript, das auf `--json` umstellt, soll nicht ploetzlich andere Werte
    // sehen.
    if request.json {
        let (text, health) =
            ferrite_ctl::run::status_json(&request.devices, request.config.as_deref());
        println!("{text}");
        return ExitCode::from(health.exit_code());
    }
    let report = ferrite_ctl::run::status(&request.devices, request.config.as_deref());
    print!("{}", report.text);
    ExitCode::from(report.health.exit_code())
}

#[cfg(unix)]
fn execute_check(request: &ferrite_ctl::args::StatusRequest) -> ExitCode {
    let check = ferrite_ctl::run::check(&request.devices, request.config.as_deref());

    // Der Befund selbst zuerst und immer: `systemctl status ferrite-check`
    // soll zeigen, was los ist, und nicht nur, dass etwas los war.
    print!("{}", check.report.text);
    match (&check.trouble, check.reported) {
        (Some(grund), _) => eprintln!("Nicht aufgezeichnet: {grund}"),
        (None, true) => println!("Zustandswechsel aufgezeichnet und gemeldet."),
        (None, false) => println!("Unveraendert — nichts zu melden."),
    }
    ExitCode::from(check.report.health.exit_code())
}

#[cfg(not(unix))]
fn execute_check(_request: &ferrite_ctl::args::StatusRequest) -> ExitCode {
    eprintln!("ferrite check braucht ein System mit Blockgeraeten.");
    ExitCode::from(EXIT_USAGE)
}

/// Ohne Blockgeraete gibt es nichts anzulegen und nichts anzusehen. Melden
/// statt so zu tun, als ginge es.
#[cfg(not(unix))]
fn execute_create(_plan: &ferrite_ctl::CreatePlan) -> ExitCode {
    eprintln!("ferrite create braucht ein System mit Blockgeraeten.");
    ExitCode::from(EXIT_USAGE)
}

#[cfg(not(unix))]
fn execute_status(_request: &ferrite_ctl::args::StatusRequest) -> ExitCode {
    eprintln!("ferrite status braucht ein System mit Blockgeraeten.");
    ExitCode::from(EXIT_USAGE)
}

/// `run` gibt es nur, wo es ublk und FUSE gibt.
#[cfg(target_os = "linux")]
fn execute_run(plan: &ferrite_ctl::RunPlan) -> ExitCode {
    match ferrite_ctl::serve::run(plan) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn execute_run(_plan: &ferrite_ctl::RunPlan) -> ExitCode {
    eprintln!("ferrite run braucht Linux mit ublk_drv und /dev/fuse.");
    ExitCode::from(EXIT_USAGE)
}

#[cfg(unix)]
fn execute_scrub(request: &ferrite_ctl::args::ScrubRequest) -> ExitCode {
    match ferrite_ctl::repair::scrub(request) {
        // Ein Befund ist kein Programmfehler, aber auch kein Erfolg: Ein
        // Monitoring, das hier `0` bekaeme, meldete eine veraltete Paritaet
        // nie.
        Ok(outcome) if outcome.is_clean() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::from(1),
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(2)
        }
    }
}

#[cfg(unix)]
fn execute_replace(plan: &ferrite_ctl::args::ReplacePlan) -> ExitCode {
    match ferrite_ctl::repair::replace(plan) {
        Ok((text, _)) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(unix)]
fn execute_rebuild(request: &ferrite_ctl::args::RebuildRequest) -> ExitCode {
    match ferrite_ctl::repair::rebuild(request) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(unix))]
fn execute_scrub(_request: &ferrite_ctl::args::ScrubRequest) -> ExitCode {
    eprintln!("ferrite scrub braucht ein System mit Blockgeraeten.");
    ExitCode::from(EXIT_USAGE)
}

#[cfg(not(unix))]
fn execute_replace(_plan: &ferrite_ctl::args::ReplacePlan) -> ExitCode {
    eprintln!("ferrite replace braucht ein System mit Blockgeraeten.");
    ExitCode::from(EXIT_USAGE)
}

#[cfg(not(unix))]
fn execute_rebuild(_request: &ferrite_ctl::args::RebuildRequest) -> ExitCode {
    eprintln!("ferrite rebuild braucht ein System mit Blockgeraeten.");
    ExitCode::from(EXIT_USAGE)
}

#[cfg(unix)]
fn execute_check_flush(devices: &[std::path::PathBuf]) -> ExitCode {
    match ferrite_ctl::repair::check_flush(devices) {
        // Der haeufige Ausgang ist `Undecidable`, und der ist kein Fehler.
        // Der Rueckgabewert sagt trotzdem, ob Write-Back in Frage kaeme —
        // sonst muesste ein Skript den Text lesen.
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(2)
        }
    }
}

#[cfg(unix)]
fn execute_discover(request: &ferrite_ctl::args::DiscoverRequest) -> ExitCode {
    let directories = if request.scan.is_empty() {
        ferrite_ctl::config::Config::default().scan
    } else {
        request.scan.clone()
    };

    let scan = ferrite_ctl::discover::scan(&directories);

    if request.json {
        println!("{}", ferrite_ctl::discover::scan_json(&scan, &directories));
        // Auch hier bleibt der Rueckgabewert derselbe: nichts gefunden ist 1.
        return if scan.arrays.is_empty() {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        };
    }

    if scan.arrays.is_empty() {
        println!(
            "Kein Ferrite-Array gefunden. Gesucht wurde in:\n  {}",
            directories
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join("\n  ")
        );
        return ExitCode::from(1);
    }

    for (uuid, members) in &scan.arrays {
        println!("Array {uuid}");
        for member in members {
            println!(
                "  {:<9} {}",
                match member.superblock.role {
                    ferrite_format::superblock::Role::Data =>
                        format!("Slot {}", member.superblock.slot_index),
                    ferrite_format::superblock::Role::ParityP => "ParityP".to_string(),
                    ferrite_format::superblock::Role::ParityQ => "ParityQ".to_string(),
                    ferrite_format::superblock::Role::Log => "Log".to_string(),
                },
                member.path.display()
            );
        }
        println!();
    }
    if scan.duplicates > 0 {
        println!(
            "{} von {} Pfaden waren zweite Namen derselben Platte.",
            scan.duplicates, scan.looked_at
        );
    }
    ExitCode::SUCCESS
}

#[cfg(not(unix))]
fn execute_check_flush(_devices: &[std::path::PathBuf]) -> ExitCode {
    eprintln!("ferrite check-flush braucht ein System mit Blockgeraeten.");
    ExitCode::from(EXIT_USAGE)
}

#[cfg(not(unix))]
fn execute_discover(_request: &ferrite_ctl::args::DiscoverRequest) -> ExitCode {
    eprintln!("ferrite discover braucht ein System mit Blockgeraeten.");
    ExitCode::from(EXIT_USAGE)
}

#[cfg(unix)]
fn execute_journal(config: Option<&std::path::Path>, json: bool) -> ExitCode {
    let config = match ferrite_ctl::run::load_config(config) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    let Some(path) = &config.journal else {
        eprintln!(
            "Es wird kein Tagebuch gefuehrt. `journal = /var/lib/ferrite/journal`\n\
             in die Konfiguration eintragen — ohne Aufzeichnung ergibt ein Jahr\n\
             Betrieb nichts, was sich vorzeigen liesse."
        );
        return ExitCode::from(2);
    };

    let text = std::fs::read_to_string(path).unwrap_or_default();
    let summary = ferrite_ctl::journal::summarize(&text);
    if json {
        println!("{}", ferrite_ctl::journal::summary_json(&summary));
    } else {
        print!("{}", ferrite_ctl::journal::render(&summary));
    }

    // Wie bei `status`: Die Zahl sagt, ob jemand hinsehen muss.
    if summary.needs_attention() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(not(unix))]
fn execute_journal(_config: Option<&std::path::Path>, _json: bool) -> ExitCode {
    eprintln!("ferrite journal braucht ein System mit Blockgeraeten.");
    ExitCode::from(EXIT_USAGE)
}
