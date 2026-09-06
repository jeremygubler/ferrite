// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Der Parser fuer die Meldungen, die btrfs in den Kernel-Ringpuffer schreibt.
//!
//! Kein I/O, keine Allokation, keine Konfiguration — dieselbe Trennung wie
//! zwischen `format/` und `engine/`. Was hier steht, laesst sich mit einem
//! `&str` pruefen, und genau das tut die Testdatei.
//!
//! # Die Haltung des Parsers
//!
//! Eine Zeile aus `/dev/kmsg` ist **Eingabe**, nicht Wahrheit: Der Puffer ist
//! beschreibbar, die Formulierungen aendern sich zwischen Kernelversionen, und
//! eine abgeschnittene Zeile sieht einer vollstaendigen zum Verwechseln
//! aehnlich. Deshalb wird nur erkannt, was vollstaendig dasteht. Fehlt ein
//! Feld, kommt `None` und keine Schaetzung.

/// Ein Pruefsummenfehler, den btrfs beim Scrub gemeldet hat.
///
/// Die Felder sind Ausschnitte der Zeile, keine Kopien: Der Parser allokiert
/// nicht, und wer die Werte behalten will, sagt das selbst.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrubError<'a> {
    /// Das Geraet, so wie btrfs es nennt — meist ein Pfad wie `/dev/ublkb0`.
    pub device: &'a str,
    /// Der logische Offset innerhalb des Dateisystems. Fuer die Reparatur ohne
    /// Bedeutung, fuer die Diagnose nicht.
    pub logical: u64,
    /// Der Offset **auf dem Geraet**. Das ist der Wert, um den es geht.
    pub physical: u64,
    /// Die Laenge in Bytes, sofern die Meldung sie nennt. Meldungen ueber
    /// Metadaten nennen sie nicht — dort steht die Baumhoehe statt einer
    /// Laenge, und die sagt ueber die Ausdehnung auf der Platte nichts.
    pub length: Option<u32>,
}

/// Liest eine Kernelzeile als Pruefsummenfehler, falls sie einer ist.
///
/// Erkannt werden beide Formen, die der Scrub erzeugt: die ausfuehrliche
/// (`checksum error at logical … on dev …, physical …, …, length …`) und die
/// knappe (`unable to fixup (regular) error at logical … on dev … physical …`).
/// Sie unterscheiden sich im Komma vor `physical`, weshalb hier nach
/// `" physical "` gesucht wird und nicht nach `", physical "`.
///
/// Nicht erkannt wird die Zaehlerzeile `bdev … errs: …`. Sie nennt keinen
/// Offset, und ohne Offset gibt es nichts zu reparieren.
pub fn parse_scrub_error(line: &str) -> Option<ScrubError<'_>> {
    // Beide Marker muessen da sein. `BTRFS` allein traegt keine Aussage — der
    // Ringpuffer enthaelt auch Zeilen ueber Mounts und Geraetefunde.
    if !line.contains("BTRFS") {
        return None;
    }

    let logical = number_after(line, " at logical ")?;
    let device = word_after(line, " on dev ")?;
    let physical = number_after(line, " physical ")?;
    let length = number_after(line, " length ").and_then(|value| u32::try_from(value).ok());

    Some(ScrubError {
        device,
        logical,
        physical,
        length,
    })
}

/// Die Dezimalzahl, die direkt hinter `key` steht.
///
/// `None`, wenn `key` fehlt oder dahinter keine Ziffer kommt. Ein Ueberlauf
/// ergibt ebenfalls `None`: Eine Zahl, die nicht in `u64` passt, ist keine
/// gekuerzte Zahl, sondern gar keine.
fn number_after(line: &str, key: &str) -> Option<u64> {
    let rest = line.split(key).nth(1)?;
    let end = rest
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(rest.len());
    rest.get(..end)?.parse().ok()
}

/// Das Wort, das direkt hinter `key` steht — bis zum naechsten Komma,
/// Leerzeichen oder Zeilenende.
fn word_after<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.split(key).nth(1)?;
    let end = rest.find([',', ' ']).unwrap_or(rest.len());
    let word = rest.get(..end)?;
    (!word.is_empty()).then_some(word)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Die ausfuehrliche Form, wie sie `scrub_print_warning` erzeugt.
    const VERBOSE: &str = "BTRFS warning (device ublkb0): checksum error at logical 22020096 on dev /dev/ublkb0, physical 22020096, root 5, inode 257, offset 0, length 4096, links 1 (path: datei)";

    /// Die knappe Form ohne Komma vor `physical`.
    const TERSE: &str = "BTRFS error (device ublkb0): unable to fixup (regular) error at logical 22020096 on dev /dev/ublkb0 physical 22020096";

    #[test]
    fn the_verbose_form_yields_device_offset_and_length() {
        let error = parse_scrub_error(VERBOSE).expect("erkannt");
        assert_eq!(error.device, "/dev/ublkb0");
        assert_eq!(error.logical, 22_020_096);
        assert_eq!(error.physical, 22_020_096);
        assert_eq!(error.length, Some(4096));
    }

    #[test]
    fn the_terse_form_yields_no_length() {
        let error = parse_scrub_error(TERSE).expect("erkannt");
        assert_eq!(error.device, "/dev/ublkb0");
        assert_eq!(error.physical, 22_020_096);
        assert_eq!(
            error.length, None,
            "eine Laenge, die nicht dasteht, darf nicht erfunden werden"
        );
    }

    #[test]
    fn a_metadata_error_yields_no_length() {
        let line = "BTRFS warning (device ublkb1): checksum/header error at logical 30408704 on dev /dev/ublkb1, physical 30408704: metadata leaf (level 0) in tree 5";
        let error = parse_scrub_error(line).expect("erkannt");
        assert_eq!(error.device, "/dev/ublkb1");
        assert_eq!(error.physical, 30_408_704);
        assert_eq!(error.length, None);
    }

    #[test]
    fn the_counter_line_is_not_a_damage_report() {
        let line = "BTRFS error (device ublkb0): bdev /dev/ublkb0 errs: wr 0, rd 0, flush 0, corrupt 1, gen 0";
        assert_eq!(
            parse_scrub_error(line),
            None,
            "ohne Offset gibt es nichts zu reparieren"
        );
    }

    #[test]
    fn a_line_from_another_subsystem_is_ignored() {
        let line = "EXT4-fs error (device sda1): at logical 100 on dev /dev/sda1 physical 200";
        assert_eq!(parse_scrub_error(line), None);
    }

    #[test]
    fn a_truncated_line_is_refused_instead_of_guessed() {
        let line =
            "BTRFS warning (device ublkb0): checksum error at logical 22020096 on dev /dev/ubl";
        assert_eq!(
            parse_scrub_error(line),
            None,
            "ohne physical gibt es keinen Offset"
        );
    }

    #[test]
    fn a_physical_offset_that_does_not_fit_is_refused() {
        let line = "BTRFS warning (device ublkb0): checksum error at logical 1 on dev /dev/ublkb0, physical 99999999999999999999999, length 4096";
        assert_eq!(parse_scrub_error(line), None);
    }

    #[test]
    fn a_length_that_does_not_fit_is_dropped_but_the_offset_survives() {
        let line = "BTRFS warning (device ublkb0): checksum error at logical 1 on dev /dev/ublkb0, physical 4096, length 99999999999";
        let error = parse_scrub_error(line).expect("erkannt");
        assert_eq!(error.physical, 4096);
        assert_eq!(error.length, None);
    }

    /// Derselbe feste LCG wie in `format/tests/roundtrip.rs`: reproduzierbar,
    /// ohne Test-Dependency, in Millisekunden durch.
    fn lcg(state: &mut u64) -> u8 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (*state >> 33) as u8
    }

    #[test]
    fn random_bytes_never_panic() {
        let mut state = 0x5EED_5EED_5EED_5EEDu64;
        let alphabet = b"BTRFS at logical on dev /physical, length 0123456789\n\xc3\xa4";
        for _ in 0..2000 {
            let len = usize::from(lcg(&mut state)) % 200;
            let line: String = (0..len)
                .map(|_| {
                    let index = usize::from(lcg(&mut state)) % alphabet.len();
                    char::from(alphabet[index])
                })
                .collect();
            // Das Ergebnis ist gleichgueltig, der fehlende Panic ist es nicht.
            let _ = parse_scrub_error(&line);
        }
    }

    #[test]
    fn multibyte_characters_do_not_split_a_slice() {
        // Ein Pfad mit Umlaut hinter dem Marker: Wer hier mit Byte-Offsets
        // rechnet statt mit Zeichengrenzen, paniert.
        let line = "BTRFS warning (device x): checksum error at logical 1 on dev /dev/plättchen, physical 4096, length 4096";
        let error = parse_scrub_error(line).expect("erkannt");
        assert_eq!(error.device, "/dev/plättchen");
    }
}
