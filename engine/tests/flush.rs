// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Flush-Test, `docs/FORMAT.md` Abschnitt 5.3.
//!
//! Der wichtigste Test hier ist der letzte: Aus allen Kombinationen von Fakten
//! darf genau eine zu „ehrlich" fuehren. Ein Flush-Test, der im Zweifel
//! „ehrlich" sagt, erzeugt Vertrauen, das nicht gedeckt ist.

use std::fs::File;
use std::path::PathBuf;

use ferrite_engine::{
    check_flush, judge, probe_write_path, DeviceFacts, DeviceKind, FlushVerdict, MemberDevice,
    WriteCache, WriteMode,
};

/// Fakten, die fuer sich genommen nichts beweisen: alles unbekannt, Flush
/// fehlerfrei.
fn unknown() -> DeviceFacts {
    DeviceFacts {
        kind: DeviceKind::Unknown,
        write_cache: None,
        virtualized: None,
        flush_succeeded: true,
        write_read_back: None,
    }
}

/// Der einzige Satz Fakten, der zu `Honest` fuehren darf.
fn the_only_honest_case() -> DeviceFacts {
    DeviceFacts {
        kind: DeviceKind::BlockDevice,
        write_cache: Some(WriteCache::WriteThrough),
        virtualized: Some(false),
        flush_succeeded: true,
        write_read_back: Some(true),
    }
}

// --- Beweise gegen das Geraet --------------------------------------------

#[test]
fn a_flush_that_errors_settles_the_matter() {
    // Der eine Fall, der ohne jeden weiteren Umstand entschieden ist.
    let mut facts = the_only_honest_case();
    facts.flush_succeeded = false;
    assert_eq!(judge(&facts).verdict, FlushVerdict::Refused);
}

#[test]
fn data_that_does_not_come_back_settles_the_matter() {
    let mut facts = the_only_honest_case();
    facts.write_read_back = Some(false);
    assert_eq!(judge(&facts).verdict, FlushVerdict::Refused);
}

#[test]
fn a_failed_flush_outranks_a_successful_write_probe() {
    // Die Beweise gegen das Geraet werden zuerst geprueft. Sonst koennte eine
    // gelungene Schreibprobe einen Flush-Fehler ueberdecken.
    let facts = DeviceFacts {
        flush_succeeded: false,
        write_read_back: Some(true),
        ..the_only_honest_case()
    };
    let check = judge(&facts);
    assert_eq!(check.verdict, FlushVerdict::Refused);
    assert!(check.reason.contains("FLUSH"));
}

// --- Was nicht entscheidbar ist ------------------------------------------

#[test]
fn a_successful_flush_alone_proves_nothing() {
    // Genau die Luege, um die es in Abschnitt 5.3 geht: Ein virtualisiertes
    // Geraet bestaetigt FLUSH sofort.
    assert_eq!(judge(&unknown()).verdict, FlushVerdict::Undecidable);
}

#[test]
fn a_loop_device_is_not_measurable() {
    let facts = DeviceFacts {
        kind: DeviceKind::LoopDevice,
        ..the_only_honest_case()
    };
    let check = judge(&facts);
    assert_eq!(check.verdict, FlushVerdict::Undecidable);
    assert!(check.reason.contains("Dateisystem"));
}

#[test]
fn a_regular_file_is_not_measurable() {
    let facts = DeviceFacts {
        kind: DeviceKind::RegularFile,
        ..the_only_honest_case()
    };
    assert_eq!(judge(&facts).verdict, FlushVerdict::Undecidable);
}

#[test]
fn a_virtualized_system_is_not_measurable() {
    let facts = DeviceFacts {
        virtualized: Some(true),
        ..the_only_honest_case()
    };
    let check = judge(&facts);
    assert_eq!(check.verdict, FlushVerdict::Undecidable);
    assert!(check.reason.contains("Hypervisor"));
}

#[test]
fn not_knowing_whether_it_is_virtualized_counts_as_virtualized() {
    // Ein falsches „nein" fuehrt zu Write-Back auf einem Geraet, das den Flush
    // womoeglich nur behauptet. Die beiden Fehler sind nicht gleich teuer.
    let facts = DeviceFacts {
        virtualized: None,
        ..the_only_honest_case()
    };
    assert_eq!(judge(&facts).verdict, FlushVerdict::Undecidable);
}

#[test]
fn a_volatile_write_cache_is_not_measurable() {
    let facts = DeviceFacts {
        write_cache: Some(WriteCache::WriteBack),
        ..the_only_honest_case()
    };
    let check = judge(&facts);
    assert_eq!(check.verdict, FlushVerdict::Undecidable);
    assert!(check.reason.contains("Stromausfall"));
}

#[test]
fn a_silent_kernel_is_not_measurable() {
    let facts = DeviceFacts {
        write_cache: None,
        ..the_only_honest_case()
    };
    assert_eq!(judge(&facts).verdict, FlushVerdict::Undecidable);
}

// --- Der einzige positive Fall -------------------------------------------

#[test]
fn nothing_to_lose_is_the_one_way_through() {
    let check = judge(&the_only_honest_case());
    assert_eq!(check.verdict, FlushVerdict::Honest);
    assert_eq!(check.write_mode(), WriteMode::WriteBack);
}

#[test]
fn the_honest_case_does_not_need_the_write_probe() {
    // Die Schreibprobe zerstoert den Bereich, auf den sie zeigt. Sie darf
    // deshalb nicht Voraussetzung fuer ein Ergebnis sein — nur ihr Fehlschlag
    // zaehlt.
    let facts = DeviceFacts {
        write_read_back: None,
        ..the_only_honest_case()
    };
    assert_eq!(judge(&facts).verdict, FlushVerdict::Honest);
}

/// Der Test, um den es geht: Von allen 216 Kombinationen darf genau eine zu
/// `Honest` fuehren.
///
/// Ohne diese Aufzaehlung waere jede spaetere Erweiterung von `judge` eine
/// Wette darauf, dass niemand versehentlich einen zweiten Weg oeffnet.
#[test]
fn exactly_one_combination_of_facts_yields_honest() {
    let kinds = [
        DeviceKind::BlockDevice,
        DeviceKind::LoopDevice,
        DeviceKind::RegularFile,
        DeviceKind::Unknown,
    ];
    let caches = [
        None,
        Some(WriteCache::WriteBack),
        Some(WriteCache::WriteThrough),
    ];
    let virtualized = [None, Some(true), Some(false)];
    let flushed = [true, false];
    let read_back = [None, Some(true), Some(false)];

    let mut honest = Vec::new();
    let mut total = 0;
    for kind in kinds {
        for write_cache in caches {
            for virtualized in virtualized {
                for flush_succeeded in flushed {
                    for write_read_back in read_back {
                        total += 1;
                        let facts = DeviceFacts {
                            kind,
                            write_cache,
                            virtualized,
                            flush_succeeded,
                            write_read_back,
                        };
                        let check = judge(&facts);

                        // Was nicht `Honest` ist, muss Write-Through sein.
                        // Abschnitt 5.3 stellt „negativ" und „nicht
                        // durchfuehrbar" gleich.
                        if check.verdict != FlushVerdict::Honest {
                            assert_eq!(check.write_mode(), WriteMode::WriteThrough);
                        }
                        if check.verdict == FlushVerdict::Honest {
                            honest.push(facts);
                        }
                    }
                }
            }
        }
    }

    assert_eq!(total, 216);
    // Ein echtes Blockgeraet, nicht virtualisiert, ohne fluechtigen Cache,
    // Flush fehlerfrei — und die Schreibprobe entweder gelungen oder nicht
    // gelaufen.
    assert_eq!(honest.len(), 2, "gefunden: {honest:#?}");
    for facts in &honest {
        assert_eq!(facts.kind, DeviceKind::BlockDevice);
        assert_eq!(facts.write_cache, Some(WriteCache::WriteThrough));
        assert_eq!(facts.virtualized, Some(false));
        assert!(facts.flush_succeeded);
        assert_ne!(facts.write_read_back, Some(false));
    }
}

// --- Gegen ein echtes Geraet ----------------------------------------------

/// Eine Datei, die sich nach dem Test selbst wegraeumt.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("ferrite-flush-{name}-{}.img", std::process::id()));
        let file = File::create(&path).expect("Datei anlegen");
        file.set_len(1 << 20).expect("Groesse setzen");
        Scratch(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn a_file_is_never_honest_no_matter_the_platform() {
    // Auf Linux, weil es eine Datei ist. Anderswo, weil dort keine der
    // Angaben zu bekommen ist. Beide Wege fuehren zu Write-Through, und das
    // ist die richtige Antwort.
    let scratch = Scratch::new("datei");
    let device = MemberDevice::open(&scratch.0).unwrap();

    let check = check_flush(&device, None);
    assert_ne!(check.verdict, FlushVerdict::Honest);
    assert_eq!(check.write_mode(), WriteMode::WriteThrough);
    assert!(check.facts.flush_succeeded, "sync_data hat funktioniert");
}

#[test]
fn the_write_probe_confirms_the_path_works() {
    let scratch = Scratch::new("probe");
    let device = MemberDevice::open(&scratch.0).unwrap();
    assert!(probe_write_path(&device, 4096).unwrap());

    // Und der Bereich traegt danach wirklich das Muster — die Probe ist
    // zerstoerend, das steht so im Doc-Comment.
    let mut read_back = [0u8; 16];
    device.read_at(4096, &mut read_back).unwrap();
    assert_eq!(read_back[0], 0x5A);
    assert_eq!(read_back[1], 0x5A ^ 1);
}

#[test]
fn the_write_probe_does_not_lift_a_file_to_honest() {
    let scratch = Scratch::new("probe-hebt-nicht");
    let device = MemberDevice::open(&scratch.0).unwrap();
    let read_back = probe_write_path(&device, 4096).unwrap();

    let check = check_flush(&device, Some(read_back));
    assert_ne!(check.verdict, FlushVerdict::Honest);
}

#[test]
fn a_read_only_device_refuses_the_write_probe() {
    let scratch = Scratch::new("nur-lesend");
    let device = MemberDevice::open_read_only(&scratch.0).unwrap();
    assert!(probe_write_path(&device, 4096).is_err());
}
