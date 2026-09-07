// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Was das Paket verspricht, gegen das gehalten, was der Code sagt.
//!
//! # Wogegen das schuetzt
//!
//! Ein Paket besteht aus einem Dutzend Dateien, die einander Pfade zurufen:
//! Die Unit ruft `/usr/bin/ferrite` auf, das Bauskript legt es dorthin, die
//! Handbuchseite nennt `/etc/ferrite/ferrite.conf`, das Bauskript legt es
//! dorthin, und die Beispielkonfiguration traegt denselben Pfad in ihrer
//! ersten Zeile. Verschiebt jemand einen davon, faellt es **nirgends** auf:
//! Das Paket baut, installiert sich, und der Dienst startet nicht — oder
//! schlimmer, er startet und liest eine Konfiguration, die niemand
//! bearbeitet hat.
//!
//! Diese Tests brauchen weder Root noch eine Distribution. Sie laufen ueberall
//! und finden genau die Klasse Fehler, die man sonst erst auf der Zielmaschine
//! sieht.

use std::path::{Path, PathBuf};

use ferrite_ctl::man;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("ctl/ liegt unter der Wurzel")
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Alles, was das Paket ablegt — einmal aufgeschrieben, gegen beide Bauwege
/// gehalten.
///
/// Das `.deb` ist der Weg, der in CI wirklich installiert und benutzt wird;
/// das `.rpm` wird gebaut und sein Inhalt geprueft. Genau deshalb braucht es
/// diese Liste: Was nur in einem der beiden landet, faellt sonst erst dem
/// auf, der die andere Distribution benutzt.
const INSTALLED: &[&str] = &[
    "/usr/bin/ferrite",
    "ferrite.service",
    "ferrite-scrub.service",
    "ferrite-scrub.timer",
    "modules-load.d/ferrite.conf",
    "/etc/ferrite/ferrite.conf",
    "ferrite.conf.example",
    "ferrite.8",
    "/var/lib/ferrite",
];

#[test]
fn the_checked_in_manual_is_what_the_generator_writes() {
    // `cargo run -p ferrite-ctl --example dump_man > packaging/ferrite.8`
    assert_eq!(
        read("packaging/ferrite.8"),
        man::page(),
        "packaging/ferrite.8 ist veraltet — neu erzeugen mit\n    \
         cargo run -p ferrite-ctl --example dump_man > packaging/ferrite.8"
    );
}

#[test]
fn the_checked_in_config_example_is_what_the_code_says() {
    assert_eq!(
        read("packaging/ferrite.conf.example"),
        ferrite_ctl::config::EXAMPLE,
        "packaging/ferrite.conf.example ist veraltet — neu erzeugen mit\n    \
         cargo run -p ferrite-ctl --example dump_config > packaging/ferrite.conf.example"
    );
}

#[test]
fn every_unit_calls_the_path_the_package_installs() {
    // Eine Unit, die auf `/usr/local/bin/ferrite` zeigt, faellt beim Bauen
    // nicht auf und beim Starten sofort — auf der Maschine des Betreibers.
    let mut gesehen = 0;
    for unit in ["ferrite.service", "ferrite-scrub.service"] {
        for line in read(&format!("packaging/systemd/{unit}")).lines() {
            let Some(command) = line.strip_prefix("ExecStart=") else {
                continue;
            };
            gesehen += 1;
            assert!(
                command.starts_with(man::BINARY_PATH),
                "{unit}: ExecStart zeigt auf {command}, das Paket legt es nach {}",
                man::BINARY_PATH
            );
        }
    }
    assert_eq!(gesehen, 2, "beide Units brauchen ein ExecStart");
}

/// Die rpm-Makros, die im Spec vorkommen, mit den Werten, unter denen gebaut
/// wird.
///
/// `_unitdir` steht so in `build-rpm.sh` — es kommt sonst aus
/// `systemd-rpm-macros`, und die gibt es auf einem Debian-Bauwirt nicht.
const RPM_MACROS: &[(&str, &str)] = &[
    ("%{buildroot}", ""),
    ("%{_bindir}", "/usr/bin"),
    ("%{_sysconfdir}", "/etc"),
    ("%{_unitdir}", "/usr/lib/systemd/system"),
    ("%{_mandir}", "/usr/share/man"),
    ("%{_docdir}", "/usr/share/doc"),
    ("%{_prefix}", "/usr"),
];

/// Wohin das Bauskript wirklich schreibt.
///
/// # Warum nicht einfach nach dem Pfad suchen
///
/// Ein `contains` ueber die ganze Datei findet den Pfad auch dann noch, wenn
/// er nur noch in der `conffiles`-Liste steht und die `install`-Zeile
/// woandershin zeigt. Gemessen: Genau so blieb die Sabotage
/// „Konfiguration nach /etc/ferrite.conf statt /etc/ferrite/" unentdeckt —
/// das Paket haette eine Konffile angemeldet, die es nicht ablegt.
///
/// Deshalb wird gesammelt, was hinter `$STAGE` steht: die Ziele der
/// `install`-Aufrufe, der Umleitungen und der angelegten Verzeichnisse.
fn deb_destinations() -> Vec<String> {
    let text = read("packaging/build-deb.sh");
    let mut ziele = Vec::new();
    for (offset, _) in text.match_indices("$STAGE") {
        let rest = &text[offset + "$STAGE".len()..];
        let ende = rest.find('"').unwrap_or(0);
        let pfad = &rest[..ende];
        if pfad.starts_with('/') {
            ziele.push(pfad.to_string());
        }
    }
    assert!(
        ziele.len() > 5,
        "aus dem Bauskript liessen sich kaum Ziele lesen: {ziele:?}"
    );
    ziele
}

#[test]
fn both_build_ways_lay_down_the_same_things() {
    // Der Spec spricht in Makros, das Bauskript in Pfaden. Ohne diese
    // Uebersetzung liesse sich das eine nicht gegen das andere halten — und
    // was nur in einem der beiden Pakete landet, faellt erst dem auf, der die
    // andere Distribution benutzt.
    let deb = deb_destinations();
    let mut spec = read("packaging/ferrite.spec");
    for (macro_name, value) in RPM_MACROS {
        spec = spec.replace(macro_name, value);
    }
    for thing in INSTALLED {
        assert!(
            deb.iter().any(|ziel| ziel.contains(thing)),
            "das .deb legt {thing} nicht ab, sondern nur: {deb:?}"
        );
        assert!(spec.contains(thing), "das .rpm legt {thing} nicht ab");
    }
}

#[test]
fn the_configuration_is_a_conffile_in_both() {
    // Ohne das ueberschreibt die naechste Installation die Konfiguration des
    // Betreibers wortlos. Ein Upgrade, das die Suchpfade zuruecksetzt, ist ein
    // Array, das nach dem naechsten `apt upgrade` nicht mehr hochkommt.
    let deb = read("packaging/build-deb.sh");
    assert!(
        deb.contains("DEBIAN/conffiles"),
        "das .deb fuehrt keine conffiles-Liste"
    );
    let spec = read("packaging/ferrite.spec");
    assert!(
        spec.contains("%config(noreplace)"),
        "das .rpm markiert die Konfiguration nicht als noreplace"
    );
}

#[test]
fn no_maintainer_script_switches_a_service_on() {
    // Die Entscheidung, die nicht unbemerkt kippen darf: Wer ferrite.service
    // startet, uebergibt Blockgeraete an ublk und haengt Dateisysteme ein. Ein
    // Paket, das das beim Auspacken tut, hat sich diese Entscheidung angemasst.
    for script in [
        "packaging/deb/postinst",
        "packaging/deb/prerm",
        "packaging/deb/postrm",
        "packaging/ferrite.spec",
    ] {
        for line in without_here_documents(&read(script)) {
            let line = line.trim_start();
            assert!(
                !line.starts_with("systemctl enable") && !line.starts_with("systemctl start"),
                "{script} schaltet einen Dienst ein: {line}"
            );
        }
    }
}

/// Die Zeilen eines Shell-Skripts ohne die Textbloecke darin.
///
/// # Warum das noetig ist
///
/// Das `postinst` **zeigt** dem Betreiber `systemctl enable --now
/// ferrite.service` — als Text in einem Here-Dokument, nicht als Befehl. Ein
/// Test, der nur nach der Zeichenkette sucht, kann beides nicht auseinander
/// halten, und ein Test, der stattdessen auf die Einrueckung schaut, haelt das
/// erste eingerueckte `systemctl` fuer Text. Also wird gezaehlt, wo ein
/// Here-Dokument anfaengt und wo es aufhoert.
fn without_here_documents(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut ende: Option<String> = None;
    for line in text.lines() {
        if let Some(marke) = &ende {
            if line.trim_end() == marke {
                ende = None;
            }
            continue;
        }
        if let Some((_, rest)) = line.split_once("<<") {
            let marke = rest.trim().trim_start_matches('-').trim_matches('\'');
            if !marke.is_empty() && marke.chars().all(|c| c.is_ascii_uppercase()) {
                ende = Some(marke.to_string());
            }
        }
        lines.push(line);
    }
    assert!(ende.is_none(), "ein Here-Dokument wurde nicht geschlossen");
    lines
}

#[test]
#[cfg(unix)]
fn every_script_may_actually_be_run() {
    // Das Ausfuehrbar-Bit steht im Git-Index, nicht in der Arbeitskopie eines
    // Windows-Rechners — und wer dort eine Datei neu anlegt, vergisst es. Das
    // faellt nirgends auf: Die Tests laufen, das Skript laesst sich mit `sh`
    // starten, und erst der CI-Job bricht mit `Permission denied` ab.
    // Gemessen: genau so, beim ersten Anlauf.
    //
    // **Auf einem WSL-Checkout unter /mnt/c faellt dieser Test nie um.** DrvFs
    // meldet jede Datei als 0777, egal was `chmod` sagt. Nachgewiesen wurde er
    // deshalb in einem frischen `git clone` auf einem echten Linux-Dateisystem:
    // dort steht das Bit aus dem Index in der Datei, und ohne es wird der Test
    // rot.
    use std::os::unix::fs::PermissionsExt;

    for script in [
        "packaging/build-deb.sh",
        "packaging/build-rpm.sh",
        "packaging/deb/postinst",
        "packaging/deb/prerm",
        "packaging/deb/postrm",
    ] {
        let path = root().join(script);
        let mode = std::fs::metadata(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
            .permissions()
            .mode();
        assert!(
            mode & 0o111 != 0,
            "{script} ist nicht ausfuehrbar ({mode:o}) — `git update-index --chmod=+x` fehlt"
        );
    }
}

#[test]
fn the_scheduled_scrub_does_not_repair() {
    // Ein Zeitplan, der von selbst Paritaet neu bildet, ueberschriebe eine
    // veraltete Paritaet auch dann, wenn die Ursache noch da ist. Erst
    // ansehen, dann entscheiden.
    let unit = read("packaging/systemd/ferrite-scrub.service");
    let start = unit
        .lines()
        .find(|line| line.starts_with("ExecStart="))
        .expect("ExecStart fehlt");
    assert!(
        !start.contains("--repair"),
        "der Scrub nach Zeitplan repariert: {start}"
    );
}

#[test]
fn the_modules_the_package_asks_for_are_the_ones_the_code_needs() {
    // `ferrite run` sagt beim Start, wenn ublk_drv fehlt. Das ist die richtige
    // Meldung — und die Datei hier ist der Grund, warum sie selten noetig ist.
    let modules = read("packaging/modules-load.d/ferrite.conf");
    let geladen: Vec<&str> = modules
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    assert_eq!(geladen, ["ublk_drv", "fuse"]);
}

#[test]
fn the_manual_and_the_example_agree_on_where_the_configuration_lives() {
    let manual = read("packaging/ferrite.8");
    // In der Handbuchseite steht der Pfad mit geschuetzten Bindestrichen.
    assert!(
        manual.contains(&man::CONFIG_PATH.replace('-', "\\-")),
        "die Handbuchseite nennt {} nicht",
        man::CONFIG_PATH
    );
    assert!(
        read("packaging/ferrite.conf.example").starts_with(&format!("# {}\n", man::CONFIG_PATH)),
        "die Beispielkonfiguration nennt einen anderen Pfad"
    );
    // Nicht „der Pfad kommt im Skript vor", sondern „das Skript legt die Datei
    // dorthin". Die `conffiles`-Liste nennt ihn ebenfalls; ein `contains` ueber
    // die Datei liesse sich davon taeuschen.
    let ziele = deb_destinations();
    assert!(
        ziele.iter().any(|ziel| ziel == man::CONFIG_PATH),
        "das Bauskript legt die Konfiguration nicht nach {}, sondern: {ziele:?}",
        man::CONFIG_PATH
    );
    // Und was es ablegt, ist auch das, was es als Konffile anmeldet.
    let skript = read("packaging/build-deb.sh");
    assert!(
        skript.contains(&format!("printf '{}\\n'", man::CONFIG_PATH)),
        "die conffiles-Liste nennt einen anderen Pfad als die Installation"
    );
}
