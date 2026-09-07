// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Die Handbuchseite — aus derselben Quelle wie `ferrite --help`.
//!
//! # Warum erzeugt und nicht geschrieben
//!
//! Eine Handbuchseite, die von Hand neben der Hilfe gepflegt wird, weicht
//! ab. Nicht sofort, sondern beim dritten neuen Schalter, und dann steht in
//! `man ferrite` etwas anderes als in `ferrite --help` — und der Betreiber
//! glaubt dem, was er zuerst liest.
//!
//! Deshalb steht hier nur der Rahmen: Abschnitte, die eine Handbuchseite
//! braucht und eine Hilfe nicht (Dateien, Rueckgabewerte, Verweise). Der
//! Rumpf ist [`crate::args::HELP`], Zeichen fuer Zeichen.
//!
//! `packaging/ferrite.8` ist das Ergebnis von
//! `cargo run -p ferrite-ctl --example dump_man`, und ein Test in
//! `ctl/tests/packaging.rs` haelt die eingecheckte Datei dagegen.

/// Wohin das Paket die Konfiguration legt.
///
/// Steht hier und nicht nur im Bauskript: Die Handbuchseite nennt den Pfad,
/// die Beispielkonfiguration traegt ihn in der ersten Zeile, und das Bauskript
/// legt die Datei dorthin. Weichen die drei ab, findet der Betreiber eine
/// Datei, die nicht gelesen wird.
pub const CONFIG_PATH: &str = "/etc/ferrite/ferrite.conf";

/// Wohin das Paket das Programm legt.
///
/// Die systemd-Units rufen genau diesen Pfad auf.
pub const BINARY_PATH: &str = "/usr/bin/ferrite";

/// Die Handbuchseite in roff, fertig fuer `/usr/share/man/man8/ferrite.8`.
pub fn page() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let mut out = String::new();

    // Kein Datum im `.TH`: Ein Datum, das niemand nachfuehrt, ist eine
    // Falschaussage mit Zeitstempel. Die Version sagt ohnehin mehr.
    out.push_str(&format!(
        ".TH FERRITE 8 \"\" \"ferrite {version}\" \"Systemverwaltung\"\n"
    ));

    out.push_str(".SH BEZEICHNUNG\n");
    out.push_str("ferrite \\- Paritaets-Array mit gemischten Plattengroessen\n");

    out.push_str(".SH BESCHREIBUNG\n");
    out.push_str(&roff_paragraph(
        "Ferrite bildet Paritaet ueber gleiche Offsets statt ueber Streifen. \
         Jede Datenplatte traegt ein eigenes Dateisystem und bleibt einzeln \
         lesbar; faellt mehr aus, als die Paritaet abdeckt, sind die uebrigen \
         Platten vollstaendig und einzeln montierbar.",
    ));
    out.push_str(&roff_paragraph(
        "Meldet btrfs einen Block als korrupt, rekonstruiert Ferrite ihn aus \
         der Paritaet und schreibt ihn zurueck. Eigene Pruefsummen ueber \
         Nutzdaten gibt es nicht \\- die liegen bei btrfs.",
    ));

    out.push_str(".SH AUFRUFE\n");
    out.push_str(&roff_block(crate::args::HELP));

    out.push_str(".SH DATEIEN\n");
    out.push_str(&roff_file(
        CONFIG_PATH,
        "Wo nach Members gesucht wird und was mit dem Ergebnis geschehen soll. \
         Welche Platte welche Rolle traegt, steht in ihrem Superblock und nicht \
         hier.",
    ));
    out.push_str(&roff_file(
        "/usr/share/doc/ferrite/ferrite.conf.example",
        "Die Beispielkonfiguration mit allen Schluesseln.",
    ));
    out.push_str(&roff_file(
        "/var/lib/ferrite/journal",
        "Das Betriebstagebuch, eine Zeile je Ereignis. Wohin es geht, sagt \
         `journal =` in der Konfiguration.",
    ));
    out.push_str(&roff_file(
        "/run/ferrite",
        "Einhaengepunkte der einzelnen Members, waehrend das Array laeuft.",
    ));

    out.push_str(".SH RUECKGABEWERTE\n");
    out.push_str(&roff_item("0", "Alles in Ordnung."));
    out.push_str(&roff_item(
        "1",
        "Ein Befund: das Array laeuft degradiert, oder ein Scrub hat \
         Abweichungen gefunden. Kein Fehler des Programms.",
    ));
    out.push_str(&roff_item(
        "2",
        "Nicht durchfuehrbar \\- das Array laesst sich so nicht zusammensetzen.",
    ));
    out.push_str(&roff_item("64", "Fehler in der Bedienung."));

    out.push_str(".SH DIENSTE\n");
    out.push_str(&roff_paragraph(
        "Das Paket bringt \\fBferrite.service\\fR mit, dazu \
         \\fBferrite\\-scrub.timer\\fR fuer einen monatlichen Durchlauf. \
         \\fBKeines von beiden wird bei der Installation eingeschaltet.\\fR \
         Ein Dienst, der ungefragt Blockgeraete uebernimmt und Dateisysteme \
         einhaengt, gehoert nicht zu den Dingen, die ein Paket im Vorbeigehen \
         entscheidet.",
    ));
    out.push_str(&roff_block(
        "    systemctl enable --now ferrite.service\n    systemctl enable --now ferrite-scrub.timer\n",
    ));
    out.push_str(&roff_paragraph(
        "Der Scrub laeuft \\fBohne \\-\\-repair\\fR. Ein Zeitplan, der von \
         selbst Paritaet neu bildet, ueberschriebe eine veraltete Paritaet \
         auch dann, wenn die Ursache noch da ist.",
    ));

    out.push_str(".SH VORAUSSETZUNGEN\n");
    out.push_str(&roff_paragraph(
        "Linux mit geladenem \\fBublk_drv\\fR und \\fB/dev/fuse\\fR; das Paket \
         legt dafuer eine Datei unter \\fI/usr/lib/modules\\-load.d\\fR ab. \
         Fuer den FUSE\\-Passthrough Kernel 6.9 oder neuer \\- auf aelteren \
         Kernen laeuft alles durch den Prozess, langsamer, aber richtig.",
    ));

    out.push_str(".SH SIEHE AUCH\n");
    out.push_str(&roff_paragraph(
        "\\fBbtrfs\\-scrub\\fR(8), \\fBsystemd.timer\\fR(5)",
    ));
    out.push_str(&roff_paragraph(
        "Das Formatdokument \\fIdocs/FORMAT.md\\fR ist normativ: Es beschreibt \
         das On\\-Disk\\-Layout, und es ist bei 1.0 eingefroren.",
    ));

    out
}

/// Ein Absatz Fliesstext.
fn roff_paragraph(text: &str) -> String {
    format!(".PP\n{}\n", escape_leading(text))
}

/// Ein Block, der so bleiben muss, wie er ist.
///
/// `.nf` schaltet den Umbruch ab. Ohne das zoege roff die Einrueckung der
/// Hilfe zusammen, und aus einer Aufrufuebersicht wuerde ein Fliesstext.
fn roff_block(text: &str) -> String {
    let mut out = String::from(".nf\n");
    for line in text.lines() {
        out.push_str(&escape_leading(&escape(line)));
        out.push('\n');
    }
    out.push_str(".fi\n");
    out
}

/// Ein Eintrag der Dateiliste.
fn roff_file(path: &str, what: &str) -> String {
    format!(".TP\n.I {}\n{}\n", escape(path), escape_leading(what))
}

/// Ein Eintrag der Rueckgabewerte.
fn roff_item(value: &str, what: &str) -> String {
    format!(".TP\n.B {value}\n{}\n", escape_leading(what))
}

/// Macht aus einer Zeile Text eine Zeile roff.
///
/// Zwei Zeichen sind gefaehrlich: Der Backslash leitet in roff jede
/// Anweisung ein, und ein Bindestrich wird als weiches Trennzeichen gesetzt
/// \- beim Kopieren aus dem Terminal kaeme dann ein Unicode\-Strich heraus,
/// und `ferrite \-\-data` liefe nicht.
fn escape(line: &str) -> String {
    let mut out = line.replace('\\', "\\e").replace('-', "\\-");
    // Der Gedankenstrich kommt im deutschen Text staendig vor. Ihn als UTF-8
    // stehen zu lassen ginge auf den meisten Systemen gut — `man` schickt die
    // Seite durch `preconv` —, aber eben nur auf den meisten. roff hat ein
    // eigenes Zeichen dafuer, und das ist ueberall dasselbe.
    for (from, to) in ESCAPES {
        out = out.replace(from, to);
    }
    out
}

/// Zeichen, die im deutschen Text vorkommen und in roff einen eigenen Namen
/// haben.
///
/// Was hier fehlt, faellt in `the_page_is_pure_ascii` auf. Umlaute stehen
/// bewusst nicht drin: Sie gehoeren nach den Projektregeln ausgeschrieben, und
/// eine stille Umschrift an dieser Stelle waere eine Einladung, das zu
/// vergessen.
const ESCAPES: &[(&str, &str)] = &[
    ("\u{2014}", "\\(em"),
    ("\u{2013}", "\\(en"),
    ("\u{201e}", "\\(Bq"),
    ("\u{201c}", "\\(lq"),
    ("\u{201d}", "\\(rq"),
    ("\u{2026}", "\\&..."),
];

/// Schuetzt eine Zeile, die mit `.` oder `'` beginnt.
///
/// Beides ist am Zeilenanfang eine roff\-Anweisung. Eine Zeile der Hilfe, die
/// zufaellig so anfaengt, verschwaende sonst spurlos.
fn escape_leading(line: &str) -> String {
    if line.starts_with('.') || line.starts_with('\'') {
        format!("\\&{line}")
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_from_the_help_is_in_the_manual() {
        // Die Handbuchseite traegt die Hilfe Zeichen fuer Zeichen. Faellt das
        // je auseinander, faellt es hier auf und nicht beim Leser.
        let page = page();
        for command in [
            "create",
            "status",
            "run",
            "scrub",
            "replace",
            "rebuild",
            "discover",
            "check\\-flush",
            "journal",
        ] {
            assert!(
                page.contains(command),
                "{command} fehlt in der Handbuchseite"
            );
        }
    }

    #[test]
    fn the_manual_names_the_paths_the_package_uses() {
        let page = page();
        assert!(page.contains(&escape(CONFIG_PATH)));
        // Die Beispielkonfiguration nennt denselben Pfad in ihrer ersten
        // Zeile. Weichen die beiden ab, sucht der Betreiber am falschen Ort.
        assert!(
            crate::config::EXAMPLE.starts_with(&format!("# {CONFIG_PATH}\n")),
            "die Beispielkonfiguration nennt einen anderen Pfad"
        );
    }

    #[test]
    fn a_backslash_does_not_become_an_instruction() {
        assert_eq!(escape("a\\b"), "a\\eb");
        assert_eq!(escape("--data"), "\\-\\-data");
        // Erst der Backslash, dann der Bindestrich: andersherum wuerde der
        // frisch gesetzte Backslash gleich wieder verdoppelt.
        assert_eq!(escape("\\-"), "\\e\\-");
    }

    #[test]
    fn a_line_starting_with_a_dot_is_protected() {
        assert_eq!(escape_leading(".foo"), "\\&.foo");
        assert_eq!(escape_leading("'foo"), "\\&'foo");
        assert_eq!(escape_leading("foo.bar"), "foo.bar");
    }

    #[test]
    fn the_block_keeps_every_line_of_its_source() {
        let block = roff_block("eins\nzwei\n");
        assert!(block.starts_with(".nf\n"));
        assert!(block.ends_with(".fi\n"));
        assert_eq!(block.lines().count(), 4);
    }

    #[test]
    fn the_page_starts_with_a_header_roff_understands() {
        assert!(page().starts_with(".TH FERRITE 8 "));
    }

    #[test]
    fn the_page_is_pure_ascii() {
        // Eine Handbuchseite in UTF-8 laeuft auf den meisten Systemen — `man`
        // schickt sie durch `preconv`. Auf den uebrigen kommt Buchstabensalat
        // heraus, und zwar genau dort, wo jemand nachschlaegt, weil er nicht
        // weiterweiss. Was roff selbst kennt, wird uebersetzt; alles andere
        // faellt hier auf und gehoert im Quelltext ausgeschrieben.
        let page = page();
        let fremd: Vec<char> = page.chars().filter(|c| !c.is_ascii()).collect();
        assert!(
            fremd.is_empty(),
            "nicht-ASCII in der Handbuchseite: {fremd:?} — entweder in ESCAPES \
             eintragen oder im Quelltext ausschreiben"
        );
    }

    #[test]
    fn an_em_dash_becomes_a_roff_character() {
        assert_eq!(escape("a\u{2014}b"), "a\\(emb");
        // Und der Backslash, den ESCAPES einsetzt, wird nicht noch einmal
        // verdoppelt: Die Reihenfolge in `escape` haengt daran.
        assert!(!escape("a\u{2014}b").contains("\\e("));
    }
}
