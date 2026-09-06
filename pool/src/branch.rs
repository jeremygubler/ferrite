// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Ein Branch: ein Data-Member, so wie ihn der Pool sieht.

/// Welcher Data-Slot des Arrays.
///
/// Der Wert **ist** der `slot_index` aus dem Superblock. Als eigener Typ und
/// nicht als nacktes `u16`, damit er sich nicht mit einer Tiefe, einem Index in
/// einer Kandidatenliste oder einem Dateideskriptor verwechseln laesst — genau
/// solche Verwechslungen schreiben eine Datei auf die falsche Platte.
///
/// Dass dieses Crate den Slot-Begriff kennt, ohne von `ferrite-engine`
/// abzuhaengen, ist Absicht: Der Pool entscheidet ueber Platten, oeffnet aber
/// keine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BranchId(pub u16);

/// Ein Data-Member als Ziel fuer neue Objekte.
///
/// `free` ist der *tatsaechlich freie* Platz des Dateisystems auf diesem
/// Member, nicht die Restgroesse der Payload-Region. Wer hier die
/// Payload-Groesse einsetzt, rechnet an btrfs' Metadaten vorbei und laeuft
/// spaeter in ein ENOSPC, das der Pool haette kommen sehen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Branch {
    pub id: BranchId,
    /// Groesse des Dateisystems auf diesem Member, in Bytes.
    pub size: u64,
    /// Davon frei, in Bytes.
    pub free: u64,
    /// Nimmt dieser Branch neue Objekte auf?
    ///
    /// `false` fuer einen Member, der gerade wiederaufgebaut wird, der als
    /// unbrauchbar gemeldet ist oder den der Betreiber ausgehaengt hat. Ein
    /// Read geht ueber die Paritaet weiter; ein neues Objekt dorthin zu legen
    /// waere Arbeit, die der naechste Rebuild ueberschreibt.
    pub writable: bool,
}

impl Branch {
    /// Ein beschreibbarer Branch.
    pub fn new(id: BranchId, size: u64, free: u64) -> Self {
        Branch {
            id,
            size,
            free,
            writable: true,
        }
    }

    /// Derselbe Branch, aber nur lesbar.
    pub fn read_only(mut self) -> Self {
        self.writable = false;
        self
    }

    /// Bleiben nach `needed` Bytes noch mindestens `min_free` uebrig?
    ///
    /// Saettigend gerechnet: `free` kommt von `statfs` und `needed` von einem
    /// Aufrufer. Ein Ueberlauf ergaebe hier ein `true`, das eine volle Platte
    /// als leer ausgibt.
    pub fn has_room_for(&self, needed: u64, min_free: u64) -> bool {
        self.free
            .checked_sub(needed)
            .is_some_and(|rest| rest >= min_free)
    }
}

/// Wieviel im ganzen Pool frei ist.
///
/// Fuer `statfs`. Saettigend, damit die Summe ueber viele grosse Platten nicht
/// ueberlaeuft und dabei kleiner wird als ein einzelner Summand.
pub fn total_free(branches: &[Branch]) -> u64 {
    branches
        .iter()
        .fold(0u64, |sum, branch| sum.saturating_add(branch.free))
}

/// Wie gross der ganze Pool ist.
pub fn total_size(branches: &[Branch]) -> u64 {
    branches
        .iter()
        .fold(0u64, |sum, branch| sum.saturating_add(branch.size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_is_measured_against_the_reserve() {
        let branch = Branch::new(BranchId(0), 1000, 100);
        assert!(branch.has_room_for(40, 50), "40 + 50 passen in 100");
        assert!(branch.has_room_for(50, 50), "genau aufgehend zaehlt noch");
        assert!(!branch.has_room_for(51, 50), "51 + 50 passen nicht in 100");
    }

    #[test]
    fn a_request_larger_than_the_disk_does_not_wrap_around() {
        let branch = Branch::new(BranchId(0), 1000, 100);
        assert!(
            !branch.has_room_for(u64::MAX, 0),
            "ein Ueberlauf gaebe eine volle Platte als leer aus"
        );
    }

    #[test]
    fn the_pool_is_the_sum_of_its_branches() {
        let branches = [
            Branch::new(BranchId(0), 100, 10),
            Branch::new(BranchId(1), 200, 20),
        ];
        assert_eq!(total_size(&branches), 300);
        assert_eq!(total_free(&branches), 30);
    }

    #[test]
    fn a_sum_that_would_overflow_saturates_instead_of_shrinking() {
        let branches = [
            Branch::new(BranchId(0), u64::MAX, u64::MAX),
            Branch::new(BranchId(1), 1, 1),
        ];
        assert_eq!(total_free(&branches), u64::MAX);
    }
}
