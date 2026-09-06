// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Die Platzierungsentscheidung: auf welchen Branch ein neues Objekt gehoert.
//!
//! # Der Ablauf
//!
//! Die Regeln werden **nacheinander** angewandt, und jede verkleinert nur die
//! Menge der Kandidaten:
//!
//! 1. Beschreibbar und vom Share eingeschlossen.
//! 2. Bindet die Split-Regel, bleiben nur die Branches, die den Vorfahren
//!    schon tragen.
//! 3. Davon nur die mit genug Platz.
//! 4. Unter den uebrigen waehlt die Allocation.
//!
//! Bleibt nach Schritt 3 nichts uebrig und lag es an Schritt 2, entscheidet
//! [`SplitOverflow`], ob ohne die Einschraenkung noch einmal gesucht wird.
//!
//! Dass die Allocation **zuletzt** kommt, ist der Kern: Sie waehlt, sie
//! entscheidet nicht ueber Zulaessigkeit. Andersherum koennte `MostFree` eine
//! Platte vorschlagen, die der Share gar nicht benutzen darf.

use crate::branch::{Branch, BranchId};
use crate::error::{PoolError, Result};
use crate::path::depth_of;
use crate::policy::{Allocation, SharePolicy, SplitOverflow};

/// Was der Aufrufer ueber das neue Objekt weiss.
#[derive(Debug, Clone, Copy)]
pub struct PlacementRequest<'a> {
    /// Pfad relativ zur Wurzel des Shares.
    pub path: &'a str,
    /// Branches, die den von der Split-Regel gemeinten Vorfahren bereits
    /// tragen.
    ///
    /// Leer heisst: Es gibt ihn noch nicht. Dann ist die Wahl frei, und der
    /// Aufrufer legt die fehlenden Verzeichnisse auf dem gewaehlten Branch an.
    /// Welcher Vorfahre gemeint ist, sagt
    /// [`SharePolicy::split_depth`] zusammen mit
    /// [`ancestor_at`](crate::ancestor_at) — der Pool liest kein Verzeichnis
    /// und kann es deshalb nicht selbst nachsehen.
    pub anchors: &'a [BranchId],
    /// Wieviele Bytes das Objekt mindestens braucht. `0`, wenn unbekannt —
    /// bei einem `create` ist die spaetere Groesse noch niemandem bekannt.
    pub needed: u64,
    /// Der zuletzt benutzte Branch, fuer [`Allocation::RoundRobin`].
    pub cursor: Option<BranchId>,
}

impl<'a> PlacementRequest<'a> {
    /// Eine Anfrage ohne Vorfahren, ohne Groessenangabe, ohne Zeiger.
    pub fn new(path: &'a str) -> Self {
        PlacementRequest {
            path,
            anchors: &[],
            needed: 0,
            cursor: None,
        }
    }

    pub fn with_anchors(mut self, anchors: &'a [BranchId]) -> Self {
        self.anchors = anchors;
        self
    }

    pub fn with_needed(mut self, needed: u64) -> Self {
        self.needed = needed;
        self
    }

    pub fn with_cursor(mut self, cursor: BranchId) -> Self {
        self.cursor = Some(cursor);
        self
    }
}

/// Das Ergebnis einer Platzierung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// Der gewaehlte Branch.
    pub branch: BranchId,
    /// Musste die Split-Regel dafuer gebrochen werden?
    ///
    /// Kein Fehler, aber auch nichts, was unbemerkt bleiben soll: Das Layout
    /// ist danach ein anderes, als der Betreiber es bestellt hat.
    pub spilled: bool,
    /// Der Zeiger fuer die naechste Anfrage mit [`Allocation::RoundRobin`].
    ///
    /// Immer gesetzt, auch bei den anderen Massstaeben — dann ist er einfach
    /// der gewaehlte Branch, und ein Wechsel der Allocation im laufenden
    /// Betrieb faengt nicht bei null an.
    pub cursor: BranchId,
}

/// Waehlt den Branch fuer ein neues Objekt.
pub fn place(
    branches: &[Branch],
    policy: &SharePolicy,
    request: &PlacementRequest<'_>,
) -> Result<Placement> {
    let depth = depth_of(request.path)?;

    // 1. Beschreibbar und eingeschlossen.
    let mut eligible: Vec<&Branch> = branches
        .iter()
        .filter(|branch| branch.writable && policy.includes(branch.id))
        .collect();
    if eligible.is_empty() {
        return Err(PoolError::NoBranch);
    }
    // Nach Slot-Index, nicht nach der Reihenfolge des Aufrufers: `FillUp` und
    // der Gleichstand bei `MostFree` sind ueber den Index definiert, und eine
    // umsortierte Liste duerfte daran nichts aendern.
    eligible.sort_unstable_by_key(|branch| branch.id);

    // 2. Die Split-Regel, falls sie fuer diese Tiefe bindet.
    let bound = policy.binds_at(depth) && !request.anchors.is_empty();
    let anchored: Vec<&Branch> = if bound {
        eligible
            .iter()
            .copied()
            .filter(|branch| request.anchors.contains(&branch.id))
            .collect()
    } else {
        eligible.clone()
    };

    // 3. Genug Platz.
    let roomy = with_room(&anchored, request.needed, policy.min_free);
    if let Some(branch) = choose(&roomy, policy.allocation, request.cursor) {
        return Ok(Placement {
            branch,
            spilled: false,
            cursor: branch,
        });
    }

    // Der Vorfahre liegt auf einer vollen oder nicht beschreibbaren Platte.
    // Jetzt entscheidet der Betreiber, was ihm wichtiger ist.
    if bound && policy.overflow == SplitOverflow::Spill {
        let roomy = with_room(&eligible, request.needed, policy.min_free);
        if let Some(branch) = choose(&roomy, policy.allocation, request.cursor) {
            return Ok(Placement {
                branch,
                spilled: true,
                cursor: branch,
            });
        }
    }

    Err(PoolError::NoSpace {
        needed: request.needed,
        min_free: policy.min_free,
    })
}

/// Die Kandidaten, auf denen das Objekt samt Reserve noch Platz hat.
fn with_room<'a>(candidates: &[&'a Branch], needed: u64, min_free: u64) -> Vec<&'a Branch> {
    candidates
        .iter()
        .copied()
        .filter(|branch| branch.has_room_for(needed, min_free))
        .collect()
}

/// Waehlt aus einer nach Slot-Index geordneten Kandidatenliste.
///
/// `None`, wenn die Liste leer ist — der Aufrufer weiss dann, dass es an
/// Schritt 3 lag und nicht an der Allocation.
fn choose(
    candidates: &[&Branch],
    allocation: Allocation,
    cursor: Option<BranchId>,
) -> Option<BranchId> {
    match allocation {
        // `max_by_key` liefert bei Gleichstand das **letzte** Maximum. Hier
        // soll der kleinste Index gewinnen, also wird rueckwaerts gesucht.
        Allocation::MostFree => candidates
            .iter()
            .rev()
            .max_by_key(|branch| branch.free)
            .map(|branch| branch.id),
        Allocation::FillUp => candidates.first().map(|branch| branch.id),
        Allocation::RoundRobin => {
            let next = cursor.and_then(|last| {
                candidates
                    .iter()
                    .find(|branch| branch.id > last)
                    .map(|branch| branch.id)
            });
            // Kein Kandidat hinter dem Zeiger: von vorn. Das ist der Umlauf.
            next.or_else(|| candidates.first().map(|branch| branch.id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::SplitDepth;

    const GIB: u64 = 1 << 30;

    fn branches() -> Vec<Branch> {
        vec![
            Branch::new(BranchId(0), 100 * GIB, 10 * GIB),
            Branch::new(BranchId(1), 100 * GIB, 60 * GIB),
            Branch::new(BranchId(2), 100 * GIB, 20 * GIB),
        ]
    }

    fn policy(allocation: Allocation) -> SharePolicy {
        SharePolicy {
            allocation,
            ..SharePolicy::default()
        }
    }

    // --- Die Massstaebe ---------------------------------------------------

    #[test]
    fn most_free_takes_the_emptiest_branch() {
        let placement = place(
            &branches(),
            &policy(Allocation::MostFree),
            &PlacementRequest::new("Filme/film.mkv"),
        )
        .unwrap();
        assert_eq!(placement.branch, BranchId(1));
        assert!(!placement.spilled);
    }

    #[test]
    fn most_free_breaks_a_tie_towards_the_lower_slot() {
        let branches = vec![
            Branch::new(BranchId(0), 100, 50),
            Branch::new(BranchId(1), 100, 50),
        ];
        let placement = place(
            &branches,
            &policy(Allocation::MostFree),
            &PlacementRequest::new("x"),
        )
        .unwrap();
        assert_eq!(
            placement.branch,
            BranchId(0),
            "bei Gleichstand entscheidet der Index, nicht die Laufrichtung"
        );
    }

    #[test]
    fn the_order_of_the_caller_does_not_change_the_answer() {
        let mut reversed = branches();
        reversed.reverse();
        let forwards = place(
            &branches(),
            &policy(Allocation::FillUp),
            &PlacementRequest::new("x"),
        )
        .unwrap();
        let backwards = place(
            &reversed,
            &policy(Allocation::FillUp),
            &PlacementRequest::new("x"),
        )
        .unwrap();
        assert_eq!(forwards, backwards);
    }

    #[test]
    fn fill_up_takes_the_first_branch_with_room() {
        let placement = place(
            &branches(),
            &policy(Allocation::FillUp),
            &PlacementRequest::new("x"),
        )
        .unwrap();
        assert_eq!(placement.branch, BranchId(0));
    }

    #[test]
    fn fill_up_moves_on_when_the_first_branch_is_too_full() {
        let branches = vec![
            Branch::new(BranchId(0), 100 * GIB, GIB),
            Branch::new(BranchId(1), 100 * GIB, 60 * GIB),
        ];
        let placement = place(
            &branches,
            &policy(Allocation::FillUp),
            &PlacementRequest::new("x").with_needed(2 * GIB),
        )
        .unwrap();
        assert_eq!(placement.branch, BranchId(1));
    }

    #[test]
    fn round_robin_takes_the_branch_after_the_cursor() {
        let placement = place(
            &branches(),
            &policy(Allocation::RoundRobin),
            &PlacementRequest::new("x").with_cursor(BranchId(0)),
        )
        .unwrap();
        assert_eq!(placement.branch, BranchId(1));
        assert_eq!(placement.cursor, BranchId(1), "der Zeiger zieht mit");
    }

    #[test]
    fn round_robin_wraps_around_at_the_end() {
        let placement = place(
            &branches(),
            &policy(Allocation::RoundRobin),
            &PlacementRequest::new("x").with_cursor(BranchId(2)),
        )
        .unwrap();
        assert_eq!(placement.branch, BranchId(0));
    }

    #[test]
    fn round_robin_skips_a_branch_without_room() {
        let branches = vec![
            Branch::new(BranchId(0), 100 * GIB, 60 * GIB),
            Branch::new(BranchId(1), 100 * GIB, GIB),
            Branch::new(BranchId(2), 100 * GIB, 60 * GIB),
        ];
        let placement = place(
            &branches,
            &policy(Allocation::RoundRobin),
            &PlacementRequest::new("x")
                .with_cursor(BranchId(0))
                .with_needed(2 * GIB),
        )
        .unwrap();
        assert_eq!(
            placement.branch,
            BranchId(2),
            "die volle Platte kommt in der Reihe gar nicht vor"
        );
    }

    // --- Zulaessigkeit ----------------------------------------------------

    #[test]
    fn a_read_only_branch_takes_nothing_new() {
        let branches = vec![
            branches()[1].read_only(),
            Branch::new(BranchId(2), 100 * GIB, 20 * GIB),
        ];
        let placement = place(
            &branches,
            &policy(Allocation::MostFree),
            &PlacementRequest::new("x"),
        )
        .unwrap();
        assert_eq!(
            placement.branch,
            BranchId(2),
            "der leerere Branch wird gerade wiederaufgebaut"
        );
    }

    #[test]
    fn a_share_only_writes_to_the_branches_it_is_allowed_to() {
        let share = SharePolicy {
            allocation: Allocation::MostFree,
            included: vec![BranchId(0), BranchId(2)],
            ..SharePolicy::default()
        };
        let placement = place(&branches(), &share, &PlacementRequest::new("x")).unwrap();
        assert_eq!(placement.branch, BranchId(2), "Branch 1 waere leerer");
    }

    #[test]
    fn the_reserve_is_kept_free() {
        let branches = vec![Branch::new(BranchId(0), 100 * GIB, 10 * GIB)];
        let share = SharePolicy {
            min_free: 8 * GIB,
            ..SharePolicy::default()
        };
        assert!(
            place(
                &branches,
                &share,
                &PlacementRequest::new("x").with_needed(GIB)
            )
            .is_ok(),
            "1 GiB passt neben 8 GiB Reserve in 10 GiB"
        );
        assert_eq!(
            place(
                &branches,
                &share,
                &PlacementRequest::new("x").with_needed(3 * GIB)
            ),
            Err(PoolError::NoSpace {
                needed: 3 * GIB,
                min_free: 8 * GIB
            })
        );
    }

    #[test]
    fn a_pool_without_a_usable_branch_says_so_precisely() {
        // Kein Branch ueberhaupt ist etwas anderes als kein Platz — beim
        // ersten ist die Konfiguration falsch, beim zweiten die Platte voll.
        assert_eq!(
            place(&[], &SharePolicy::default(), &PlacementRequest::new("x")),
            Err(PoolError::NoBranch)
        );

        let all_read_only: Vec<Branch> = branches().into_iter().map(Branch::read_only).collect();
        assert_eq!(
            place(
                &all_read_only,
                &SharePolicy::default(),
                &PlacementRequest::new("x")
            ),
            Err(PoolError::NoBranch)
        );
    }

    #[test]
    fn a_path_that_climbs_out_of_the_branch_is_refused_before_anything_else() {
        assert!(matches!(
            place(
                &branches(),
                &SharePolicy::default(),
                &PlacementRequest::new("../../etc/shadow")
            ),
            Err(PoolError::InvalidPath { .. })
        ));
    }

    // --- Die Split-Regel --------------------------------------------------

    fn split_policy(overflow: SplitOverflow) -> SharePolicy {
        SharePolicy {
            allocation: Allocation::MostFree,
            split: SplitDepth::UpTo(1),
            overflow,
            ..SharePolicy::default()
        }
    }

    #[test]
    fn above_the_split_depth_the_choice_stays_free() {
        // `Filme` selbst hat Tiefe 1 und darf dorthin, wo am meisten frei ist,
        // auch wenn `Musik` schon woanders liegt.
        let anchors = [BranchId(0)];
        let placement = place(
            &branches(),
            &split_policy(SplitOverflow::Fail),
            &PlacementRequest::new("Filme").with_anchors(&anchors),
        )
        .unwrap();
        assert_eq!(placement.branch, BranchId(1));
    }

    #[test]
    fn below_the_split_depth_everything_follows_its_ancestor() {
        // `Filme` liegt auf Branch 0. Alles darunter geht dorthin, obwohl
        // Branch 1 mehr frei hat.
        let anchors = [BranchId(0)];
        let placement = place(
            &branches(),
            &split_policy(SplitOverflow::Fail),
            &PlacementRequest::new("Filme/2026/film.mkv").with_anchors(&anchors),
        )
        .unwrap();
        assert_eq!(placement.branch, BranchId(0));
        assert!(!placement.spilled);
    }

    #[test]
    fn an_ancestor_on_several_branches_narrows_but_does_not_decide() {
        let anchors = [BranchId(0), BranchId(2)];
        let placement = place(
            &branches(),
            &split_policy(SplitOverflow::Fail),
            &PlacementRequest::new("Filme/2026/film.mkv").with_anchors(&anchors),
        )
        .unwrap();
        assert_eq!(
            placement.branch,
            BranchId(2),
            "unter den Ankern waehlt weiter die Allocation"
        );
    }

    #[test]
    fn a_missing_ancestor_leaves_the_choice_free() {
        // Noch niemand hat `Serien` angelegt. Dann gibt es nichts, dem zu
        // folgen waere.
        let placement = place(
            &branches(),
            &split_policy(SplitOverflow::Fail),
            &PlacementRequest::new("Serien/S01/e01.mkv"),
        )
        .unwrap();
        assert_eq!(placement.branch, BranchId(1));
    }

    #[test]
    fn a_full_ancestor_spills_and_says_so() {
        let branches = vec![
            Branch::new(BranchId(0), 100 * GIB, GIB),
            Branch::new(BranchId(1), 100 * GIB, 60 * GIB),
        ];
        let anchors = [BranchId(0)];
        let placement = place(
            &branches,
            &split_policy(SplitOverflow::Spill),
            &PlacementRequest::new("Filme/2026/film.mkv")
                .with_anchors(&anchors)
                .with_needed(2 * GIB),
        )
        .unwrap();
        assert_eq!(placement.branch, BranchId(1));
        assert!(
            placement.spilled,
            "der Bruch der Split-Regel muss sichtbar sein, sonst ist er still"
        );
    }

    #[test]
    fn a_full_ancestor_can_also_be_a_refusal() {
        let branches = vec![
            Branch::new(BranchId(0), 100 * GIB, GIB),
            Branch::new(BranchId(1), 100 * GIB, 60 * GIB),
        ];
        let anchors = [BranchId(0)];
        assert_eq!(
            place(
                &branches,
                &split_policy(SplitOverflow::Fail),
                &PlacementRequest::new("Filme/2026/film.mkv")
                    .with_anchors(&anchors)
                    .with_needed(2 * GIB),
            ),
            Err(PoolError::NoSpace {
                needed: 2 * GIB,
                min_free: 0
            })
        );
    }

    #[test]
    fn an_ancestor_on_a_read_only_branch_spills_too() {
        // Die Platte wird gerade wiederaufgebaut. Dorthin zu schreiben hiesse,
        // Arbeit zu machen, die der Rebuild ueberschreibt.
        let branches = vec![
            Branch::new(BranchId(0), 100 * GIB, 60 * GIB).read_only(),
            Branch::new(BranchId(1), 100 * GIB, 20 * GIB),
        ];
        let anchors = [BranchId(0)];
        let placement = place(
            &branches,
            &split_policy(SplitOverflow::Spill),
            &PlacementRequest::new("Filme/2026/film.mkv").with_anchors(&anchors),
        )
        .unwrap();
        assert_eq!(placement.branch, BranchId(1));
        assert!(placement.spilled);
    }

    #[test]
    fn a_full_pool_is_refused_even_with_spill() {
        let branches = vec![
            Branch::new(BranchId(0), 100 * GIB, GIB),
            Branch::new(BranchId(1), 100 * GIB, GIB),
        ];
        let anchors = [BranchId(0)];
        assert!(matches!(
            place(
                &branches,
                &split_policy(SplitOverflow::Spill),
                &PlacementRequest::new("Filme/x/y.mkv")
                    .with_anchors(&anchors)
                    .with_needed(2 * GIB),
            ),
            Err(PoolError::NoSpace { .. })
        ));
    }

    #[test]
    fn split_up_to_zero_keeps_the_whole_share_together() {
        let share = SharePolicy {
            allocation: Allocation::MostFree,
            split: SplitDepth::UpTo(0),
            overflow: SplitOverflow::Fail,
            ..SharePolicy::default()
        };
        let anchors = [BranchId(0)];
        let placement = place(
            &branches(),
            &share,
            &PlacementRequest::new("Filme").with_anchors(&anchors),
        )
        .unwrap();
        assert_eq!(
            placement.branch,
            BranchId(0),
            "schon das erste Verzeichnis folgt der Share-Wurzel"
        );
    }

    #[test]
    fn an_anchor_that_is_not_a_branch_of_this_pool_is_ignored() {
        // Ein Anker aus einer alten Konfiguration. Er darf die Auswahl nicht
        // leeren, sonst schluege der Pool wegen einer Karteileiche fehl.
        let anchors = [BranchId(9)];
        let placement = place(
            &branches(),
            &split_policy(SplitOverflow::Spill),
            &PlacementRequest::new("Filme/x/y.mkv").with_anchors(&anchors),
        )
        .unwrap();
        assert_eq!(placement.branch, BranchId(1));
        assert!(placement.spilled);
    }
}
