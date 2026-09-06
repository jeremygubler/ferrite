// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Pfade im Pool.
//!
//! Ein Pfad im Pool ist **relativ zur Wurzel des Shares**, mit `/` getrennt,
//! ohne fuehrenden und ohne abschliessenden Trenner. `Filme/2026/film.mkv` ist
//! einer, `/Filme` und `Filme/` sind keine.
//!
//! # Warum das hier geprueft wird und nicht spaeter
//!
//! Der Pool setzt aus diesem Pfad und dem Wurzelverzeichnis eines Branches
//! einen Pfad im Dateisystem zusammen. Enthaelt er `..`, zeigt das Ergebnis
//! aus dem Branch heraus — auf das Wurzeldateisystem des Servers. Ein FUSE-Pfad
//! kommt von einem Klienten und ist damit Eingabe, nicht Wahrheit.
//!
//! Der Kernel schickt ueber FUSE zwar Namen und keine Pfade, und `.` und `..`
//! loest er selbst auf. Darauf zu bauen hiesse, die Sicherheit dieses Crates
//! an eine Eigenschaft seines Aufrufers zu haengen. Geprueft wird hier.

use crate::error::{PoolError, Result};

/// Laengstmoegliche einzelne Namenskomponente.
///
/// `NAME_MAX` unter Linux. Ein laengerer Name wird vom Dateisystem ohnehin
/// abgelehnt — nur eben erst, nachdem der Pool ihn schon platziert hat.
pub const NAME_MAX: usize = 255;

/// Die Komponenten eines Pfades, ohne ihn zu pruefen.
///
/// Fuer Aufrufer, die bereits geprueft haben. Wer unsicher ist, nimmt
/// [`depth_of`] — das prueft und zaehlt in einem Zug.
pub fn components(path: &str) -> impl Iterator<Item = &str> {
    path.split('/')
}

/// Prueft einen Pfad und liefert seine Tiefe in Komponenten.
///
/// `"a"` hat Tiefe 1, `"a/b"` Tiefe 2. Der leere Pfad ist die Wurzel des
/// Shares und hat Tiefe 0 — er ist gueltig, denn ein Share hat eine Wurzel.
pub fn depth_of(path: &str) -> Result<u32> {
    if path.is_empty() {
        return Ok(0);
    }
    if path.starts_with('/') {
        return Err(PoolError::InvalidPath {
            reason: "absolut, erwartet wird ein Pfad relativ zur Share-Wurzel",
        });
    }
    if path.ends_with('/') {
        return Err(PoolError::InvalidPath {
            reason: "endet auf einem Trenner",
        });
    }

    let mut count = 0u32;
    for component in path.split('/') {
        if component.is_empty() {
            return Err(PoolError::InvalidPath {
                reason: "leere Komponente, also zwei Trenner hintereinander",
            });
        }
        if component == "." || component == ".." {
            return Err(PoolError::InvalidPath {
                reason: "'.' oder '..' — das zeigte aus dem Branch heraus",
            });
        }
        if component.len() > NAME_MAX {
            return Err(PoolError::InvalidPath {
                reason: "Komponente laenger als NAME_MAX",
            });
        }
        if component.contains('\0') {
            return Err(PoolError::InvalidPath {
                reason: "Nullbyte im Namen",
            });
        }
        count = count.checked_add(1).ok_or(PoolError::InvalidPath {
            reason: "mehr Komponenten als zaehlbar",
        })?;
    }
    Ok(count)
}

/// Der Vorfahre der angegebenen Tiefe, als Ausschnitt des Pfades.
///
/// `ancestor_at("a/b/c", 2)` ist `Some("a/b")`, `ancestor_at("a/b/c", 0)` ist
/// `Some("")` — die Wurzel des Shares. Ist der Pfad kuerzer als die gesuchte
/// Tiefe, kommt `None`: Es gibt diesen Vorfahren nicht.
///
/// Gebraucht wird das fuer die Split-Regel. Sie sagt, bis zu welcher Tiefe ein
/// Verzeichnis ueber mehrere Branches verteilt sein darf; darunter richtet sich
/// alles nach dem Branch, der genau diesen Vorfahren traegt.
pub fn ancestor_at(path: &str, depth: u32) -> Option<&str> {
    if depth == 0 {
        return Some("");
    }
    if path.is_empty() {
        return None;
    }

    let mut seen = 0u32;
    for (index, character) in path.char_indices() {
        if character == '/' {
            seen += 1;
            if seen == depth {
                return Some(&path[..index]);
            }
        }
    }
    // Kein Trenner mehr gefunden: Der ganze Pfad ist der Vorfahre, wenn seine
    // Tiefe genau passt.
    (seen + 1 == depth).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_counts_components() {
        assert_eq!(depth_of("").unwrap(), 0);
        assert_eq!(depth_of("a").unwrap(), 1);
        assert_eq!(depth_of("a/b").unwrap(), 2);
        assert_eq!(depth_of("Filme/2026/film.mkv").unwrap(), 3);
    }

    #[test]
    fn a_path_that_climbs_out_of_the_branch_is_refused() {
        for path in ["..", "../x", "a/../../etc/shadow", "a/b/.."] {
            assert!(
                depth_of(path).is_err(),
                "{path} zeigt aus dem Branch heraus"
            );
        }
    }

    #[test]
    fn a_single_dot_is_refused_too() {
        // Harmlos gemeint, aber der Pool baut daraus einen Pfad, und ein
        // Vorfahre namens "." macht die Split-Regel unberechenbar.
        assert!(depth_of("./a").is_err());
        assert!(depth_of("a/./b").is_err());
    }

    #[test]
    fn an_absolute_path_is_refused() {
        assert_eq!(
            depth_of("/etc/shadow"),
            Err(PoolError::InvalidPath {
                reason: "absolut, erwartet wird ein Pfad relativ zur Share-Wurzel",
            })
        );
    }

    #[test]
    fn separators_that_come_in_pairs_are_refused() {
        assert!(depth_of("a//b").is_err());
        assert!(depth_of("a/").is_err());
    }

    #[test]
    fn a_nul_byte_is_refused() {
        assert!(depth_of("a\0b").is_err());
    }

    #[test]
    fn a_component_longer_than_name_max_is_refused() {
        let long = "x".repeat(NAME_MAX);
        assert_eq!(depth_of(&long).unwrap(), 1);
        let too_long = "x".repeat(NAME_MAX + 1);
        assert!(depth_of(&too_long).is_err());
    }

    #[test]
    fn a_name_that_only_looks_like_a_climb_is_fine() {
        // `..foo` und `foo..` sind gewoehnliche Namen. Wer auf `contains("..")`
        // prueft statt auf die ganze Komponente, verbietet sie ohne Grund.
        assert_eq!(depth_of("..foo/bar..").unwrap(), 2);
        assert_eq!(depth_of("...").unwrap(), 1);
    }

    #[test]
    fn the_ancestor_at_a_depth_is_a_prefix_of_the_path() {
        assert_eq!(ancestor_at("a/b/c", 0), Some(""));
        assert_eq!(ancestor_at("a/b/c", 1), Some("a"));
        assert_eq!(ancestor_at("a/b/c", 2), Some("a/b"));
        assert_eq!(ancestor_at("a/b/c", 3), Some("a/b/c"));
    }

    #[test]
    fn there_is_no_ancestor_deeper_than_the_path() {
        assert_eq!(ancestor_at("a/b", 3), None);
        assert_eq!(ancestor_at("", 1), None);
    }

    #[test]
    fn multibyte_names_do_not_split_a_slice() {
        // Wer hier mit Byte-Offsets statt mit Zeichengrenzen rechnet, paniert.
        assert_eq!(ancestor_at("Bücher/Größe/x", 2), Some("Bücher/Größe"));
        assert_eq!(depth_of("Bücher/Größe/x").unwrap(), 3);
    }
}
