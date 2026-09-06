// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Ein Share im Betrieb, ueber viele Platzierungen hinweg.
//!
//! Die Unit-Tests im Crate pruefen jede Regel fuer sich. Hier laufen sie
//! zusammen: Ein Verzeichnisbaum waechst, Platten fuellen sich, und die Frage
//! ist, ob das Ergebnis danach noch die Eigenschaften hat, die der Betreiber
//! bestellt hat.
//!
//! Der Pool selbst legt nichts an — hier steht deshalb eine winzige
//! Nachbildung eines Dateisystems: eine Liste von „Pfad liegt auf Branch".
//! Sie ersetzt kein FUSE, sie liefert nur die Anker, die der Pool als
//! Parameter erwartet.

use std::collections::BTreeMap;

use ferrite_pool::{
    ancestor_at, merge_listing, place, Allocation, Branch, BranchId, EntryKind, PlacementRequest,
    PoolError, Resolution, SharePolicy, SplitDepth, SplitOverflow,
};

const GIB: u64 = 1 << 30;

/// Ein nachgebildeter Pool: welcher Pfad liegt auf welchen Branches, und
/// wieviel ist wo noch frei.
struct Scenario {
    branches: Vec<Branch>,
    /// Pfad -> Branches, die ihn tragen. `BTreeMap`, damit die Reihenfolge
    /// ueber Laeufe hinweg gleich bleibt.
    tree: BTreeMap<String, Vec<BranchId>>,
}

impl Scenario {
    fn new(free: &[u64]) -> Self {
        Scenario {
            branches: free
                .iter()
                .enumerate()
                .map(|(index, free)| Branch::new(BranchId(index as u16), 100 * GIB, *free))
                .collect(),
            tree: BTreeMap::new(),
        }
    }

    fn branch_mut(&mut self, id: BranchId) -> &mut Branch {
        self.branches
            .iter_mut()
            .find(|branch| branch.id == id)
            .expect("Branch aus einer Platzierung muss es geben")
    }

    /// Die Branches, die den von der Split-Regel gemeinten Vorfahren tragen.
    fn anchors(&self, policy: &SharePolicy, path: &str) -> Vec<BranchId> {
        let Some(depth) = policy.split_depth() else {
            return Vec::new();
        };
        let Some(ancestor) = ancestor_at(path, depth) else {
            return Vec::new();
        };
        self.tree.get(ancestor).cloned().unwrap_or_default()
    }

    /// Legt eine Datei an: platzieren, Vorfahren mitziehen, Platz abziehen.
    fn create(
        &mut self,
        policy: &SharePolicy,
        path: &str,
        size: u64,
    ) -> Result<BranchId, PoolError> {
        let anchors = self.anchors(policy, path);
        let placement = place(
            &self.branches,
            policy,
            &PlacementRequest::new(path)
                .with_anchors(&anchors)
                .with_needed(size),
        )?;

        // Die Verzeichnisse auf dem Weg dorthin entstehen mit.
        let components: Vec<&str> = path.split('/').collect();
        for depth in 1..=components.len() {
            let prefix = components[..depth].join("/");
            let holders = self.tree.entry(prefix).or_default();
            if !holders.contains(&placement.branch) {
                holders.push(placement.branch);
            }
        }
        self.branch_mut(placement.branch).free -= size;
        Ok(placement.branch)
    }

    /// Auf welchem Branch liegt dieser Pfad?
    fn holders(&self, path: &str) -> Vec<BranchId> {
        self.tree.get(path).cloned().unwrap_or_default()
    }
}

fn media_policy(split: SplitDepth) -> SharePolicy {
    SharePolicy {
        allocation: Allocation::MostFree,
        split,
        overflow: SplitOverflow::Spill,
        min_free: GIB,
        included: Vec::new(),
    }
}

#[test]
fn a_season_stays_on_one_disk() {
    // Der Sinn der Split-Regel: Wer eine Staffel schaut, soll nicht jede
    // Platte im Gehaeuse wecken.
    let policy = media_policy(SplitDepth::UpTo(1));
    let mut scenario = Scenario::new(&[50 * GIB, 60 * GIB, 55 * GIB]);

    let mut used = Vec::new();
    for episode in 1..=10 {
        let path = format!("Serien/Eine Serie/S01/E{episode:02}.mkv");
        used.push(scenario.create(&policy, &path, 2 * GIB).unwrap());
    }

    let first = used[0];
    assert!(
        used.iter().all(|branch| *branch == first),
        "die Staffel ist ueber mehrere Platten verteilt: {used:?}"
    );
    assert_eq!(
        scenario.holders("Serien/Eine Serie"),
        vec![first],
        "auch das Verzeichnis darf nur auf einer Platte entstanden sein"
    );
}

#[test]
fn without_a_split_rule_the_same_season_spreads_out() {
    // Die Gegenprobe. Ohne sie koennte der Test darueber gruen sein, weil
    // `MostFree` zufaellig immer dieselbe Platte trifft.
    let policy = media_policy(SplitDepth::Anywhere);
    let mut scenario = Scenario::new(&[50 * GIB, 60 * GIB, 55 * GIB]);

    let mut used = Vec::new();
    for episode in 1..=10 {
        let path = format!("Serien/Eine Serie/S01/E{episode:02}.mkv");
        used.push(scenario.create(&policy, &path, 2 * GIB).unwrap());
    }

    assert!(
        used.iter().any(|branch| *branch != used[0]),
        "ohne Split-Regel muesste MostFree die Platten wechseln"
    );
}

#[test]
fn top_level_shares_land_on_different_disks() {
    // Split-Tiefe 1 heisst nicht „alles auf eine Platte": Die obersten
    // Verzeichnisse duerfen und sollen sich verteilen.
    let policy = media_policy(SplitDepth::UpTo(1));
    let mut scenario = Scenario::new(&[60 * GIB, 60 * GIB, 60 * GIB]);

    let filme = scenario.create(&policy, "Filme/a.mkv", 20 * GIB).unwrap();
    let musik = scenario.create(&policy, "Musik/a.flac", 20 * GIB).unwrap();
    let fotos = scenario.create(&policy, "Fotos/a.raw", 20 * GIB).unwrap();

    assert_eq!(
        [filme, musik, fotos]
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3,
        "drei gleich grosse Verzeichnisse auf drei gleich leere Platten"
    );
}

#[test]
fn a_full_disk_spills_and_the_break_is_visible() {
    let policy = media_policy(SplitDepth::UpTo(1));
    let mut scenario = Scenario::new(&[10 * GIB, 60 * GIB]);

    // Erst laeuft alles auf Branch 1 — er hat mehr frei.
    scenario
        .create(&policy, "Filme/eins.mkv", 56 * GIB)
        .unwrap();
    assert_eq!(scenario.holders("Filme"), vec![BranchId(1)]);

    // Jetzt passt nichts Grosses mehr dorthin. Die Split-Regel zeigt auf
    // Branch 1, der Platz ist auf Branch 0.
    let anchors = scenario.anchors(&policy, "Filme/zwei.mkv");
    let placement = place(
        &scenario.branches,
        &policy,
        &PlacementRequest::new("Filme/zwei.mkv")
            .with_anchors(&anchors)
            .with_needed(5 * GIB),
    )
    .unwrap();

    assert_eq!(placement.branch, BranchId(0));
    assert!(
        placement.spilled,
        "der Bruch der Split-Regel muss gemeldet werden, nicht bloss geschehen"
    );
}

#[test]
fn a_share_that_may_not_spill_says_no_instead() {
    let policy = SharePolicy {
        overflow: SplitOverflow::Fail,
        ..media_policy(SplitDepth::UpTo(1))
    };
    let mut scenario = Scenario::new(&[10 * GIB, 60 * GIB]);
    scenario
        .create(&policy, "Filme/eins.mkv", 56 * GIB)
        .unwrap();

    let anchors = scenario.anchors(&policy, "Filme/zwei.mkv");
    assert!(matches!(
        place(
            &scenario.branches,
            &policy,
            &PlacementRequest::new("Filme/zwei.mkv")
                .with_anchors(&anchors)
                .with_needed(5 * GIB),
        ),
        Err(PoolError::NoSpace { .. })
    ));
}

#[test]
fn the_reserve_survives_a_share_being_filled_up() {
    // Ein btrfs, das bis auf das letzte Byte vollaeuft, laesst sich nicht mehr
    // aufraeumen. Die Reserve ist deshalb keine Bequemlichkeit.
    let policy = SharePolicy {
        allocation: Allocation::FillUp,
        min_free: 5 * GIB,
        ..media_policy(SplitDepth::Anywhere)
    };
    let mut scenario = Scenario::new(&[20 * GIB, 20 * GIB]);

    let mut placed = 0;
    for nth in 0..100 {
        match scenario.create(&policy, &format!("Ablage/{nth}.bin"), 2 * GIB) {
            Ok(_) => placed += 1,
            Err(PoolError::NoSpace { .. }) => break,
            Err(other) => panic!("unerwartet: {other}"),
        }
    }

    assert!(placed > 0, "es muss etwas hineingepasst haben");
    for branch in &scenario.branches {
        assert!(
            branch.free >= 5 * GIB,
            "Branch {:?} ist unter die Reserve gefallen: {} Bytes frei",
            branch.id,
            branch.free
        );
    }
}

#[test]
fn the_union_shows_a_tree_that_lies_on_several_disks() {
    // Was der Nutzer sieht: ein Verzeichnis, obwohl der Inhalt auf zwei
    // Platten liegt.
    let listings = [
        (
            BranchId(0),
            vec![
                ("Filme".to_string(), EntryKind::Directory),
                ("Musik".to_string(), EntryKind::Directory),
            ],
        ),
        (
            BranchId(1),
            vec![
                ("Filme".to_string(), EntryKind::Directory),
                ("Fotos".to_string(), EntryKind::Directory),
            ],
        ),
    ];

    let merged = merge_listing(&listings);
    let names: Vec<&str> = merged.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, ["Filme", "Fotos", "Musik"]);

    let filme = &merged[0].resolution;
    assert_eq!(
        filme,
        &Resolution::Directory {
            branches: vec![BranchId(0), BranchId(1)]
        },
        "wer `Filme` betritt, muss beide Platten lesen"
    );
    assert!(merged.iter().all(|entry| !entry.resolution.is_conflict()));
}

#[test]
fn a_file_that_exists_twice_is_shown_and_reported() {
    // Der Fall, den Unraid still aufloest. Ferrite bedient ihn genauso
    // deterministisch, nennt ihn aber beim Namen.
    let listings = [
        (
            BranchId(1),
            vec![("urlaub.jpg".to_string(), EntryKind::File)],
        ),
        (
            BranchId(0),
            vec![("urlaub.jpg".to_string(), EntryKind::File)],
        ),
    ];

    let merged = merge_listing(&listings);
    assert_eq!(merged.len(), 1, "der Name erscheint einmal");
    assert!(merged[0].resolution.is_conflict());
    assert_eq!(
        merged[0].resolution.served_by(),
        Some(BranchId(0)),
        "bedient wird immer derselbe Branch, unabhaengig von der Lesereihenfolge"
    );
}
