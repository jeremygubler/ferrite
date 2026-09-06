// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Scrub, Ersatz und Rebuild — die drei Aufrufe, mit denen ein Betreiber ein
//! Array wieder in Ordnung bringt.
//!
//! # Warum die drei zusammengehoeren
//!
//! Sie sind ein Ablauf und nicht drei Funktionen. Eine Platte faellt aus,
//! `replace` nimmt die neue auf, `rebuild` fuellt sie, `scrub` bestaetigt, dass
//! danach alles zusammenpasst. Wer nur `rebuild` baut, hat ein Kommando, das
//! niemand erreichen kann: Ohne `replace` gibt es keinen Slot, der auf einen
//! Rebuild wartet.
//!
//! # Was ein Scrub findet
//!
//! Eine Paritaet, die nicht mehr zu den Daten passt. Der haeufigste Grund ist
//! ein Geraet, das einen Flush bestaetigt hat, ohne zu schreiben (Abschnitt
//! 5.3): Danach steht auf dem Data-Member der neue Inhalt und in der Paritaet
//! der alte. Auffallen wuerde das sonst erst beim naechsten Plattenausfall —
//! und dann rekonstruiert es Muell.
//!
//! # Wem der Scrub glaubt
//!
//! Den Daten. `--repair` bildet die Paritaet aus den Data-Members neu, statt
//! umgekehrt. Das ist keine Willkuer: Die Pruefsummen liegen bei btrfs auf den
//! Data-Members (Regel 7), und was dort falsch ist, findet der Repair-Broker.
//! Die Paritaet hat niemanden, der sie prueft — sie ist die abgeleitete
//! Groesse und wird deshalb neu abgeleitet.

use std::path::PathBuf;

use ferrite_engine::{ArrayWriter, EngineError};
use ferrite_format::superblock::MemberState;

use crate::args::{RebuildRequest, ReplacePlan, ScrubRequest};
use crate::report::size;
use crate::run::CtlError;

type Result<T> = std::result::Result<T, CtlError>;

/// Wieviele Parity-Bloecke ein Durchgang des Scrubs auf einmal prueft.
///
/// Gross genug, dass die Systemaufrufe nicht dominieren, klein genug, dass die
/// Puffer je Member in den Cache passen. Bei 64-KiB-Bloecken sind das 4 MiB je
/// Member — bei acht Members also 32 MiB im Zugriff.
const CHUNK_BLOCKS: u64 = 64;

// --- Scrub ----------------------------------------------------------------

/// Was ein Scrub ergeben hat.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScrubOutcome {
    pub blocks_checked: u64,
    /// Bloecke, deren Paritaet nicht zum Inhalt passte.
    pub mismatched: Vec<u64>,
    pub repaired: u64,
}

impl ScrubOutcome {
    pub fn is_clean(&self) -> bool {
        self.mismatched.is_empty()
    }
}

/// Prueft die Paritaet ueber die gesamte Payload.
///
/// Geprueft wird in Stuecken, gemeldet aber blockgenau: Faellt ein Stueck
/// durch, wird es Block fuer Block nachgeprueft. Der Betreiber soll wissen,
/// **wo** es klemmt, und nicht nur, dass irgendwo etwas nicht stimmt.
pub fn scrub(request: &ScrubRequest) -> Result<ScrubOutcome> {
    let config = crate::run::load_config(request.config.as_deref())?;
    let devices = crate::run::devices_or_search(&request.devices, &config)?;
    let mut writer = crate::run::open_array(&devices)?;
    let block = 1u64 << writer.block_size_log2();
    let blocks = total_blocks(&writer)?;

    println!("Scrub ueber {} Bloecke zu je {}.", blocks, size(block));

    let mut outcome = ScrubOutcome::default();
    let mut first = 0u64;
    while first < blocks {
        let count = CHUNK_BLOCKS.min(blocks - first);
        let offset = first * block;
        let len = usize::try_from(count * block).map_err(|_| CtlError::Missing {
            what: "der Scrub-Puffer passt nicht in den Adressraum",
        })?;

        match writer.verify_parity(offset, len) {
            Ok(true) => {}
            // Erst jetzt blockgenau nachsehen — das kostet, aber nur dort, wo
            // ohnehin schon etwas nicht stimmt.
            Ok(false) => narrow_down(&mut writer, first, count, block, &mut outcome)?,
            Err(error) => return Err(CtlError::Engine(error)),
        }
        outcome.blocks_checked += count;
        first += count;
    }

    if outcome.is_clean() {
        println!("Die Paritaet passt zu allen Data-Members.");
        return Ok(outcome);
    }

    println!(
        "{} von {} Bloecken passen nicht:",
        outcome.mismatched.len(),
        outcome.blocks_checked
    );
    for chunk in outcome.mismatched.chunks(16) {
        let list: Vec<String> = chunk.iter().map(u64::to_string).collect();
        println!("  {}", list.join(" "));
    }

    if !request.repair {
        println!(
            "\nEs wurde nichts geaendert. Zum Neubilden der Paritaet dasselbe Kommando\n\
             mit --repair. Die Daten gelten dabei als richtig."
        );
        return Ok(outcome);
    }

    for block_index in outcome.mismatched.clone() {
        // `rebuild_parity` lehnt ab, wenn ein Data-Member unbrauchbar ist.
        // Genau richtig: Eine Paritaet ueber einen unbrauchbaren Member
        // machte die Rekonstruktion unmoeglich.
        writer
            .rebuild_parity(block_index * block, block as usize)
            .map_err(CtlError::Engine)?;
        outcome.repaired += 1;
    }
    println!("{} Bloecke neu gebildet.", outcome.repaired);
    Ok(outcome)
}

/// Sucht innerhalb eines durchgefallenen Stuecks die einzelnen Bloecke.
fn narrow_down(
    writer: &mut ArrayWriter,
    first: u64,
    count: u64,
    block: u64,
    outcome: &mut ScrubOutcome,
) -> Result<()> {
    for index in first..first + count {
        let matches = writer
            .verify_parity(index * block, block as usize)
            .map_err(CtlError::Engine)?;
        if !matches {
            outcome.mismatched.push(index);
        }
    }
    Ok(())
}

/// Wieviele Parity-Bloecke das Array hat.
///
/// Der laengste Data-Member gibt sie vor — nicht der kuerzeste: Jenseits des
/// Endes eines kurzen Members liest er Nullbytes, und die gehoeren zur
/// Paritaet dazu.
fn total_blocks(writer: &ArrayWriter) -> Result<u64> {
    let block = 1u64 << writer.block_size_log2();
    let mut longest = 0u64;
    for slot in 0..u16::from(writer.data_slot_count()) {
        let payload = writer
            .member(slot)
            .map_err(CtlError::Engine)?
            .payload_size();
        longest = longest.max(payload);
    }
    Ok(longest / block)
}

// --- Rebuild --------------------------------------------------------------

/// Stellt einen Member aus der Paritaet wieder her.
pub fn rebuild(request: &RebuildRequest) -> Result<()> {
    let config = crate::run::load_config(request.config.as_deref())?;
    let devices = crate::run::devices_or_search(&request.devices, &config)?;
    let mut writer = crate::run::open_array(&devices)?;
    let state = writer
        .member(request.slot_index)
        .map_err(CtlError::Engine)?
        .superblock()
        .member_state;

    if state == MemberState::Clean {
        println!(
            "Slot {} traegt gueltige Daten — es gibt nichts wiederherzustellen.",
            request.slot_index
        );
        return Ok(());
    }

    let mut disk_rebuild = ferrite_engine::DiskRebuild::resume(&writer, request.slot_index)
        .map_err(CtlError::Engine)?;
    let total = disk_rebuild.remaining_blocks();
    println!("Rebuild von Slot {}: {total} Bloecke.", request.slot_index);

    let mut done = 0u64;
    let mut next_report = 0u64;
    while !disk_rebuild.is_complete() {
        let before = disk_rebuild.remaining_blocks();
        disk_rebuild
            .step(&mut writer, request.batch)
            .map_err(CtlError::Engine)?;
        done += before - disk_rebuild.remaining_blocks();

        // Nicht bei jedem Durchgang eine Zeile: Bei einer grossen Platte
        // waeren das Zehntausende, und der Fortschritt ginge darin unter.
        let percent = done.saturating_mul(100).checked_div(total).unwrap_or(100);
        if percent >= next_report {
            println!("  {percent} % ({done}/{total})");
            next_report = percent + 10;
        }
    }

    println!("Slot {} ist wiederhergestellt.", request.slot_index);
    Ok(())
}

// --- Replace --------------------------------------------------------------

/// Nimmt eine neue Platte als Ersatz fuer einen ausgefallenen Slot auf.
///
/// Danach traegt sie einen Superblock dieses Arrays mit `member_state =
/// Stale`: Sie gehoert dazu, hat aber noch keine gueltigen Daten. Ein Read auf
/// sie wird ab sofort rekonstruiert, und `rebuild` fuellt sie.
///
/// # Warum die Payload gedeckelt wird
///
/// Ein Data-Member darf nicht laenger sein als der Parity-Member (Regel 6 aus
/// Abschnitt 2.1): Fuer den Teil jenseits von P gaebe es keine Redundanz. Eine
/// groessere Ersatzplatte ist erlaubt, ihr Ueberhang bleibt nur ungenutzt —
/// abgelehnt wird sie nicht, denn eine passende Platte zu finden ist Jahre
/// spaeter schwer genug.
pub fn replace(plan: &ReplacePlan) -> Result<(String, bool)> {
    use ferrite_engine::{max_payload_size, read_superblock, write_superblock, MemberDevice};
    use ferrite_format::superblock::{Role, Superblock};

    // Die ueberlebenden Members lesen — daraus kommt alles, was der neue
    // Superblock ueber das Array wissen muss.
    let mut survivors = Vec::with_capacity(plan.devices.len());
    for path in &plan.devices {
        let device = MemberDevice::open_read_only(path).map_err(at(path))?;
        survivors.push(read_superblock(&device).map_err(at(path))?);
    }
    let reference = survivors.first().ok_or(CtlError::Missing {
        what: "keine ueberlebenden Members angegeben",
    })?;

    if plan.slot_index >= reference.data_slot_count as u16 {
        return Err(CtlError::Missing {
            what: "diesen Slot gibt es in dem Array nicht",
        });
    }
    if survivors
        .iter()
        .any(|member| member.role == Role::Data && member.slot_index == plan.slot_index)
    {
        return Err(CtlError::Missing {
            what: "dieser Slot ist noch besetzt — die alte Platte steht in der Liste",
        });
    }

    let parity = survivors
        .iter()
        .find(|member| member.role == Role::ParityP)
        .ok_or(CtlError::Missing {
            what: "ohne ParityP laesst sich nichts wiederherstellen",
        })?;

    let device = MemberDevice::open(&plan.replacement).map_err(at(&plan.replacement))?;
    if !plan.force {
        if let Ok(existing) = read_superblock(&device) {
            return Err(CtlError::AlreadyMember {
                path: plan.replacement.clone(),
                array: existing.array_uuid.to_string(),
            });
        }
    }

    let room = max_payload_size(device.size(), reference.parity_block_size_log2)
        .map_err(at(&plan.replacement))?;
    let payload_size = room.min(parity.payload_size);

    let mut text = format!(
        "Slot {} bekommt {}.\n  {} Geraet, davon {} nutzbar{}\n",
        plan.slot_index,
        plan.replacement.display(),
        size(device.size()),
        size(payload_size),
        if room > parity.payload_size {
            " — der Rest bleibt ungenutzt, ParityP ist kuerzer"
        } else {
            ""
        }
    );
    text.push_str("\nDanach gilt der Slot als unbrauchbar und wartet auf `ferrite rebuild`.\n");

    if !plan.confirmed {
        text.push_str(
            "\nEs wurde nichts geschrieben. Zum Ausfuehren dasselbe Kommando mit --yes.\n",
        );
        return Ok((text, false));
    }

    let mut superblock = Superblock::new(
        reference.array_uuid,
        crate::run::random_uuid()?,
        Role::Data,
        reference.data_slot_count,
        payload_size,
    );
    superblock.parity_block_size_log2 = reference.parity_block_size_log2;
    superblock.slot_index = plan.slot_index;
    superblock.created_unix = reference.created_unix;
    // **Stale und nicht Clean.** Auf der Platte steht nichts, was zu diesem
    // Array gehoert; wer sie fuer gueltig erklaerte, liesse jeden Read dort
    // Muell liefern statt ihn zu rekonstruieren.
    superblock.member_state = MemberState::Stale;
    superblock.rebuild_progress = 0;

    // Erst pruefen, ob das Ergebnis ein Array ergibt, dann schreiben.
    let mut together = survivors.clone();
    together.push(superblock.clone());
    ferrite_format::assemble(&together)
        .map_err(EngineError::Format)
        .map_err(CtlError::Engine)?;

    write_superblock(&device, &superblock).map_err(at(&plan.replacement))?;
    text.push_str("\nAufgenommen. Jetzt `ferrite rebuild --slot ...` aufrufen.\n");
    Ok((text, true))
}

fn at(path: &std::path::Path) -> impl FnOnce(EngineError) -> CtlError + '_ {
    move |source| CtlError::Device {
        path: path.to_path_buf(),
        source,
    }
}

// --- Flush-Test ------------------------------------------------------------

/// Fragt ein Geraet, ob sein `FLUSH` ehrlich ist (Abschnitt 5.3).
///
/// # Warum es dieses Kommando gibt
///
/// Write-Back ist gesperrt, bis der Test `Honest` sagt, und er hat das auf
/// keiner Maschine getan, die dieses Projekt bisher gesehen hat — alle waren
/// virtualisiert. Auf blankem Blech kann er es sagen. Die Frage laesst sich
/// nur dort beantworten, wo die Platte steht, und deshalb gehoert sie in ein
/// Kommando statt in einen Test.
///
/// # Was der Test nicht kann
///
/// Beweisen, dass ein Geraet ehrlich ist. Er sammelt, was die Plattform
/// hergibt, und nur eine einzige Kombination fuehrt zu `Honest`: echtes
/// Blockgeraet, kein fluechtiger Schreibcache, nicht virtualisiert. Alles
/// andere heisst „nicht entscheidbar", und das ist nach Abschnitt 5.3
/// dasselbe wie „nein".
///
/// Geschrieben wird **nichts**. Die Schreibprobe zerstoert den Bereich, auf
/// den sie zeigt; sie gehoert ins Anlegen eines Arrays und nicht in eine
/// Auskunft.
pub fn check_flush(devices: &[PathBuf]) -> Result<bool> {
    use ferrite_engine::{FlushVerdict, MemberDevice, WriteMode};

    let mut all_honest = true;
    for path in devices {
        let device = MemberDevice::open_read_only(path).map_err(at(path))?;
        let check = ferrite_engine::check_flush(&device, None);

        println!("{}", path.display());
        println!("  Geraet:        {:?}", check.facts.kind);
        println!(
            "  Schreibcache:  {}",
            match check.facts.write_cache {
                Some(cache) => format!("{cache:?}"),
                None => "der Kernel sagt nichts dazu".to_string(),
            }
        );
        println!(
            "  Virtualisiert: {}",
            match check.facts.virtualized {
                Some(true) => "ja — jede Angabe kann die des Hypervisors sein",
                Some(false) => "nein",
                None => "nicht feststellbar",
            }
        );
        println!(
            "  Flush:         {}",
            if check.facts.flush_succeeded {
                "ohne Fehler"
            } else {
                "fehlgeschlagen"
            }
        );
        println!("  Urteil:        {:?} — {}", check.verdict, check.reason);
        println!(
            "  Betriebsart:   {}\n",
            match check.write_mode() {
                WriteMode::WriteBack => "Write-Back waere erlaubt",
                WriteMode::WriteThrough => "Write-Through (Abschnitt 5.3)",
            }
        );

        if check.verdict != FlushVerdict::Honest {
            all_honest = false;
        }
    }

    if all_honest {
        println!(
            "Alle angegebenen Geraete kaemen als Log-Member fuer Write-Back in Frage.\n\
             Der Modus selbst ist noch nicht gebaut — aber die Voraussetzung dafuer\n\
             stand bisher auf keiner Maschine, die dieses Projekt gesehen hat."
        );
    } else {
        println!(
            "Write-Through. Das ist der Normalfall und kein Mangel: Abschnitt 5.3\n\
             stellt „faellt negativ aus\" und „ist nicht durchfuehrbar\" gleich."
        );
    }
    Ok(all_honest)
}
