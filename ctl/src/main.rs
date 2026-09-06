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
        Command::Status(request) => execute_status(&request.devices),
        Command::Run(plan) => execute_run(&plan),
        Command::Scrub(request) => execute_scrub(&request),
        Command::Replace(plan) => execute_replace(&plan),
        Command::Rebuild(request) => execute_rebuild(&request),
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
fn execute_status(devices: &[std::path::PathBuf]) -> ExitCode {
    let report = ferrite_ctl::run::status(devices);
    print!("{}", report.text);
    ExitCode::from(report.health.exit_code())
}

/// Ohne Blockgeraete gibt es nichts anzulegen und nichts anzusehen. Melden
/// statt so zu tun, als ginge es.
#[cfg(not(unix))]
fn execute_create(_plan: &ferrite_ctl::CreatePlan) -> ExitCode {
    eprintln!("ferrite create braucht ein System mit Blockgeraeten.");
    ExitCode::from(EXIT_USAGE)
}

#[cfg(not(unix))]
fn execute_status(_devices: &[std::path::PathBuf]) -> ExitCode {
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
