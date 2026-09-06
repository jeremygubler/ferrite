// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Die Vereinigung: was ein Name bedeutet, der auf mehreren Branches vorkommt.
//!
//! # Die drei Faelle
//!
//! * **Ein Branch traegt ihn.** Der Normalfall. Der Pool zeigt ihn.
//! * **Mehrere Branches tragen ein Verzeichnis desselben Namens.** Auch
//!   normal, und der Grund, warum es einen Pool gibt: Die Eintraege werden
//!   vereinigt.
//! * **Mehrere Branches tragen etwas, das kein Verzeichnis ist.** Das ist ein
//!   Konflikt, und er wird gemeldet.
//!
//! # Warum ein doppelter Name ein Konflikt ist und keine Auswahl
//!
//! Unraid loest ihn ueber die Plattenreihenfolge auf: Die Datei auf der
//! niedrigsten Platte gewinnt, die andere ist unsichtbar. Loescht man die
//! sichtbare, taucht die andere auf — mit anderem Inhalt und anderem Datum.
//! Das ist eine verlaessliche Quelle von Verwirrung und von scheinbar
//! wiederauferstandenen Dateien.
//!
//! Ferrite **bedient** den Zugriff genauso deterministisch (siehe
//! [`Resolution::served_by`]) — ein Dateisystem muss antworten. Aber es nennt
//! die Lage beim Namen, statt sie zu verstecken: Der Konflikt steht in der
//! Auflistung, und die Control plane kann ihn zeigen. Sichtbar aufloesen kann
//! ihn nur ein Mensch, denn nur er weiss, welche der beiden Dateien er
//! behalten will.

use crate::branch::BranchId;

/// Was fuer ein Ding ein Eintrag ist.
///
/// Feiner als „Verzeichnis oder nicht" muss es fuer die Vereinigung nicht
/// sein: Verzeichnisse werden zusammengefasst, alles andere nicht.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    /// Geraetedatei, FIFO, Socket. Nichts, was der Pool zusammenfasst.
    Other,
}

impl EntryKind {
    pub fn is_directory(self) -> bool {
        self == EntryKind::Directory
    }
}

/// Ein Eintrag, wie ihn ein einzelner Branch fuehrt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BranchEntry {
    pub branch: BranchId,
    pub kind: EntryKind,
}

/// Warum ein Name nicht eindeutig ist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conflict {
    /// Derselbe Name traegt auf mehreren Branches etwas, das kein Verzeichnis
    /// ist — zwei Dateien, oder eine Datei und ein Symlink.
    Duplicate { entries: Vec<BranchEntry> },
    /// Auf dem einen ein Verzeichnis, auf dem anderen etwas anderes.
    ///
    /// Schlimmer als ein doppelter Dateiname: Ein Verzeichnis liesse sich
    /// betreten, eine Datei lesen, und welches von beidem der Name bedeutet,
    /// haengt sonst von der Plattenreihenfolge ab.
    KindMismatch { entries: Vec<BranchEntry> },
}

impl Conflict {
    pub fn entries(&self) -> &[BranchEntry] {
        match self {
            Conflict::Duplicate { entries } | Conflict::KindMismatch { entries } => entries,
        }
    }
}

/// Was ein Name im Pool bedeutet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Kein Branch traegt ihn.
    Missing,
    /// Genau ein Branch traegt ihn.
    Single { branch: BranchId, kind: EntryKind },
    /// Ein Verzeichnis auf mehreren Branches. Die Eintraege werden vereinigt.
    Directory { branches: Vec<BranchId> },
    /// Nicht eindeutig.
    Conflict(Conflict),
}

impl Resolution {
    /// Welcher Branch den Zugriff bedient.
    ///
    /// Auch im Konfliktfall gibt es eine Antwort, und sie ist immer dieselbe:
    /// der kleinste Slot-Index. Ein Dateisystem, das bei einem Konflikt einen
    /// Fehler liefert, macht den ganzen Ordner unbenutzbar; eines, das mal so
    /// und mal anders antwortet, ist schlimmer als beides.
    ///
    /// `None` nur fuer [`Resolution::Missing`].
    pub fn served_by(&self) -> Option<BranchId> {
        match self {
            Resolution::Missing => None,
            Resolution::Single { branch, .. } => Some(*branch),
            Resolution::Directory { branches } => branches.iter().min().copied(),
            Resolution::Conflict(conflict) => {
                conflict.entries().iter().map(|entry| entry.branch).min()
            }
        }
    }

    /// Als was der Pool den Namen zeigt.
    pub fn kind(&self) -> Option<EntryKind> {
        match self {
            Resolution::Missing => None,
            Resolution::Single { kind, .. } => Some(*kind),
            Resolution::Directory { .. } => Some(EntryKind::Directory),
            Resolution::Conflict(conflict) => conflict
                .entries()
                .iter()
                .min_by_key(|entry| entry.branch)
                .map(|entry| entry.kind),
        }
    }

    pub fn is_conflict(&self) -> bool {
        matches!(self, Resolution::Conflict(_))
    }
}

/// Loest einen Namen auf, der auf den angegebenen Branches vorkommt.
///
/// Die Reihenfolge der Eingabe ist gleichgueltig; das Ergebnis ist es nicht.
pub fn resolve(entries: &[BranchEntry]) -> Resolution {
    let mut entries: Vec<BranchEntry> = entries.to_vec();
    entries.sort_unstable_by_key(|entry| entry.branch);
    entries.dedup();

    match entries.len() {
        0 => Resolution::Missing,
        1 => Resolution::Single {
            branch: entries[0].branch,
            kind: entries[0].kind,
        },
        _ => {
            let directories = entries
                .iter()
                .filter(|entry| entry.kind.is_directory())
                .count();
            if directories == entries.len() {
                Resolution::Directory {
                    branches: entries.iter().map(|entry| entry.branch).collect(),
                }
            } else if directories == 0 {
                Resolution::Conflict(Conflict::Duplicate { entries })
            } else {
                Resolution::Conflict(Conflict::KindMismatch { entries })
            }
        }
    }
}

/// Ein Eintrag der vereinigten Auflistung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedEntry {
    pub name: String,
    pub resolution: Resolution,
}

/// Vereinigt die Verzeichnisauflistungen mehrerer Branches.
///
/// Die Eingabe ist, was der Aufrufer je Branch gelesen hat — dieses Crate
/// liest kein Verzeichnis. Zurueck kommt jede Name **einmal**, nach Namen
/// sortiert.
///
/// # Warum sortiert
///
/// `readdir` darf jede Reihenfolge liefern, aber sie muss ueber die Aufrufe
/// hinweg stabil sein: Der Kernel setzt eine Auflistung aus mehreren Aufrufen
/// zusammen und merkt sich einen Offset darin. Kaeme die Reihenfolge aus der
/// Reihenfolge der Branches, aenderte sie sich, sobald eine Platte hinzukommt
/// — mitten in einem laufenden `ls`.
pub fn merge_listing(listings: &[(BranchId, Vec<(String, EntryKind)>)]) -> Vec<MergedEntry> {
    let mut collected: Vec<(String, BranchEntry)> = Vec::new();
    for (branch, entries) in listings {
        for (name, kind) in entries {
            collected.push((
                name.clone(),
                BranchEntry {
                    branch: *branch,
                    kind: *kind,
                },
            ));
        }
    }
    collected.sort_unstable_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then(left.1.branch.cmp(&right.1.branch))
    });

    let mut merged: Vec<MergedEntry> = Vec::new();
    let mut index = 0;
    while index < collected.len() {
        let name = &collected[index].0;
        let mut end = index;
        while end < collected.len() && collected[end].0 == *name {
            end += 1;
        }
        let entries: Vec<BranchEntry> = collected[index..end]
            .iter()
            .map(|(_, entry)| *entry)
            .collect();
        merged.push(MergedEntry {
            name: name.clone(),
            resolution: resolve(&entries),
        });
        index = end;
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(branch: u16, kind: EntryKind) -> BranchEntry {
        BranchEntry {
            branch: BranchId(branch),
            kind,
        }
    }

    fn listing(branch: u16, entries: &[(&str, EntryKind)]) -> (BranchId, Vec<(String, EntryKind)>) {
        (
            BranchId(branch),
            entries
                .iter()
                .map(|(name, kind)| ((*name).to_string(), *kind))
                .collect(),
        )
    }

    // --- Aufloesung eines Namens ------------------------------------------

    #[test]
    fn a_name_nobody_carries_is_missing() {
        assert_eq!(resolve(&[]), Resolution::Missing);
        assert_eq!(resolve(&[]).served_by(), None);
    }

    #[test]
    fn a_name_on_one_branch_resolves_to_that_branch() {
        assert_eq!(
            resolve(&[entry(2, EntryKind::File)]),
            Resolution::Single {
                branch: BranchId(2),
                kind: EntryKind::File
            }
        );
    }

    #[test]
    fn a_directory_on_several_branches_is_the_union() {
        let resolution = resolve(&[
            entry(2, EntryKind::Directory),
            entry(0, EntryKind::Directory),
        ]);
        assert_eq!(
            resolution,
            Resolution::Directory {
                branches: vec![BranchId(0), BranchId(2)]
            },
            "die Eingabereihenfolge darf keine Rolle spielen"
        );
        assert_eq!(resolution.kind(), Some(EntryKind::Directory));
    }

    #[test]
    fn the_same_file_on_two_branches_is_a_conflict() {
        let resolution = resolve(&[entry(0, EntryKind::File), entry(1, EntryKind::File)]);
        assert!(resolution.is_conflict());
        assert!(matches!(
            resolution,
            Resolution::Conflict(Conflict::Duplicate { .. })
        ));
    }

    #[test]
    fn a_file_beside_a_directory_is_a_different_conflict() {
        // Die schaerfere Lage: Der eine Branch laedt zum Betreten ein, der
        // andere zum Lesen.
        let resolution = resolve(&[entry(0, EntryKind::Directory), entry(1, EntryKind::File)]);
        assert!(matches!(
            resolution,
            Resolution::Conflict(Conflict::KindMismatch { .. })
        ));
    }

    #[test]
    fn a_symlink_beside_a_file_is_a_duplicate_not_a_kind_mismatch() {
        // Beides ist kein Verzeichnis, also gibt es nichts zu vereinigen —
        // aber auch keinen Unterschied im Umgang damit.
        let resolution = resolve(&[entry(0, EntryKind::File), entry(1, EntryKind::Symlink)]);
        assert!(matches!(
            resolution,
            Resolution::Conflict(Conflict::Duplicate { .. })
        ));
    }

    #[test]
    fn a_conflict_is_always_served_by_the_same_branch() {
        // Ein Dateisystem muss antworten. Die Antwort darf nur nicht von der
        // Reihenfolge abhaengen, in der die Branches gelesen wurden.
        let forwards = resolve(&[entry(2, EntryKind::File), entry(1, EntryKind::File)]);
        let backwards = resolve(&[entry(1, EntryKind::File), entry(2, EntryKind::File)]);
        assert_eq!(forwards, backwards);
        assert_eq!(forwards.served_by(), Some(BranchId(1)));
    }

    #[test]
    fn a_kind_mismatch_is_served_as_what_the_lowest_branch_has() {
        let resolution = resolve(&[entry(1, EntryKind::Directory), entry(0, EntryKind::File)]);
        assert_eq!(resolution.served_by(), Some(BranchId(0)));
        assert_eq!(resolution.kind(), Some(EntryKind::File));
    }

    #[test]
    fn the_same_entry_reported_twice_is_not_a_conflict() {
        // Kann bei einem erneuten Lesen desselben Branches vorkommen. Zwei
        // identische Meldungen sind eine Tatsache, keine zwei Dateien.
        assert_eq!(
            resolve(&[entry(1, EntryKind::File), entry(1, EntryKind::File)]),
            Resolution::Single {
                branch: BranchId(1),
                kind: EntryKind::File
            }
        );
    }

    // --- Vereinigte Auflistung --------------------------------------------

    #[test]
    fn a_listing_shows_every_name_once() {
        let listings = [
            listing(
                0,
                &[
                    ("Filme", EntryKind::Directory),
                    ("liesmich.txt", EntryKind::File),
                ],
            ),
            listing(
                1,
                &[
                    ("Filme", EntryKind::Directory),
                    ("Musik", EntryKind::Directory),
                ],
            ),
        ];
        let merged = merge_listing(&listings);
        let names: Vec<&str> = merged.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["Filme", "Musik", "liesmich.txt"]);
    }

    #[test]
    fn the_listing_is_sorted_and_not_in_branch_order() {
        // Kaeme die Reihenfolge aus den Branches, aenderte ein Plattenwechsel
        // sie mitten in einem laufenden `ls`.
        let listings = [
            listing(0, &[("z", EntryKind::File)]),
            listing(1, &[("a", EntryKind::File)]),
        ];
        let names: Vec<String> = merge_listing(&listings)
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(names, ["a", "z"]);
    }

    #[test]
    fn the_listing_carries_the_conflict_along() {
        let listings = [
            listing(0, &[("gleich.txt", EntryKind::File)]),
            listing(1, &[("gleich.txt", EntryKind::File)]),
        ];
        let merged = merge_listing(&listings);
        assert_eq!(merged.len(), 1);
        assert!(
            merged[0].resolution.is_conflict(),
            "der Konflikt gehoert in die Auflistung, nicht in ein Log"
        );
        assert_eq!(merged[0].resolution.served_by(), Some(BranchId(0)));
    }

    #[test]
    fn a_directory_in_the_listing_names_all_its_branches() {
        let listings = [
            listing(0, &[("Filme", EntryKind::Directory)]),
            listing(1, &[("Filme", EntryKind::Directory)]),
            listing(2, &[("Filme", EntryKind::Directory)]),
        ];
        let merged = merge_listing(&listings);
        assert_eq!(
            merged[0].resolution,
            Resolution::Directory {
                branches: vec![BranchId(0), BranchId(1), BranchId(2)]
            },
            "wer das Verzeichnis betritt, muss alle drei lesen"
        );
    }

    #[test]
    fn an_empty_pool_lists_nothing() {
        assert_eq!(merge_listing(&[]), Vec::new());
    }

    #[test]
    fn the_order_of_the_branch_listings_does_not_change_the_result() {
        let a = listing(0, &[("x", EntryKind::File), ("y", EntryKind::Directory)]);
        let b = listing(1, &[("y", EntryKind::Directory), ("z", EntryKind::File)]);
        assert_eq!(
            merge_listing(&[a.clone(), b.clone()]),
            merge_listing(&[b, a])
        );
    }
}
