// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Die Konfigurationsdatei.
//!
//! # Was **nicht** darin steht
//!
//! Die Geraeteliste. Welche Platte welche Rolle traegt, steht in ihrem
//! Superblock — eine zweite Liste daneben waere ein zweiter Ort fuer denselben
//! Zustand, und dort weichen sie eines Tages voneinander ab. Steckt jemand die
//! Platten um, findet Ferrite sie trotzdem; steht die Zuordnung in einer
//! Datei, findet er die falschen.
//!
//! Das ist zugleich der Unterschied zu Unraid, den das README als „Superbloecke
//! statt Config im USB-Flash" beschreibt. Diese Datei sagt nur, **wo gesucht
//! wird** und **was mit dem Ergebnis geschehen soll**.
//!
//! # Warum ein eigener Parser
//!
//! Dieselbe Ueberlegung wie bei der Kommandozeile. Das Format ist
//! `schluessel = wert`, eine Zeile je Angabe, `#` leitet einen Kommentar ein.
//! Dafuer eine Serialisierungsbibliothek samt ihrem Abhaengigkeitsbaum
//! einzutragen, waere ein schlechtes Geschaeft — zumal die Fehlermeldungen
//! dann von ihr kaemen und nicht von hier.

use std::path::PathBuf;

use crate::args::DEFAULT_STATE_DIR;

/// Der uebliche Ort.
pub const DEFAULT_PATH: &str = "/etc/ferrite/ferrite.conf";

/// Wo gesucht wird, wenn nichts anderes dasteht.
///
/// `/dev/disk/by-id` und nicht `/dev`: Dort stehen stabile Namen, die einen
/// Neustart und einen Steckplatzwechsel ueberleben, und es liegen keine
/// Nicht-Blockgeraete herum, die ein Oeffnen haengen lassen koennten.
pub const DEFAULT_SCAN: &str = "/dev/disk/by-id";

/// Die Konfiguration eines Ferrite-Dienstes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Welches Array. `None` heisst: das einzige, das gefunden wird.
    pub array: Option<String>,
    /// Verzeichnisse, in denen nach Members gesucht wird.
    pub scan: Vec<PathBuf>,
    /// Wo der vereinigte Pool eingehaengt wird. `None` heisst: nur die
    /// Blockgeraete bereitstellen.
    pub pool: Option<PathBuf>,
    pub state_dir: PathBuf,
    pub fstype: String,
    /// Wonach ein neues Objekt platziert wird.
    pub allocation: String,
    /// Bis zu welcher Tiefe ein Verzeichnis verteilt sein darf. `None` heisst
    /// unbeschraenkt.
    pub split: Option<u32>,
    pub min_free: u64,
    /// Was geschieht, wenn die Split-Regel auf eine volle Platte zeigt.
    pub overflow: String,
    /// Programm, das bei einem Befund aufgerufen wird.
    ///
    /// Ein Programm und keine eingebaute E-Mail: Wer eine Nachricht auf sein
    /// Telefon will, schreibt drei Zeilen Shell. Wer SMTP einbaut, schleppt
    /// eine Bibliothek mit und trifft trotzdem nie den Geschmack des naechsten
    /// Betreibers.
    pub notify: Option<PathBuf>,
    /// Wohin das Betriebstagebuch geschrieben wird.
    pub journal: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            array: None,
            scan: vec![PathBuf::from(DEFAULT_SCAN)],
            pool: None,
            state_dir: PathBuf::from(DEFAULT_STATE_DIR),
            fstype: "btrfs".to_string(),
            allocation: "most-free".to_string(),
            split: None,
            min_free: 0,
            overflow: "spill".to_string(),
            notify: None,
            journal: None,
        }
    }
}

/// Warum eine Konfigurationsdatei nicht benutzbar ist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    /// Zeilennummer, bei 1 beginnend — damit ein Editor direkt hinspringt.
    pub line: usize,
    pub reason: String,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Zeile {}: {}", self.line, self.reason)
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// Liest die Konfiguration aus dem Text einer Datei.
    ///
    /// Unbekannte Schluessel sind ein **Fehler** und keine Warnung. Ein
    /// vertippter Schluessel, der stillschweigend ignoriert wird, laesst den
    /// Betreiber glauben, seine Einstellung gelte — und die faellt ihm erst
    /// auf, wenn es darauf ankommt.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let mut config = Config::default();
        let mut scan_seen = false;

        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let content = raw.split('#').next().unwrap_or("").trim();
            if content.is_empty() {
                continue;
            }

            let Some((key, value)) = content.split_once('=') else {
                return Err(ConfigError {
                    line,
                    reason: format!("kein `schluessel = wert`: {content}"),
                });
            };
            let key = key.trim();
            let value = value.trim();
            if value.is_empty() {
                return Err(ConfigError {
                    line,
                    reason: format!("{key} ohne Wert"),
                });
            }

            match key {
                "array" => config.array = Some(value.to_string()),
                // Mehrfach erlaubt und **ersetzt** die Voreinstellung: Wer
                // einen eigenen Suchpfad angibt, will meist nicht zusaetzlich
                // in `/dev/disk/by-id` suchen.
                "scan" => {
                    if !scan_seen {
                        config.scan.clear();
                        scan_seen = true;
                    }
                    config.scan.push(PathBuf::from(value));
                }
                "pool" => config.pool = Some(PathBuf::from(value)),
                "state-dir" => config.state_dir = PathBuf::from(value),
                "fstype" => config.fstype = value.to_string(),
                "allocation" => {
                    config.allocation =
                        check_one_of(line, key, value, &["most-free", "fill-up", "round-robin"])?
                }
                "split" => {
                    config.split = if value == "anywhere" {
                        None
                    } else {
                        Some(value.parse::<u32>().map_err(|_| ConfigError {
                            line,
                            reason: format!(
                                "split braucht eine Zahl oder `anywhere`, nicht {value}"
                            ),
                        })?)
                    }
                }
                "min-free" => {
                    config.min_free = parse_size(value).ok_or_else(|| ConfigError {
                        line,
                        reason: format!("min-free braucht eine Groesse wie 20G, nicht {value}"),
                    })?
                }
                "overflow" => config.overflow = check_one_of(line, key, value, &["spill", "fail"])?,
                "notify" => config.notify = Some(PathBuf::from(value)),
                "journal" => config.journal = Some(PathBuf::from(value)),
                other => {
                    return Err(ConfigError {
                        line,
                        reason: format!("unbekannter Schluessel: {other}"),
                    })
                }
            }
        }
        Ok(config)
    }
}

fn check_one_of(
    line: usize,
    key: &str,
    value: &str,
    allowed: &[&str],
) -> Result<String, ConfigError> {
    if allowed.contains(&value) {
        return Ok(value.to_string());
    }
    Err(ConfigError {
        line,
        reason: format!(
            "{key} kennt {value} nicht — erlaubt: {}",
            allowed.join(", ")
        ),
    })
}

/// `20G`, `512M`, `1T` — oder eine nackte Zahl in Bytes.
///
/// Binaerpraefixe, wie ueberall im Projekt: Der Superblock rechnet in Bytes,
/// `statvfs` in Bloecken, und beide sind binaer. Wer hier dezimal rechnete,
/// bekaeme eine Reserve, die um sieben Prozent daneben liegt.
fn parse_size(value: &str) -> Option<u64> {
    let (digits, factor) = match value.as_bytes().last()? {
        b'K' | b'k' => (&value[..value.len() - 1], 1u64 << 10),
        b'M' | b'm' => (&value[..value.len() - 1], 1 << 20),
        b'G' | b'g' => (&value[..value.len() - 1], 1 << 30),
        b'T' | b't' => (&value[..value.len() - 1], 1 << 40),
        _ => (value, 1),
    };
    digits.parse::<u64>().ok()?.checked_mul(factor)
}

impl Config {
    /// Die Platzierungsregeln, wie `pool` sie braucht.
    ///
    /// Die Umwandlung steht hier und nicht im Pool: Der kennt keine
    /// Konfigurationsdatei, und die Namen in ihr sind eine Sache dieses
    /// Werkzeugs. Faellt in `pool` ein Massstab weg, bricht dieser Code — und
    /// das ist richtig so.
    pub fn share_policy(&self) -> ferrite_pool::SharePolicy {
        use ferrite_pool::{Allocation, SharePolicy, SplitDepth, SplitOverflow};

        SharePolicy {
            allocation: match self.allocation.as_str() {
                "fill-up" => Allocation::FillUp,
                "round-robin" => Allocation::RoundRobin,
                // `parse` laesst nichts anderes durch; hier steht der
                // Normalfall und keine stille Notbremse.
                _ => Allocation::MostFree,
            },
            split: match self.split {
                Some(depth) => SplitDepth::UpTo(depth),
                None => SplitDepth::Anywhere,
            },
            overflow: match self.overflow.as_str() {
                "fail" => SplitOverflow::Fail,
                _ => SplitOverflow::Spill,
            },
            min_free: self.min_free,
            included: Vec::new(),
        }
    }
}

/// Ein Beispiel, das sich als Datei ablegen laesst.
///
/// Steht hier und nicht nur in der Dokumentation, damit `the_example_parses`
/// es prueft: Ein Beispiel, das nicht durchgeht, ist schlimmer als keines.
pub const EXAMPLE: &str = "\
# /etc/ferrite/ferrite.conf
#
# Welche Platte welche Rolle traegt, steht in ihrem Superblock und nicht hier.
# Diese Datei sagt nur, wo gesucht wird und was mit dem Ergebnis geschehen soll.

# Welches Array. Weglassen, wenn nur eines angeschlossen ist.
# array = 3898328e-a28c-41e4-9c5a-267701f28c12

# Wo nach Members gesucht wird. Mehrfach erlaubt.
scan = /dev/disk/by-id

# Wo der vereinigte Baum erscheint. Weglassen fuer nur die Blockgeraete.
pool = /mnt/pool
state-dir = /run/ferrite
fstype = btrfs

# Platzierung neuer Dateien.
allocation = most-free      # most-free | fill-up | round-robin
split = 1                   # Tiefe, oder `anywhere`
min-free = 20G              # was auf jeder Platte frei bleibt
overflow = spill            # spill | fail

# Wird bei einem Befund aufgerufen; bekommt den Bericht auf der
# Standardeingabe. Drei Zeilen Shell reichen fuer eine Nachricht aufs Telefon.
# notify = /usr/local/lib/ferrite/notify

# Betriebstagebuch: eine Zeile je Ereignis, zum Mitschreiben ueber Monate.
journal = /var/lib/ferrite/journal
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_gives_the_defaults() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
        assert_eq!(Config::parse("\n\n   \n").unwrap(), Config::default());
    }

    #[test]
    fn the_example_parses() {
        // Ein Beispiel, das nicht durchgeht, kostet den ersten Nutzer eine
        // Stunde und das Projekt sein Zutrauen.
        let config = Config::parse(EXAMPLE).expect("das Beispiel muss durchgehen");
        assert_eq!(config.pool, Some(PathBuf::from("/mnt/pool")));
        assert_eq!(config.split, Some(1));
        assert_eq!(config.min_free, 20 << 30);
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let config = Config::parse("# nur ein Kommentar\n\npool = /mnt/x # dahinter auch\n")
            .expect("gueltig");
        assert_eq!(config.pool, Some(PathBuf::from("/mnt/x")));
    }

    #[test]
    fn a_typo_in_a_key_is_an_error_and_not_a_shrug() {
        // Der wichtigste Fall: Ein stillschweigend ignorierter Schluessel
        // laesst den Betreiber glauben, seine Einstellung gelte.
        let error = Config::parse("poool = /mnt/x\n").expect_err("muss auffallen");
        assert_eq!(error.line, 1);
        assert!(error.reason.contains("poool"));
    }

    #[test]
    fn the_line_number_points_at_the_mistake() {
        let error = Config::parse("pool = /mnt/x\n\n# nichts\nfalsch = 1\n").expect_err("Fehler");
        assert_eq!(error.line, 4);
    }

    #[test]
    fn a_line_without_an_equals_sign_is_refused() {
        assert!(Config::parse("pool /mnt/x\n").is_err());
    }

    #[test]
    fn a_key_without_a_value_is_refused() {
        // Sonst haette `pool =` einen leeren Pfad zur Folge, und der Pool
        // laege im Arbeitsverzeichnis.
        let error = Config::parse("pool =\n").expect_err("Fehler");
        assert!(error.reason.contains("ohne Wert"));
    }

    #[test]
    fn a_scan_path_replaces_the_default_instead_of_adding_to_it() {
        let config = Config::parse("scan = /dev/mapper\n").unwrap();
        assert_eq!(config.scan, vec![PathBuf::from("/dev/mapper")]);
    }

    #[test]
    fn several_scan_paths_add_up() {
        let config = Config::parse("scan = /dev/mapper\nscan = /srv/images\n").unwrap();
        assert_eq!(
            config.scan,
            vec![PathBuf::from("/dev/mapper"), PathBuf::from("/srv/images")]
        );
    }

    #[test]
    fn sizes_are_read_with_their_unit() {
        assert_eq!(parse_size("0"), Some(0));
        assert_eq!(parse_size("4096"), Some(4096));
        assert_eq!(parse_size("512M"), Some(512 << 20));
        assert_eq!(parse_size("20G"), Some(20 << 30));
        assert_eq!(parse_size("2T"), Some(2u64 << 40));
    }

    #[test]
    fn nonsense_as_a_size_is_refused() {
        assert_eq!(parse_size(""), None);
        assert_eq!(parse_size("G"), None);
        assert_eq!(parse_size("-1G"), None);
        assert_eq!(parse_size("20GB"), None);
        assert!(Config::parse("min-free = viel\n").is_err());
    }

    #[test]
    fn an_unknown_allocation_is_named_together_with_the_allowed_ones() {
        let error = Config::parse("allocation = zufall\n").expect_err("Fehler");
        assert!(error.reason.contains("zufall"));
        assert!(
            error.reason.contains("most-free"),
            "die erlaubten Werte gehoeren in die Meldung: {}",
            error.reason
        );
    }

    #[test]
    fn every_allocation_the_pool_knows_is_accepted() {
        // Waechst `pool::Allocation`, faellt dieser Test — und erinnert daran,
        // dass die Konfiguration nachzuziehen ist.
        for value in ["most-free", "fill-up", "round-robin"] {
            assert!(Config::parse(&format!("allocation = {value}\n")).is_ok());
        }
    }

    #[test]
    fn split_understands_a_depth_and_the_word_for_none() {
        assert_eq!(Config::parse("split = 0\n").unwrap().split, Some(0));
        assert_eq!(Config::parse("split = 3\n").unwrap().split, Some(3));
        assert_eq!(Config::parse("split = anywhere\n").unwrap().split, None);
        assert!(Config::parse("split = tief\n").is_err());
    }

    #[test]
    fn overflow_takes_only_what_the_policy_knows() {
        assert!(Config::parse("overflow = spill\n").is_ok());
        assert!(Config::parse("overflow = fail\n").is_ok());
        assert!(Config::parse("overflow = vielleicht\n").is_err());
    }

    #[test]
    fn every_setting_reaches_the_pool() {
        use ferrite_pool::{Allocation, SplitDepth, SplitOverflow};

        let config =
            Config::parse("allocation = fill-up\nsplit = 2\noverflow = fail\nmin-free = 1G\n")
                .unwrap();
        let policy = config.share_policy();

        assert_eq!(policy.allocation, Allocation::FillUp);
        assert_eq!(policy.split, SplitDepth::UpTo(2));
        assert_eq!(policy.overflow, SplitOverflow::Fail);
        assert_eq!(policy.min_free, 1 << 30);
    }

    #[test]
    fn the_defaults_match_what_the_pool_would_choose_on_its_own() {
        // Sonst verhielte sich Ferrite mit einer leeren Konfigurationsdatei
        // anders als ohne eine.
        assert_eq!(
            Config::default().share_policy(),
            ferrite_pool::SharePolicy::default()
        );
    }

    #[test]
    fn split_anywhere_reaches_the_pool_as_such() {
        assert_eq!(
            Config::parse("split = anywhere\n")
                .unwrap()
                .share_policy()
                .split,
            ferrite_pool::SplitDepth::Anywhere
        );
    }

    #[test]
    fn a_value_may_contain_spaces_and_equals_signs() {
        // Pfade duerfen das. Getrennt wird beim **ersten** Gleichheitszeichen.
        let config = Config::parse("notify = /usr/lib/ferrite/melde --an=telefon\n").unwrap();
        assert_eq!(
            config.notify,
            Some(PathBuf::from("/usr/lib/ferrite/melde --an=telefon"))
        );
    }
}
