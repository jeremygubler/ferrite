// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Die Regeln eines Shares: wohin ein neues Objekt gehoert.

use crate::branch::BranchId;

/// Nach welchem Massstab unter den in Frage kommenden Branches gewaehlt wird.
///
/// Alle drei sind **deterministisch**. Bei Gleichstand gewinnt der kleinere
/// `slot_index`, und der Zeiger fuer [`Allocation::RoundRobin`] kommt als
/// Parameter herein statt aus einem globalen Zustand (Regel 8 aus
/// `CLAUDE.md`). Wer dieselbe Lage zweimal vorlegt, bekommt zweimal dieselbe
/// Antwort — sonst liesse sich eine Fehlplatzierung nicht nachstellen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Allocation {
    /// Der Branch mit dem meisten freien Platz.
    ///
    /// Haelt die Platten gleichmaessig gefuellt. Der Preis: Ein Verzeichnis
    /// verteilt sich ueber alle Platten, und beim Lesen laufen alle an. Fuer
    /// ein NAS, das nachts Platten schlafen legen soll, ist das der falsche
    /// Massstab — dafuer gibt es [`Allocation::FillUp`].
    #[default]
    MostFree,
    /// Der erste Branch mit Platz, in der Reihenfolge der Slot-Indizes.
    ///
    /// Fuellt eine Platte nach der anderen. Was zusammen geschrieben wurde,
    /// liegt zusammen — und die uebrigen Platten koennen schlafen.
    FillUp,
    /// Reihum, ausgehend vom zuletzt benutzten Branch.
    ///
    /// Verteilt gleichmaessig nach Anzahl statt nach Groesse. Nuetzlich, wenn
    /// viele kleine Objekte anfallen und der freie Platz sich dabei kaum
    /// aendert — dann traefe `MostFree` immer wieder dieselbe Platte.
    RoundRobin,
}

/// Bis zu welcher Tiefe ein Verzeichnis ueber mehrere Branches verteilt sein
/// darf.
///
/// # Wofuer das da ist
///
/// Ohne diese Regel landet die zweite Folge einer Serie auf einer anderen
/// Platte als die erste. Fuer die Korrektheit ist das gleichgueltig, fuer den
/// Betrieb nicht: Wer eine Serie schaut, weckt dann jede Platte im Gehaeuse,
/// und wer eine Platte verliert, verliert aus jedem Verzeichnis ein Stueck
/// statt einiger Verzeichnisse ganz.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SplitDepth {
    /// Keine Einschraenkung: Jedes Objekt darf ueberall hin.
    #[default]
    Anywhere,
    /// Objekte bis einschliesslich dieser Tiefe duerfen frei platziert werden.
    /// Alles darunter folgt dem Branch, der den Vorfahren **genau dieser
    /// Tiefe** traegt.
    ///
    /// `UpTo(1)` heisst also: `Filme` und `Musik` duerfen auf verschiedenen
    /// Platten liegen, aber alles unterhalb von `Filme` bleibt zusammen.
    /// `UpTo(0)` heisst: der ganze Share auf einer Platte.
    UpTo(u32),
}

/// Was geschieht, wenn die Split-Regel auf einen vollen Branch zeigt.
///
/// # Die Abwaegung
///
/// Die Split-Regel ist ein Versprechen ueber das Layout, die volle Platte eine
/// physikalische Tatsache. Beides zugleich geht nicht.
///
/// `Spill` ist die Voreinstellung: Ein NAS, das ENOSPC meldet, waehrend 30 TB
/// frei sind, ist im Alltag unbrauchbar. Verschwiegen wird der Bruch trotzdem
/// nicht — [`Placement::spilled`](crate::Placement::spilled) sagt es, und die
/// Control plane kann es melden. Wer das Layout hoeher haengt als die
/// Verfuegbarkeit, stellt auf `Fail`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SplitOverflow {
    /// Auf einen anderen Branch ausweichen und den Bruch melden.
    #[default]
    Spill,
    /// Die Regel halten und den Platz verweigern.
    Fail,
}

/// Die Regeln eines Shares.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SharePolicy {
    pub allocation: Allocation,
    pub split: SplitDepth,
    pub overflow: SplitOverflow,
    /// Wieviele Bytes auf einem Branch frei bleiben muessen.
    ///
    /// Ein btrfs, das bis auf das letzte Byte vollaeuft, laesst sich nicht
    /// mehr aufraeumen — es braucht freien Platz, um Bloecke umzuschichten.
    /// Die Reserve ist deshalb kein Komfort, sondern die Bedingung dafuer,
    /// dass die Platte wieder leer werden kann.
    pub min_free: u64,
    /// Welche Branches dieser Share benutzen darf. Leer heisst: alle.
    ///
    /// Leer als „alle" und nicht als „keine": Ein Share ohne Angabe soll den
    /// ganzen Pool benutzen, und ein Tippfehler in der Konfiguration soll
    /// nicht dazu fuehren, dass gar nichts mehr geschrieben wird.
    pub included: Vec<BranchId>,
}

impl SharePolicy {
    /// Darf dieser Share auf diesen Branch schreiben?
    pub fn includes(&self, branch: BranchId) -> bool {
        self.included.is_empty() || self.included.contains(&branch)
    }

    /// Die Tiefe, ab der die Split-Regel bindet, oder `None` fuer
    /// [`SplitDepth::Anywhere`].
    pub fn split_depth(&self) -> Option<u32> {
        match self.split {
            SplitDepth::Anywhere => None,
            SplitDepth::UpTo(depth) => Some(depth),
        }
    }

    /// Bindet die Split-Regel fuer ein Objekt dieser Tiefe?
    ///
    /// Erst **unterhalb** der angegebenen Tiefe. Ein Objekt genau dieser Tiefe
    /// darf noch frei platziert werden — sonst waere `UpTo(1)` dasselbe wie
    /// `UpTo(0)`.
    pub fn binds_at(&self, depth: u32) -> bool {
        self.split_depth().is_some_and(|limit| depth > limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_include_list_means_every_branch() {
        let policy = SharePolicy::default();
        assert!(policy.includes(BranchId(0)));
        assert!(policy.includes(BranchId(7)));
    }

    #[test]
    fn a_filled_include_list_excludes_everything_else() {
        let policy = SharePolicy {
            included: vec![BranchId(1), BranchId(2)],
            ..SharePolicy::default()
        };
        assert!(!policy.includes(BranchId(0)));
        assert!(policy.includes(BranchId(1)));
        assert!(!policy.includes(BranchId(3)));
    }

    #[test]
    fn the_split_rule_binds_only_below_its_depth() {
        let policy = SharePolicy {
            split: SplitDepth::UpTo(1),
            ..SharePolicy::default()
        };
        assert!(!policy.binds_at(0), "die Share-Wurzel selbst nie");
        assert!(!policy.binds_at(1), "auf Tiefe 1 darf noch verteilt werden");
        assert!(policy.binds_at(2), "ab Tiefe 2 bindet sie");
    }

    #[test]
    fn split_up_to_zero_keeps_the_whole_share_on_one_branch() {
        let policy = SharePolicy {
            split: SplitDepth::UpTo(0),
            ..SharePolicy::default()
        };
        assert!(!policy.binds_at(0));
        assert!(policy.binds_at(1), "schon das erste Verzeichnis bindet");
    }

    #[test]
    fn without_a_split_rule_nothing_binds() {
        let policy = SharePolicy::default();
        assert!(!policy.binds_at(64));
    }
}
