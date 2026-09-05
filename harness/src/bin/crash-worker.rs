// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Der Arbeiter des Crash-Harness.
//!
//! Laeuft als eigener Prozess, damit ihn ein `SIGKILL` mitten im Schreibpfad
//! treffen kann, ohne den Testlaeufer mitzunehmen. Aufruf:
//!
//! ```text
//! crash-worker create <verzeichnis>
//! crash-worker write  <verzeichnis> <anzahl>
//! crash-worker verify <verzeichnis>
//! ```
//!
//! `write` liest den Abbruchpunkt aus `FERRITE_CRASH_AT`. Ist er gesetzt und
//! erreicht, stirbt der Prozess dort — ohne Destruktoren, ohne Flush.
//!
//! Nach jedem zurueckgekehrten Write vermerkt der Arbeiter dessen Nummer in
//! `bestaetigt.txt` **ausserhalb** des Arrays und flusht die Datei. Diese Notiz
//! ist die Grundlage der dritten Zusage: Was dort steht, wurde bestaetigt und
//! muss danach noch im Array stehen.

// Das Harness braucht `SIGKILL` an den eigenen Prozess. Anderswo gibt es das
// nicht, und eine Nachbildung waere genau die Attrappe, die dieses Projekt
// nicht will: Sie testete etwas anderes als die Realitaet.
#![allow(unused_imports)]

use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;

use ferrite_harness::{plan, PlannedWrite};

/// Name der Notizdatei. Liegt neben dem Array, nicht darin — sie darf vom
/// Abbruchpunkt-Zaehler nicht beruehrt werden.
#[cfg(target_os = "linux")]
const CONFIRMED: &str = "bestaetigt.txt";

#[cfg(target_os = "linux")]
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Aufruf: crash-worker <create|write|verify> <verzeichnis> [anzahl]");
        return ExitCode::from(2);
    }
    let directory = Path::new(&args[2]);

    let outcome = match args[1].as_str() {
        "create" => create(directory),
        "write" => {
            let count = args
                .get(3)
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(12);
            write(directory, count)
        }
        "degrade" => {
            let slot = args
                .get(3)
                .and_then(|value| value.parse::<u16>().ok())
                .unwrap_or(0);
            degrade(directory, slot)
        }
        "verify" => verify(directory),
        other => {
            eprintln!("unbekannte Phase: {other}");
            return ExitCode::from(2);
        }
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "linux")]
fn create(directory: &Path) -> Result<(), String> {
    ferrite_harness::create(directory).map_err(|error| format!("anlegen: {error}"))?;
    std::fs::write(directory.join(CONFIRMED), b"")
        .map_err(|error| format!("Notizdatei anlegen: {error}"))
}

/// Meldet, was ein Recovery ergeben hat.
///
/// Ein Verlust wird ausgegeben und nicht nur gezaehlt: Wer nach einem Absturz
/// im degradierten Betrieb Daten einbuesst, soll erfahren welche.
#[cfg(target_os = "linux")]
fn report(recovered: &ferrite_engine::Recovered) {
    if recovered.applied > 0 {
        eprintln!("Recovery hat {} Writes angewendet", recovered.applied);
    }
    for lost in &recovered.lost {
        eprintln!(
            "  verloren: Slot {} bei Offset {} ueber {} Bytes",
            lost.slot_index, lost.offset, lost.len
        );
    }
}

/// Fuehrt den Ablauf aus und stirbt unterwegs, wenn der Abbruchpunkt trifft.
///
/// Gibt am Ende die Zahl der gezaehlten I/O-Operationen auf der Standardausgabe
/// aus. Der Laeufer braucht sie, um zu wissen, wieviele Abbruchpunkte es gibt.
#[cfg(target_os = "linux")]
fn write(directory: &Path, count: u64) -> Result<(), String> {
    let (mut writer, recovered) =
        ferrite_harness::open(directory).map_err(|error| format!("oeffnen: {error}"))?;
    report(&recovered);

    let mut notes = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(directory.join(CONFIRMED))
        .map_err(|error| format!("Notizdatei oeffnen: {error}"))?;

    // Erst hier scharfstellen: Das Anlegen und Oeffnen soll nicht mitgezaehlt
    // werden, sonst verschoeben sich alle Abbruchpunkte, sobald das Recovery
    // etwas zu tun hat.
    ferrite_engine::crash::arm_from_env();
    ferrite_engine::crash::mutation_from_env();

    for (
        nth,
        PlannedWrite {
            slot_index,
            offset,
            data,
        },
    ) in plan(count).into_iter().enumerate()
    {
        writer
            .write(slot_index, offset, &data)
            .map_err(|error| format!("Write {nth}: {error}"))?;

        // Der Write ist zurueck — also bestaetigt. Jetzt darf er in die Notiz.
        writeln!(notes, "{nth}").map_err(|error| format!("Notiz schreiben: {error}"))?;
        notes
            .sync_data()
            .map_err(|error| format!("Notiz flushen: {error}"))?;
    }

    println!("{}", ferrite_engine::crash::count());
    Ok(())
}

/// Meldet einen Data-Member als unbrauchbar.
///
/// Danach laeuft das Array degradiert: Reads auf diesen Slot kommen aus der
/// Paritaet, und der Schreibpfad schreibt die Paritaet fort statt sie neu zu
/// rechnen — der fehlende Member liesse sich nicht lesen.
#[cfg(target_os = "linux")]
fn degrade(directory: &Path, slot: u16) -> Result<(), String> {
    let (mut writer, _) =
        ferrite_harness::open(directory).map_err(|error| format!("oeffnen: {error}"))?;
    writer
        .mark_member(slot, ferrite_format::superblock::MemberState::Stale, 0)
        .map_err(|error| format!("Member melden: {error}"))
}

/// Prueft die drei Zusagen aus dem Modulkopf von `ferrite_harness`.
#[cfg(target_os = "linux")]
fn verify(directory: &Path) -> Result<(), String> {
    // Zusage 1: Das Array laesst sich oeffnen. `open` fuehrt dabei bereits das
    // Recovery aus — Schritt 5 aus Abschnitt 5.2.
    let (writer, recovered) =
        ferrite_harness::open(directory).map_err(|error| format!("Zusage 1 verletzt: {error}"))?;
    report(&recovered);

    // Zusage 2: Die Paritaet passt zum Inhalt der Data-Members, ueber die
    // gesamte Payload-Region.
    let payload = usize::try_from(ferrite_harness::PAYLOAD)
        .map_err(|_| "Payload passt nicht in den Adressraum".to_string())?;
    let matches = writer
        .verify_parity(0, payload)
        .map_err(|error| format!("Paritaet pruefen: {error}"))?;
    if !matches {
        return Err("Zusage 2 verletzt: die Paritaet passt nicht zu den Data-Members".to_string());
    }

    // Zusage 3: Jeder bestaetigte Write steht noch im Array.
    let notes = std::fs::read_to_string(directory.join(CONFIRMED))
        .map_err(|error| format!("Notizdatei lesen: {error}"))?;
    let confirmed: Vec<u64> = notes
        .lines()
        .filter_map(|line| line.trim().parse::<u64>().ok())
        .collect();

    // Der Ablauf ist deterministisch: Aus der Nummer folgen Slot, Offset und
    // Inhalt. Spaetere Writes duerfen frueheren ins Handwerk pfuschen, deshalb
    // wird von hinten nach vorn geprueft und jede Stelle nur einmal.
    let full_plan = plan(confirmed.iter().copied().max().map_or(0, |last| last + 1));
    let mut seen: Vec<(u16, u64, usize)> = Vec::new();
    let mut skipped = 0usize;

    for nth in confirmed.iter().rev() {
        let write = &full_plan[*nth as usize];
        let range = (write.slot_index, write.offset, write.data.len());
        if seen.iter().any(|earlier| overlaps(*earlier, range)) {
            continue;
        }
        seen.push(range);

        // Was das Recovery als verloren gemeldet hat, ist verloren — und zwar
        // unvermeidbar: Ein Absturz im degradierten Betrieb laesst die
        // Paritaet in unbekanntem Zustand, und sie war die einzige Quelle fuer
        // den fehlenden Member. Der Verlust wird hier **uebersprungen und
        // gezaehlt**, nicht stillschweigend hingenommen.
        if recovered
            .lost
            .iter()
            .any(|lost| overlaps((lost.slot_index, lost.offset, lost.len), range))
        {
            skipped += 1;
            continue;
        }

        let mut read_back = vec![0u8; write.data.len()];
        writer
            .read(write.slot_index, write.offset, &mut read_back)
            .map_err(|error| format!("Write {nth} zurueckgelesen: {error}"))?;
        if read_back != write.data {
            return Err(format!(
                "Zusage 3 verletzt: bestaetigter Write {nth} (Slot {}, Offset {}) fehlt",
                write.slot_index, write.offset
            ));
        }
    }

    if skipped > 0 {
        println!(
            "ok {} bestaetigte Writes geprueft, {skipped} durch gemeldeten Verlust uebersprungen",
            confirmed.len()
        );
    } else {
        println!("ok {} bestaetigte Writes geprueft", confirmed.len());
    }
    Ok(())
}

/// Ueberschneiden sich zwei Schreibbereiche desselben Slots?
#[cfg(target_os = "linux")]
fn overlaps(a: (u16, u64, usize), b: (u16, u64, usize)) -> bool {
    a.0 == b.0 && a.1 < b.1 + b.2 as u64 && b.1 < a.1 + a.2 as u64
}

/// Auf allen anderen Plattformen gibt es das Binary, aber es tut nichts ausser
/// zu sagen, was fehlt. Melden statt mocken.
#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "Das Crash-Harness braucht Linux: der Abbruch ist ein SIGKILL an den eigenen Prozess."
    );
    std::process::ExitCode::from(2)
}
