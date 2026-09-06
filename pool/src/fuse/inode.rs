// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Die Zuordnung von Nodeid zu Pfad.
//!
//! FUSE spricht ueber `nodeid`, nicht ueber Pfade: Der Kernel merkt sich zu
//! jedem Objekt eine Zahl und schickt sie zurueck. Dieser Server muss daraus
//! wieder einen Pfad machen.
//!
//! # Was der Kernel zusichert und was er verlangt
//!
//! Er zaehlt mit, wie oft er ein Objekt nachgeschlagen hat, und schickt
//! `FORGET`, wenn er es nicht mehr braucht. Solange der Zaehler nicht null
//! ist, **muss** dieselbe Nodeid denselben Pfad meinen — sonst greift ein
//! offener Dateideskriptor auf etwas anderes zu, als sein Besitzer geoeffnet
//! hat.
//!
//! # Warum Nodeids nie wiederverwendet werden
//!
//! Ein vergessenes Objekt gibt seine Zahl nicht zurueck; der Zaehler laeuft
//! weiter. Damit ist das Paar aus Nodeid und Generation ohne zweites Feld
//! eindeutig, und die haessliche Wettlaufsituation faellt weg, in der der
//! Kernel ein `FORGET` schickt, waehrend ein neues `LOOKUP` dieselbe Zahl
//! schon wieder vergeben hat. Bei 64 Bit und einer Milliarde Vergaben je
//! Sekunde reichte der Vorrat fuenfhundert Jahre.

use std::collections::HashMap;

/// Die Nodeid der Wurzel. Vom Protokoll vorgegeben.
pub const ROOT: u64 = 1;

#[derive(Debug)]
struct Entry {
    path: String,
    /// Wie oft der Kernel dieses Objekt nachgeschlagen hat.
    lookups: u64,
}

/// Nodeid ↔ Pfad, mit dem Zaehler des Kernels.
#[derive(Debug)]
pub struct InodeTable {
    by_id: HashMap<u64, Entry>,
    by_path: HashMap<String, u64>,
    next: u64,
}

impl Default for InodeTable {
    fn default() -> Self {
        Self::new()
    }
}

impl InodeTable {
    pub fn new() -> Self {
        let mut by_id = HashMap::new();
        by_id.insert(
            ROOT,
            Entry {
                path: String::new(),
                // Die Wurzel wird nie vergessen. Der Kernel schickt fuer sie
                // kein `FORGET`, und sie zu entfernen hiesse, den Pool
                // unbenutzbar zu machen.
                lookups: u64::MAX,
            },
        );
        let mut by_path = HashMap::new();
        by_path.insert(String::new(), ROOT);

        InodeTable {
            by_id,
            by_path,
            next: ROOT + 1,
        }
    }

    /// Der Pfad zu einer Nodeid.
    pub fn path_of(&self, id: u64) -> Option<&str> {
        self.by_id.get(&id).map(|entry| entry.path.as_str())
    }

    /// Meldet ein Nachschlagen an und liefert die Nodeid.
    ///
    /// Fuer denselben Pfad immer dieselbe Zahl, solange sie nicht vergessen
    /// wurde.
    pub fn lookup(&mut self, path: &str) -> u64 {
        if let Some(id) = self.by_path.get(path).copied() {
            if let Some(entry) = self.by_id.get_mut(&id) {
                entry.lookups = entry.lookups.saturating_add(1);
            }
            return id;
        }

        let id = self.next;
        self.next += 1;
        self.by_id.insert(
            id,
            Entry {
                path: path.to_string(),
                lookups: 1,
            },
        );
        self.by_path.insert(path.to_string(), id);
        id
    }

    /// Der Kernel braucht dieses Objekt `count` mal weniger.
    ///
    /// Faellt der Zaehler auf null, verschwindet der Eintrag. Die Nodeid wird
    /// dabei **nicht** frei.
    pub fn forget(&mut self, id: u64, count: u64) {
        if id == ROOT {
            return;
        }
        let Some(entry) = self.by_id.get_mut(&id) else {
            return;
        };
        entry.lookups = entry.lookups.saturating_sub(count);
        if entry.lookups == 0 {
            let path = entry.path.clone();
            self.by_id.remove(&id);
            // Nur entfernen, wenn der Pfad noch auf diese Nodeid zeigt: Nach
            // einem `rename` kann dort schon eine andere stehen.
            if self.by_path.get(&path) == Some(&id) {
                self.by_path.remove(&path);
            }
        }
    }

    /// Zieht einen Teilbaum auf einen neuen Pfad um.
    ///
    /// Ein `rename` verschiebt nicht nur das Objekt selbst, sondern alles
    /// darunter. Bleibt ein Kind auf dem alten Pfad stehen, zeigt seine
    /// Nodeid nach dem Umzug ins Leere — und der Kernel haelt sie noch.
    pub fn rename(&mut self, from: &str, to: &str) {
        let prefix = format!("{from}/");
        let affected: Vec<(String, u64)> = self
            .by_path
            .iter()
            .filter(|(path, _)| path.as_str() == from || path.starts_with(&prefix))
            .map(|(path, id)| (path.clone(), *id))
            .collect();

        for (path, id) in affected {
            let moved = if path == from {
                to.to_string()
            } else {
                format!("{to}/{}", &path[prefix.len()..])
            };
            self.by_path.remove(&path);
            if let Some(entry) = self.by_id.get_mut(&id) {
                entry.path = moved.clone();
            }
            // Ein Objekt, das am Ziel schon lag, wurde gerade ueberschrieben.
            // Seine Nodeid bleibt gueltig, bis der Kernel sie vergisst; sie
            // zeigt nur nicht mehr auf diesen Pfad.
            self.by_path.insert(moved, id);
        }
    }

    /// Wieviele Objekte der Kernel gerade haelt. Fuer Tests.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_root_is_there_from_the_start() {
        let table = InodeTable::new();
        assert_eq!(table.path_of(ROOT), Some(""));
    }

    #[test]
    fn the_same_path_always_gets_the_same_nodeid() {
        let mut table = InodeTable::new();
        let first = table.lookup("Filme/a.mkv");
        let second = table.lookup("Filme/a.mkv");
        assert_eq!(first, second);
        assert_eq!(table.path_of(first), Some("Filme/a.mkv"));
    }

    #[test]
    fn a_nodeid_survives_until_the_kernel_has_forgotten_every_lookup() {
        let mut table = InodeTable::new();
        let id = table.lookup("a");
        table.lookup("a");

        table.forget(id, 1);
        assert_eq!(
            table.path_of(id),
            Some("a"),
            "ein Lookup steht noch aus, die Nodeid muss gelten"
        );
        table.forget(id, 1);
        assert_eq!(table.path_of(id), None);
    }

    #[test]
    fn a_forgotten_nodeid_is_never_handed_out_again() {
        let mut table = InodeTable::new();
        let first = table.lookup("a");
        table.forget(first, 1);
        let second = table.lookup("b");
        assert_ne!(
            first, second,
            "eine wiederverwendete Zahl brauchte eine Generation, um eindeutig zu bleiben"
        );
    }

    #[test]
    fn the_root_is_never_forgotten() {
        let mut table = InodeTable::new();
        table.forget(ROOT, u64::MAX);
        assert_eq!(table.path_of(ROOT), Some(""));
    }

    #[test]
    fn forgetting_something_unknown_is_harmless() {
        let mut table = InodeTable::new();
        table.forget(4242, 1);
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn a_rename_takes_the_whole_subtree_along() {
        let mut table = InodeTable::new();
        let directory = table.lookup("alt");
        let child = table.lookup("alt/kind.txt");
        let deep = table.lookup("alt/tiefer/noch.txt");

        table.rename("alt", "neu");

        assert_eq!(table.path_of(directory), Some("neu"));
        assert_eq!(
            table.path_of(child),
            Some("neu/kind.txt"),
            "ein zurueckgebliebenes Kind zeigte ins Leere"
        );
        assert_eq!(table.path_of(deep), Some("neu/tiefer/noch.txt"));
    }

    #[test]
    fn a_rename_does_not_touch_a_name_that_merely_starts_the_same() {
        let mut table = InodeTable::new();
        let other = table.lookup("altbau");
        table.lookup("alt");
        table.rename("alt", "neu");
        assert_eq!(
            table.path_of(other),
            Some("altbau"),
            "der Praefixvergleich muss an der Trennstelle enden"
        );
    }

    #[test]
    fn a_lookup_after_a_rename_finds_the_moved_object() {
        let mut table = InodeTable::new();
        let id = table.lookup("alt");
        table.rename("alt", "neu");
        assert_eq!(table.lookup("neu"), id);
    }
}
