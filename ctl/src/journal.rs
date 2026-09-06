// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Das Betriebstagebuch — und die Benachrichtigung, die daran haengt.
//!
//! # Wofuer es da ist
//!
//! Ein Jahr Betrieb ohne Aufzeichnung ergibt „lief eigentlich gut". Das ist
//! wertlos: Es ueberzeugt niemanden, es findet kein Muster, und es sagt nicht,
//! ob die Selbstheilung je etwas zu tun hatte. Ein Jahr **mit** Aufzeichnung
//! ergibt „8760 Stunden, 52 Scrubs, 3 reparierte Bloecke, 2 unsaubere
//! Abschaltungen mit 14 zurueckgespielten Writes, 0 verloren". Das ist ein
//! Beweis.
//!
//! Die Zahlen gibt es alle schon — `Recovered`, `RepairStats`, `ScrubOutcome`.
//! Bisher wurden sie weggeworfen, sobald das Kommando zurueckkehrte.
//!
//! # Warum eine Zeile je Ereignis und kein JSON
//!
//! Das Tagebuch wird von zwei Sorten Leser gelesen: von einem Menschen mit
//! `tail`, und von `awk` oder `grep`. `zeitstempel schluessel=wert …` bedient
//! beide. JSON braeuchte einen Schreiber **und** einen Leser, und der Leser
//! muesste mit einer halb geschriebenen letzten Zeile zurechtkommen — nach
//! einem Stromausfall gibt es die.
//!
//! Eine unlesbare Zeile wird beim Auswerten uebersprungen und nicht als
//! Fehler gemeldet. Das ist hier richtig: Ein Tagebuch, das sich wegen eines
//! abgeschnittenen Eintrags nicht mehr lesen laesst, verliert genau das, wofuer
//! es angelegt wurde.
//!
//! # Reines Modul
//!
//! Zeitstempel kommen als Parameter herein, nicht aus der Uhr (Regel 8
//! sinngemaess). Deshalb laesst sich ein Jahr Betrieb in einem Test
//! nachspielen, ohne ein Jahr zu warten.

use std::fmt::Write as _;

/// Wie dringend ein Ereignis ist.
///
/// Die Reihenfolge ist die Rangfolge — daran haengt, ob gemeldet wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Normalbetrieb. Steht im Tagebuch, weckt niemanden.
    Info,
    /// Etwas stimmt nicht, aber die Daten sind noch vollstaendig.
    Warning,
    /// Etwas ist verloren oder nicht mehr entscheidbar.
    Alert,
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warnung",
            Severity::Alert => "alarm",
        }
    }

    fn from_str(text: &str) -> Option<Self> {
        match text {
            "info" => Some(Severity::Info),
            "warnung" => Some(Severity::Warning),
            "alarm" => Some(Severity::Alert),
            _ => None,
        }
    }
}

/// Was passiert ist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Das Array wurde in Betrieb genommen.
    Started {
        members: usize,
    },
    /// Und wieder abgebaut.
    Stopped,
    /// Das Log wurde zurueckgespielt (Abschnitt 5.2).
    ///
    /// `lost` ist die Zahl der Bereiche, deren Inhalt dabei verlorenging —
    /// im gesunden Fall null. Ungleich null heisst: Absturz im degradierten
    /// Betrieb.
    Recovered {
        applied: u64,
        lost: usize,
    },
    Scrubbed {
        checked: u64,
        mismatched: usize,
        repaired: u64,
    },
    /// Der Repair-Broker hat einen angefressenen Bereich zurueckgeholt.
    BitRotRepaired {
        slot: u16,
        bytes: u64,
    },
    /// Er konnte es **nicht**: mehr als eine Quelle beschaedigt.
    RepairRefused {
        slot: u16,
    },
    Rebuilt {
        slot: u16,
        blocks: u64,
    },
    Replaced {
        slot: u16,
    },
    /// Ein Member traegt keine gueltigen Daten mehr.
    Degraded {
        slot: u16,
    },
}

impl Event {
    pub fn severity(&self) -> Severity {
        match self {
            Event::Started { .. } | Event::Stopped | Event::Replaced { .. } => Severity::Info,
            Event::Rebuilt { .. } => Severity::Info,
            Event::BitRotRepaired { .. } => Severity::Warning,
            Event::Scrubbed { mismatched, .. } => {
                if *mismatched == 0 {
                    Severity::Info
                } else {
                    Severity::Warning
                }
            }
            Event::Degraded { .. } => Severity::Warning,
            // Verlorene Daten und eine Reparatur, die nicht eindeutig ist:
            // beides Faelle, in denen ein Mensch hinsehen muss.
            Event::Recovered { lost, .. } => {
                if *lost == 0 {
                    Severity::Info
                } else {
                    Severity::Alert
                }
            }
            Event::RepairRefused { .. } => Severity::Alert,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Event::Started { .. } => "start",
            Event::Stopped => "stop",
            Event::Recovered { .. } => "recovery",
            Event::Scrubbed { .. } => "scrub",
            Event::BitRotRepaired { .. } => "reparatur",
            Event::RepairRefused { .. } => "reparatur-abgelehnt",
            Event::Rebuilt { .. } => "rebuild",
            Event::Replaced { .. } => "ersatz",
            Event::Degraded { .. } => "degradiert",
        }
    }

    /// Ein Satz fuer einen Menschen — der Betreff einer Meldung.
    pub fn headline(&self) -> String {
        match self {
            Event::Started { members } => format!("Array gestartet, {members} Members"),
            Event::Stopped => "Array abgebaut".to_string(),
            Event::Recovered { applied, lost: 0 } => {
                format!("Recovery: {applied} Writes zurueckgespielt")
            }
            Event::Recovered { applied, lost } => {
                format!("Recovery: {applied} Writes zurueckgespielt, {lost} Bereiche verloren")
            }
            Event::Scrubbed {
                checked,
                mismatched: 0,
                ..
            } => format!("Scrub: {checked} Bloecke, alles stimmig"),
            Event::Scrubbed {
                checked,
                mismatched,
                repaired,
            } => format!(
                "Scrub: {mismatched} von {checked} Bloecken passen nicht, {repaired} neu gebildet"
            ),
            Event::BitRotRepaired { slot, bytes } => {
                format!("Bit-Rot repariert: Slot {slot}, {bytes} Bytes")
            }
            Event::RepairRefused { slot } => format!(
                "Reparatur abgelehnt: Slot {slot} ist nicht eindeutig bestimmbar — \
                 mehr als eine Quelle beschaedigt"
            ),
            Event::Rebuilt { slot, blocks } => {
                format!("Rebuild fertig: Slot {slot}, {blocks} Bloecke")
            }
            Event::Replaced { slot } => format!("Slot {slot} durch eine neue Platte ersetzt"),
            Event::Degraded { slot } => format!("Slot {slot} traegt keine gueltigen Daten mehr"),
        }
    }

    fn fields(&self) -> Vec<(&'static str, String)> {
        match self {
            Event::Started { members } => vec![("members", members.to_string())],
            Event::Stopped => Vec::new(),
            Event::Recovered { applied, lost } => {
                vec![("applied", applied.to_string()), ("lost", lost.to_string())]
            }
            Event::Scrubbed {
                checked,
                mismatched,
                repaired,
            } => vec![
                ("checked", checked.to_string()),
                ("mismatched", mismatched.to_string()),
                ("repaired", repaired.to_string()),
            ],
            Event::BitRotRepaired { slot, bytes } => {
                vec![("slot", slot.to_string()), ("bytes", bytes.to_string())]
            }
            Event::RepairRefused { slot } | Event::Degraded { slot } | Event::Replaced { slot } => {
                vec![("slot", slot.to_string())]
            }
            Event::Rebuilt { slot, blocks } => {
                vec![("slot", slot.to_string()), ("blocks", blocks.to_string())]
            }
        }
    }
}

/// Eine Zeile des Tagebuchs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Sekunden seit 1970, UTC.
    pub at: i64,
    pub event: Event,
}

/// Schreibt eine Zeile.
///
/// Der Zeitstempel kommt als Parameter, nicht aus der Uhr: Sonst liesse sich
/// ein Jahr Betrieb nicht in einem Test nachspielen.
pub fn format(at: i64, event: &Event) -> String {
    let mut line = format!(
        "{} {} {}",
        iso8601(at),
        event.kind(),
        event.severity().as_str()
    );
    for (key, value) in event.fields() {
        let _ = write!(line, " {key}={value}");
    }
    line.push('\n');
    line
}

/// Liest eine Zeile zurueck.
///
/// `None` fuer alles, was nicht passt — auch fuer die halb geschriebene
/// letzte Zeile nach einem Stromausfall. Siehe den Modulkopf.
pub fn parse(line: &str) -> Option<Entry> {
    let mut parts = line.split_whitespace();
    let at = from_iso8601(parts.next()?)?;
    let kind = parts.next()?;
    let _severity = Severity::from_str(parts.next()?)?;

    let mut fields: Vec<(&str, u64)> = Vec::new();
    for part in parts {
        let (key, value) = part.split_once('=')?;
        fields.push((key, value.parse().ok()?));
    }
    let get = |name: &str| fields.iter().find(|(key, _)| *key == name).map(|(_, v)| *v);

    let event = match kind {
        "start" => Event::Started {
            members: get("members")? as usize,
        },
        "stop" => Event::Stopped,
        "recovery" => Event::Recovered {
            applied: get("applied")?,
            lost: get("lost")? as usize,
        },
        "scrub" => Event::Scrubbed {
            checked: get("checked")?,
            mismatched: get("mismatched")? as usize,
            repaired: get("repaired")?,
        },
        "reparatur" => Event::BitRotRepaired {
            slot: get("slot")? as u16,
            bytes: get("bytes")?,
        },
        "reparatur-abgelehnt" => Event::RepairRefused {
            slot: get("slot")? as u16,
        },
        "rebuild" => Event::Rebuilt {
            slot: get("slot")? as u16,
            blocks: get("blocks")?,
        },
        "ersatz" => Event::Replaced {
            slot: get("slot")? as u16,
        },
        "degradiert" => Event::Degraded {
            slot: get("slot")? as u16,
        },
        _ => return None,
    };
    Some(Entry { at, event })
}

// --- Auswertung -----------------------------------------------------------

/// Was in einem Zeitraum passiert ist.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub entries: u64,
    pub unreadable: u64,
    /// Erster und letzter Zeitstempel.
    pub span: Option<(i64, i64)>,
    pub starts: u64,
    pub recoveries: u64,
    pub writes_recovered: u64,
    pub ranges_lost: u64,
    pub scrubs: u64,
    pub blocks_checked: u64,
    pub blocks_mismatched: u64,
    pub parity_rebuilt: u64,
    pub bit_rot_repaired: u64,
    pub bytes_repaired: u64,
    pub repairs_refused: u64,
    pub rebuilds: u64,
    pub replacements: u64,
    pub alerts: u64,
}

impl Summary {
    /// Wieviele Stunden zwischen dem ersten und dem letzten Eintrag liegen.
    pub fn hours(&self) -> u64 {
        match self.span {
            Some((first, last)) if last > first => ((last - first) / 3600) as u64,
            _ => 0,
        }
    }

    /// Gab es etwas, das ein Mensch ansehen muss?
    pub fn needs_attention(&self) -> bool {
        self.alerts > 0
    }
}

/// Wertet ein ganzes Tagebuch aus.
///
/// Unlesbare Zeilen werden gezaehlt, nicht verschwiegen: Sind es viele,
/// stimmt etwas mit dem Schreiben nicht, und das gehoert bemerkt.
pub fn summarize(text: &str) -> Summary {
    let mut summary = Summary::default();

    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Some(entry) = parse(line) else {
            summary.unreadable += 1;
            continue;
        };
        summary.entries += 1;
        summary.span = Some(match summary.span {
            Some((first, last)) => (first.min(entry.at), last.max(entry.at)),
            None => (entry.at, entry.at),
        });
        if entry.event.severity() == Severity::Alert {
            summary.alerts += 1;
        }

        match entry.event {
            Event::Started { .. } => summary.starts += 1,
            Event::Stopped => {}
            Event::Recovered { applied, lost } => {
                summary.recoveries += 1;
                summary.writes_recovered += applied;
                summary.ranges_lost += lost as u64;
            }
            Event::Scrubbed {
                checked,
                mismatched,
                repaired,
            } => {
                summary.scrubs += 1;
                summary.blocks_checked += checked;
                summary.blocks_mismatched += mismatched as u64;
                summary.parity_rebuilt += repaired;
            }
            Event::BitRotRepaired { bytes, .. } => {
                summary.bit_rot_repaired += 1;
                summary.bytes_repaired += bytes;
            }
            Event::RepairRefused { .. } => summary.repairs_refused += 1,
            Event::Rebuilt { .. } => summary.rebuilds += 1,
            Event::Replaced { .. } => summary.replacements += 1,
            Event::Degraded { .. } => {}
        }
    }
    summary
}

/// Der Bericht, den man nach einem Jahr vorzeigt.
pub fn render(summary: &Summary) -> String {
    if summary.entries == 0 {
        return "Das Tagebuch ist leer.\n".to_string();
    }

    let mut text = String::new();
    let (first, last) = summary.span.unwrap_or((0, 0));
    let _ = writeln!(
        text,
        "{} bis {} — {} Stunden, {} Eintraege",
        iso8601(first),
        iso8601(last),
        summary.hours(),
        summary.entries
    );
    let _ = writeln!(text, "  {:<28} {}", "Inbetriebnahmen", summary.starts);
    let _ = writeln!(
        text,
        "  {:<28} {} ({} Writes zurueckgespielt)",
        "Recovery-Laeufe", summary.recoveries, summary.writes_recovered
    );
    let _ = writeln!(
        text,
        "  {:<28} {} ({} Bloecke geprueft)",
        "Scrubs", summary.scrubs, summary.blocks_checked
    );
    let _ = writeln!(
        text,
        "  {:<28} {} ({} Paritaetsbloecke neu gebildet)",
        "davon mit Abweichung", summary.blocks_mismatched, summary.parity_rebuilt
    );
    let _ = writeln!(
        text,
        "  {:<28} {} ({} Bytes)",
        "Bit-Rot repariert", summary.bit_rot_repaired, summary.bytes_repaired
    );
    let _ = writeln!(text, "  {:<28} {}", "Plattenwechsel", summary.replacements);
    let _ = writeln!(text, "  {:<28} {}", "Rebuilds", summary.rebuilds);

    // Die beiden Zahlen, um die es eigentlich geht. Sie stehen zuletzt und
    // einzeln, weil sie das Ergebnis sind und nicht eine Zeile unter vielen.
    let _ = writeln!(text);
    let _ = writeln!(
        text,
        "  {:<28} {}",
        "Bereiche verloren", summary.ranges_lost
    );
    let _ = writeln!(
        text,
        "  {:<28} {}",
        "Reparaturen abgelehnt", summary.repairs_refused
    );

    if summary.unreadable > 0 {
        let _ = writeln!(text, "\n{} Zeilen waren nicht lesbar.", summary.unreadable);
    }
    text
}

// --- Zeit -----------------------------------------------------------------

/// Sekunden seit 1970 als `2026-09-06T21:04:11Z`.
///
/// Von Hand und ohne Bibliothek: Das Umrechnen ist dreissig Zeilen, und ein
/// nackter Zeitstempel in einem Tagebuch, das man ein Jahr spaeter liest, ist
/// unbrauchbar.
pub fn iso8601(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let rest = unix.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    )
}

/// Die Umkehrung, so weit sie fuer das Wiederlesen gebraucht wird.
fn from_iso8601(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 20 || bytes[4] != b'-' || bytes[10] != b'T' || bytes[19] != b'Z' {
        return None;
    }
    let number = |from: usize, to: usize| text.get(from..to)?.parse::<i64>().ok();
    let year = number(0, 4)?;
    let month = number(5, 7)?;
    let day = number(8, 10)?;
    let hour = number(11, 13)?;
    let minute = number(14, 16)?;
    let second = number(17, 19)?;

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Howard Hinnants Algorithmus, in beide Richtungen.
///
/// Er ist bekannt, kurz und gegen bekannte Daten pruefbar — deshalb steht er
/// hier statt einer Dependency, die Zeitzonen, Schaltsekunden und ein
/// Formatierungsminisprache mitbraechte, von denen nichts gebraucht wird.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_dates_come_out_right() {
        // Gegen Daten, die sich von Hand nachrechnen lassen.
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1), "1970-01-01T00:00:01Z");
        assert_eq!(iso8601(86_399), "1970-01-01T23:59:59Z");
        assert_eq!(iso8601(86_400), "1970-01-02T00:00:00Z");
        assert_eq!(iso8601(1_000_000_000), "2001-09-09T01:46:40Z");
        assert_eq!(iso8601(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn a_leap_day_is_a_day() {
        // 2024 ist ein Schaltjahr, 2100 keines. Wer das falsch rechnet,
        // verschiebt alle Zeitstempel danach.
        assert_eq!(iso8601(1_709_164_800), "2024-02-29T00:00:00Z");
        assert_eq!(iso8601(4_107_542_400), "2100-03-01T00:00:00Z");
    }

    #[test]
    fn a_timestamp_survives_the_round_trip() {
        for unix in [0, 1, 86_400, 1_000_000_000, 1_700_000_000, 2_000_000_000] {
            assert_eq!(from_iso8601(&iso8601(unix)), Some(unix), "bei {unix}");
        }
    }

    #[test]
    fn nonsense_as_a_timestamp_is_refused() {
        assert_eq!(from_iso8601(""), None);
        assert_eq!(from_iso8601("2026-09-06"), None);
        assert_eq!(from_iso8601("2026-13-06T00:00:00Z"), None);
        assert_eq!(from_iso8601("2026-09-32T00:00:00Z"), None);
        assert_eq!(from_iso8601("gestern-um-drei-ZZZZ"), None);
    }

    // --- Zeilen ----------------------------------------------------------

    fn all_events() -> Vec<Event> {
        vec![
            Event::Started { members: 6 },
            Event::Stopped,
            Event::Recovered {
                applied: 14,
                lost: 0,
            },
            Event::Recovered {
                applied: 3,
                lost: 2,
            },
            Event::Scrubbed {
                checked: 1024,
                mismatched: 0,
                repaired: 0,
            },
            Event::Scrubbed {
                checked: 1024,
                mismatched: 3,
                repaired: 3,
            },
            Event::BitRotRepaired {
                slot: 1,
                bytes: 4096,
            },
            Event::RepairRefused { slot: 2 },
            Event::Rebuilt {
                slot: 0,
                blocks: 512,
            },
            Event::Replaced { slot: 0 },
            Event::Degraded { slot: 3 },
        ]
    }

    #[test]
    fn every_event_survives_the_round_trip() {
        // Ein Ereignis, das sich schreiben, aber nicht wieder lesen laesst,
        // faellt aus jeder Auswertung heraus — und die Auswertung ist der
        // ganze Zweck.
        for event in all_events() {
            let line = format(1_700_000_000, &event);
            let back = parse(&line).unwrap_or_else(|| panic!("nicht lesbar: {line}"));
            assert_eq!(back.event, event);
            assert_eq!(back.at, 1_700_000_000);
        }
    }

    #[test]
    fn every_event_has_a_sentence_for_a_human() {
        for event in all_events() {
            let headline = event.headline();
            assert!(!headline.is_empty());
            assert!(
                !headline.contains('{'),
                "unersetzter Platzhalter: {headline}"
            );
        }
    }

    #[test]
    fn a_line_is_greppable() {
        let line = format(
            1_700_000_000,
            &Event::Scrubbed {
                checked: 1024,
                mismatched: 3,
                repaired: 3,
            },
        );
        assert_eq!(
            line,
            "2023-11-14T22:13:20Z scrub warnung checked=1024 mismatched=3 repaired=3\n"
        );
    }

    #[test]
    fn a_half_written_line_is_skipped_and_counted() {
        // Nach einem Stromausfall steht so etwas in der letzten Zeile. Ein
        // Tagebuch, das sich deswegen nicht mehr auswerten laesst, verliert
        // genau das, wofuer es angelegt wurde.
        let text = "2023-11-14T22:13:20Z stop info\n2023-11-14T22:13:2";
        let summary = summarize(text);
        assert_eq!(summary.entries, 1);
        assert_eq!(summary.unreadable, 1);
    }

    #[test]
    fn a_line_from_a_newer_version_is_skipped_and_not_guessed() {
        let text = "2023-11-14T22:13:20Z etwas-neues info feld=1\n";
        assert_eq!(summarize(text).unreadable, 1);
    }

    // --- Dringlichkeit ----------------------------------------------------

    #[test]
    fn only_the_two_that_matter_are_alerts() {
        // Verlorene Daten und eine Reparatur, die nicht eindeutig ist. Alles
        // andere weckt niemanden — ein Alarm, der jede Woche kommt, wird
        // ignoriert, und dann auch der, auf den es ankam.
        assert_eq!(
            Event::Recovered {
                applied: 3,
                lost: 2
            }
            .severity(),
            Severity::Alert
        );
        assert_eq!(Event::RepairRefused { slot: 0 }.severity(), Severity::Alert);

        for event in [
            Event::Started { members: 6 },
            Event::Stopped,
            Event::Recovered {
                applied: 3,
                lost: 0,
            },
            Event::Scrubbed {
                checked: 1,
                mismatched: 0,
                repaired: 0,
            },
            Event::Rebuilt { slot: 0, blocks: 1 },
            Event::Replaced { slot: 0 },
        ] {
            assert!(
                event.severity() < Severity::Alert,
                "{event:?} sollte kein Alarm sein"
            );
        }
    }

    #[test]
    fn a_scrub_with_a_finding_is_a_warning_and_a_clean_one_is_not() {
        assert_eq!(
            Event::Scrubbed {
                checked: 10,
                mismatched: 0,
                repaired: 0
            }
            .severity(),
            Severity::Info
        );
        assert_eq!(
            Event::Scrubbed {
                checked: 10,
                mismatched: 1,
                repaired: 0
            }
            .severity(),
            Severity::Warning
        );
    }

    // --- Auswertung -------------------------------------------------------

    /// Ein Jahr Betrieb, in Millisekunden nachgespielt.
    fn a_year() -> String {
        let mut text = String::new();
        let start = 1_700_000_000;
        let month = 30 * 86_400;

        for index in 0..12 {
            let at = start + index * month;
            text.push_str(&format(at, &Event::Started { members: 6 }));
            text.push_str(&format(
                at + 60,
                &Event::Scrubbed {
                    checked: 1_000_000,
                    mismatched: 0,
                    repaired: 0,
                },
            ));
            text.push_str(&format(at + month - 60, &Event::Stopped));
        }
        // Drei reparierte Bit-Rot-Bloecke, eine unsaubere Abschaltung.
        text.push_str(&format(
            start + 3 * month,
            &Event::BitRotRepaired {
                slot: 1,
                bytes: 4096,
            },
        ));
        text.push_str(&format(
            start + 5 * month,
            &Event::BitRotRepaired {
                slot: 2,
                bytes: 8192,
            },
        ));
        text.push_str(&format(
            start + 9 * month,
            &Event::BitRotRepaired {
                slot: 0,
                bytes: 4096,
            },
        ));
        text.push_str(&format(
            start + 7 * month,
            &Event::Recovered {
                applied: 14,
                lost: 0,
            },
        ));
        text
    }

    #[test]
    fn a_year_of_operation_adds_up() {
        let summary = summarize(&a_year());

        assert_eq!(summary.starts, 12);
        assert_eq!(summary.scrubs, 12);
        assert_eq!(summary.blocks_checked, 12_000_000);
        assert_eq!(summary.bit_rot_repaired, 3);
        assert_eq!(summary.bytes_repaired, 4096 + 8192 + 4096);
        assert_eq!(summary.recoveries, 1);
        assert_eq!(summary.writes_recovered, 14);

        // Die beiden Zahlen, um die es geht.
        assert_eq!(summary.ranges_lost, 0);
        assert_eq!(summary.repairs_refused, 0);
        assert!(!summary.needs_attention());

        // Zwoelf Monate zu dreissig Tagen.
        assert_eq!(summary.hours(), 12 * 30 * 24 - 1);
    }

    #[test]
    fn the_report_names_the_numbers_that_matter() {
        let text = render(&summarize(&a_year()));
        for wanted in [
            "Scrubs",
            "Bit-Rot repariert",
            "Bereiche verloren",
            "Reparaturen abgelehnt",
            "Stunden",
        ] {
            assert!(text.contains(wanted), "{wanted} fehlt:\n{text}");
        }
    }

    #[test]
    fn an_empty_journal_says_so_instead_of_showing_zeros() {
        assert_eq!(render(&summarize("")), "Das Tagebuch ist leer.\n");
    }

    #[test]
    fn a_lost_range_shows_up_as_something_to_look_at() {
        let text = format(
            1_700_000_000,
            &Event::Recovered {
                applied: 3,
                lost: 2,
            },
        );
        let summary = summarize(&text);
        assert_eq!(summary.ranges_lost, 2);
        assert_eq!(summary.alerts, 1);
        assert!(summary.needs_attention());
    }
}

// --- Schreiben und melden -------------------------------------------------

/// Schreibt ein Ereignis ins Tagebuch und meldet es, wenn es dringend ist.
///
/// # Warum beides an einer Stelle
///
/// Sonst gaebe es zwei Listen davon, was wichtig ist, und sie liefen
/// auseinander. Aufgeschrieben wird alles; gemeldet wird ab
/// [`Severity::Warning`] — die Schwelle steht in [`Event::severity`] und
/// nirgends sonst.
///
/// # Warum ein Fehler hier nichts abbricht
///
/// Ein volles Dateisystem oder ein kaputtes Meldeprogramm darf keinen Scrub
/// beenden und kein Array stoppen. Der Fehler wird deshalb **zurueckgegeben**
/// und nicht verschluckt — was der Aufrufer damit macht, entscheidet er; im
/// Werkzeug wird er ausgegeben und der Vorgang laeuft weiter.
#[cfg(unix)]
pub fn record(config: &crate::config::Config, event: &Event) -> Result<(), RecordError> {
    let at = now_unix();
    if let Some(path) = &config.journal {
        append(path, &format(at, event))?;
    }
    if event.severity() >= Severity::Warning {
        if let Some(command) = &config.notify {
            notify(command, event)?;
        }
    }
    Ok(())
}

/// Warum ein Eintrag nicht geschrieben oder nicht gemeldet werden konnte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordError {
    Journal {
        path: std::path::PathBuf,
        kind: std::io::ErrorKind,
    },
    Notify {
        command: String,
        reason: String,
    },
}

impl std::fmt::Display for RecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Journal { path, kind } => {
                write!(f, "Tagebuch {}: {kind:?}", path.display())
            }
            Self::Notify { command, reason } => write!(f, "Meldung ueber {command}: {reason}"),
        }
    }
}

impl std::error::Error for RecordError {}

#[cfg(unix)]
fn append(path: &std::path::Path, line: &str) -> Result<(), RecordError> {
    use std::io::Write;

    let fail = |kind| RecordError::Journal {
        path: path.to_path_buf(),
        kind,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| fail(error.kind()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| fail(error.kind()))?;

    // Eine Zeile, ein `write`. Der Kernel schreibt einen `O_APPEND`-Write
    // unter der Groesse eines Blocks am Stueck ans Ende — zwei Prozesse, die
    // gleichzeitig schreiben, verschraenken sich dann nicht mitten in einer
    // Zeile. Zwei `write` je Zeile haetten diese Zusage nicht.
    file.write_all(line.as_bytes())
        .map_err(|error| fail(error.kind()))?;
    // Ohne Flush stuende der Eintrag ueber den Stromausfall, den er beschreibt,
    // moeglicherweise nicht auf der Platte.
    file.sync_data().map_err(|error| fail(error.kind()))
}

/// Ruft das Meldeprogramm auf: Betreff als Argument, Einzelheiten auf der
/// Standardeingabe.
///
/// Ein Programm und keine eingebaute E-Mail: Wer eine Nachricht auf sein
/// Telefon will, schreibt drei Zeilen Shell. Wer SMTP einbaut, schleppt eine
/// Bibliothek mit und trifft trotzdem nie den Geschmack des naechsten
/// Betreibers.
///
/// Die Zeichenkette aus der Konfiguration wird an Leerzeichen zerlegt: Das
/// erste Wort ist das Programm, der Rest sind feste Argumente. Keine Shell
/// dazwischen — sonst waere ein Dateiname mit einem Semikolon darin ein Weg,
/// fremde Befehle auszufuehren.
#[cfg(unix)]
fn notify(command: &str, event: &Event) -> Result<(), RecordError> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut words = command.split_whitespace();
    let program = words.next().ok_or_else(|| RecordError::Notify {
        command: command.to_string(),
        reason: "leer".to_string(),
    })?;

    let fail = |reason: String| RecordError::Notify {
        command: command.to_string(),
        reason,
    };

    let mut child = Command::new(program)
        .args(words)
        .arg(event.headline())
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| fail(error.to_string()))?;

    if let Some(mut stdin) = child.stdin.take() {
        let body = format!(
            "{}\n\nDringlichkeit: {}\nZeit: {}\n",
            event.headline(),
            event.severity().as_str(),
            iso8601(now_unix())
        );
        // Ein Meldeprogramm, das seine Eingabe nicht liest, ist kein Grund
        // zu scheitern — die Meldung selbst steht schon im Betreff.
        let _ = stdin.write_all(body.as_bytes());
    }

    let status = child.wait().map_err(|error| fail(error.to_string()))?;
    if !status.success() {
        return Err(fail(format!("endete mit {status}")));
    }
    Ok(())
}

#[cfg(unix)]
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0)
}
