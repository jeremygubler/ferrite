// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Ferrite findet seine Platten selbst.
//!
//! # Warum das keine Bequemlichkeit ist
//!
//! Wer die Geraeteliste in eine Konfigurationsdatei schreibt, hat sie an zwei
//! Orten: dort und in den Superbloecken. Steckt jemand eine Platte um, weichen
//! sie voneinander ab — und dann startet Ferrite mit der falschen. Die
//! Superbloecke sind die Wahrheit; diese Datei sagt nur, wo gesucht wird.
//!
//! # Die Falle
//!
//! Unter `/dev/disk/by-id` steht **dieselbe Platte mehrfach**: einmal als
//! `ata-…`, einmal als `wwn-…`, bei NVMe noch als `nvme-eui.…`. Wer die
//! Fundstellen ungefiltert weitergibt, uebergibt `assemble` denselben Member
//! zwei- oder dreimal — und bekommt `DuplicateMemberUuid`, obwohl mit dem
//! Array alles in Ordnung ist.
//!
//! Entdoppelt wird ueber die **Member-UUID** aus dem Superblock. Sie ist genau
//! dafuer da: Regel 3 aus Abschnitt 2.1 verlangt, dass sie je Member
//! verschieden ist. Zwei Pfade mit derselben Member-UUID sind zwei Namen fuer
//! eine Platte.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ferrite_format::superblock::Superblock;

/// Eine gefundene Platte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub superblock: Superblock,
}

/// Alles, was ein Suchlauf ergeben hat.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    /// Nach Array-UUID geordnet. `BTreeMap`, damit die Ausgabe ueber Laeufe
    /// hinweg gleich bleibt.
    pub arrays: BTreeMap<String, Vec<Found>>,
    /// Wieviele Pfade angesehen wurden.
    pub looked_at: usize,
    /// Wieviele davon derselben Platte unter einem zweiten Namen gehoerten.
    pub duplicates: usize,
}

impl Scan {
    /// Die Members eines Arrays, oder — wenn keines genannt ist — die des
    /// einzigen gefundenen.
    ///
    /// Zwei Arrays ohne Angabe sind **kein** Grund, eines zu waehlen: Welches
    /// gemeint ist, weiss nur der Betreiber, und die falsche Wahl haenge ein
    /// fremdes Array ein.
    pub fn pick(&self, wanted: Option<&str>) -> Result<&[Found], PickError> {
        match wanted {
            Some(uuid) => self
                .arrays
                .get(uuid)
                .map(Vec::as_slice)
                .ok_or_else(|| PickError::NotFound(uuid.to_string())),
            None => match self.arrays.len() {
                0 => Err(PickError::Nothing),
                1 => Ok(self.arrays.values().next().expect("genau eines").as_slice()),
                _ => Err(PickError::Several(self.arrays.keys().cloned().collect())),
            },
        }
    }
}

/// Warum aus einem Suchlauf kein Array wurde.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickError {
    /// Nirgends ein Ferrite-Superblock.
    Nothing,
    /// Mehrere Arrays gefunden, aber keines genannt.
    Several(Vec<String>),
    /// Das genannte Array war nicht dabei.
    NotFound(String),
}

impl std::fmt::Display for PickError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Nothing => write!(
                f,
                "kein Ferrite-Array gefunden. Stimmt `scan` in der Konfiguration?"
            ),
            Self::Several(uuids) => write!(
                f,
                "mehrere Arrays gefunden — welches gemeint ist, muss dastehen.\n\
                 Mit `array = <UUID>` in der Konfiguration waehlen:\n  {}",
                uuids.join("\n  ")
            ),
            Self::NotFound(uuid) => write!(f, "Array {uuid} war unter den gefundenen nicht dabei"),
        }
    }
}

impl std::error::Error for PickError {}

/// Ordnet die Fundstellen nach Array und wirft doppelte Namen weg.
///
/// Rein: Die Fundstellen kommen als Parameter herein. Deshalb laesst sich die
/// Entdopplung pruefen, ohne ein `/dev` zu haben, in dem dieselbe Platte
/// dreimal steht.
pub fn group(found: Vec<Found>) -> Scan {
    let looked_at = found.len();
    let mut arrays: BTreeMap<String, Vec<Found>> = BTreeMap::new();
    let mut duplicates = 0usize;

    for entry in found {
        let array = entry.superblock.array_uuid.to_string();
        let members = arrays.entry(array).or_default();

        // Dieselbe Member-UUID heisst: dieselbe Platte unter einem zweiten
        // Namen. Behalten wird der kleinere Pfad — nicht aus Geschmack,
        // sondern damit zwei Laeufe dieselbe Antwort geben.
        match members
            .iter_mut()
            .find(|seen| seen.superblock.member_uuid == entry.superblock.member_uuid)
        {
            Some(seen) => {
                duplicates += 1;
                if entry.path < seen.path {
                    *seen = entry;
                }
            }
            None => members.push(entry),
        }
    }

    for members in arrays.values_mut() {
        members.sort_by(|left, right| {
            (left.superblock.role as u8, left.superblock.slot_index)
                .cmp(&(right.superblock.role as u8, right.superblock.slot_index))
        });
    }

    Scan {
        arrays,
        looked_at,
        duplicates,
    }
}

/// Sieht sich alles an, was in den angegebenen Verzeichnissen liegt.
///
/// Ein Pfad, der sich nicht oeffnen oder nicht lesen laesst, wird
/// uebergangen — in `/dev/disk/by-id` liegt vieles, was kein Ferrite-Member
/// ist, und jedes davon zu melden waere Laerm statt Auskunft.
#[cfg(unix)]
/// Der Suchlauf als JSON.
///
/// Dieselbe Auskunft wie die Textausgabe, nur ohne Saetze. Groessen in Bytes.
pub fn scan_json(scan: &Scan, searched: &[PathBuf]) -> String {
    use crate::json::Value;

    let arrays = scan
        .arrays
        .iter()
        .map(|(uuid, members)| {
            Value::object(vec![
                ("array", Value::text(uuid)),
                (
                    "members",
                    Value::List(
                        members
                            .iter()
                            .map(|found| {
                                Value::object(vec![
                                    ("device", Value::text(found.path.display().to_string())),
                                    ("role", Value::text(role_name(found.superblock.role))),
                                    (
                                        "slot",
                                        Value::Number(u64::from(found.superblock.slot_index)),
                                    ),
                                    ("size", Value::Number(found.superblock.payload_size)),
                                ])
                            })
                            .collect(),
                    ),
                ),
            ])
        })
        .collect();

    Value::object(vec![
        ("arrays", Value::List(arrays)),
        ("looked_at", Value::Number(scan.looked_at as u64)),
        ("duplicates", Value::Number(scan.duplicates as u64)),
        (
            "searched",
            Value::List(
                searched
                    .iter()
                    .map(|path| Value::text(path.display().to_string()))
                    .collect(),
            ),
        ),
    ])
    .render()
}

/// Der Name einer Rolle im JSON. Englisch und stabil, wie in [`crate::report`].
fn role_name(role: ferrite_format::superblock::Role) -> &'static str {
    use ferrite_format::superblock::Role;
    match role {
        Role::Data => "data",
        Role::ParityP => "parity-p",
        Role::ParityQ => "parity-q",
        Role::Log => "log",
    }
}

pub fn scan(directories: &[PathBuf]) -> Scan {
    use ferrite_engine::{read_superblock, MemberDevice};

    let mut found = Vec::new();
    for directory in directories {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(device) = MemberDevice::open_read_only(&path) else {
                continue;
            };
            let Ok(superblock) = read_superblock(&device) else {
                continue;
            };
            found.push(Found { path, superblock });
        }
    }
    group(found)
}

/// Die Pfade eines Arrays, in der Reihenfolge Data, P, Q, Log.
pub fn paths(members: &[Found]) -> Vec<PathBuf> {
    members.iter().map(|member| member.path.clone()).collect()
}

/// Kurzform fuer die Ausgabe: `3898328e` statt der ganzen UUID.
pub fn short(uuid: &str) -> &str {
    uuid.split_once('-').map_or(uuid, |(first, _)| first)
}

/// Damit `Path` im Modul auch ohne die I/O-Haelfte benutzt wird.
#[allow(dead_code)]
fn is_there(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrite_format::superblock::Role;
    use ferrite_format::Uuid;

    fn member(array: u8, member_id: u8, role: Role, slot: u16, path: &str) -> Found {
        let mut superblock = Superblock::new(
            Uuid::from_random_bytes([array; 16]),
            Uuid::from_random_bytes([member_id; 16]),
            role,
            2,
            8 << 20,
        );
        superblock.slot_index = slot;
        Found {
            path: PathBuf::from(path),
            superblock,
        }
    }

    fn small_array(array: u8) -> Vec<Found> {
        vec![
            member(array, 1, Role::Data, 0, "/dev/disk/by-id/ata-a"),
            member(array, 2, Role::Data, 1, "/dev/disk/by-id/ata-b"),
            member(array, 3, Role::ParityP, 0, "/dev/disk/by-id/ata-p"),
            member(array, 4, Role::Log, 0, "/dev/disk/by-id/ata-l"),
        ]
    }

    #[test]
    fn one_array_is_found_without_being_named() {
        let scan = group(small_array(0xA1));
        assert_eq!(scan.arrays.len(), 1);
        assert_eq!(scan.pick(None).unwrap().len(), 4);
    }

    #[test]
    fn the_same_disk_under_two_names_counts_once() {
        // Der Fall, der `/dev/disk/by-id` ausmacht: dieselbe Platte als
        // `ata-…` und als `wwn-…`. Ungefiltert bekaeme `assemble` sie doppelt
        // und meldete `DuplicateMemberUuid` — bei einem heilen Array.
        let mut found = small_array(0xA1);
        found.push(member(0xA1, 1, Role::Data, 0, "/dev/disk/by-id/wwn-a"));
        found.push(member(0xA1, 3, Role::ParityP, 0, "/dev/disk/by-id/wwn-p"));

        let scan = group(found);
        assert_eq!(scan.looked_at, 6);
        assert_eq!(scan.duplicates, 2);
        assert_eq!(scan.pick(None).unwrap().len(), 4);
    }

    #[test]
    fn which_of_two_names_survives_does_not_depend_on_the_order() {
        // Die Reihenfolge von `read_dir` ist beliebig. Waehlte die
        // Entdopplung danach, saehe jeder Start anders aus — und eine
        // Fehlersuche haette keinen festen Boden.
        let forwards = group(vec![
            member(0xA1, 1, Role::Data, 0, "/dev/disk/by-id/ata-a"),
            member(0xA1, 1, Role::Data, 0, "/dev/disk/by-id/wwn-a"),
        ]);
        let backwards = group(vec![
            member(0xA1, 1, Role::Data, 0, "/dev/disk/by-id/wwn-a"),
            member(0xA1, 1, Role::Data, 0, "/dev/disk/by-id/ata-a"),
        ]);
        assert_eq!(forwards.pick(None).unwrap(), backwards.pick(None).unwrap());
        assert_eq!(
            forwards.pick(None).unwrap()[0].path,
            PathBuf::from("/dev/disk/by-id/ata-a")
        );
    }

    #[test]
    fn two_arrays_without_a_name_are_refused() {
        // Eines zu waehlen hiesse raten, und die falsche Wahl haenge ein
        // fremdes Array ein.
        let mut found = small_array(0xA1);
        found.extend(small_array(0xB2));

        let scan = group(found);
        assert_eq!(scan.arrays.len(), 2);
        match scan.pick(None) {
            Err(PickError::Several(uuids)) => assert_eq!(uuids.len(), 2),
            other => panic!("erwartet war Several, kam: {other:?}"),
        }
    }

    #[test]
    fn two_arrays_can_be_told_apart_by_uuid() {
        let mut found = small_array(0xA1);
        found.extend(small_array(0xB2));
        let scan = group(found);

        let wanted = scan.arrays.keys().next().unwrap().clone();
        assert_eq!(scan.pick(Some(&wanted)).unwrap().len(), 4);
    }

    #[test]
    fn an_array_that_is_not_there_is_named_in_the_error() {
        let scan = group(small_array(0xA1));
        assert_eq!(
            scan.pick(Some("gibt-es-nicht")),
            Err(PickError::NotFound("gibt-es-nicht".to_string()))
        );
    }

    #[test]
    fn nothing_found_says_where_it_looked() {
        let scan = group(Vec::new());
        let error = scan.pick(None).expect_err("nichts da");
        assert_eq!(error, PickError::Nothing);
        assert!(error.to_string().contains("scan"));
    }

    #[test]
    fn the_members_come_out_in_the_order_the_rest_of_the_project_uses() {
        // Data nach Slot, dann P, Q, Log — dieselbe Reihenfolge wie in
        // `member_files` und im Bericht. Ein Werkzeug, das sie mal so und mal
        // anders auflistet, macht das Vergleichen zweier Ausgaben zur Arbeit.
        let found = vec![
            member(0xA1, 4, Role::Log, 0, "/dev/l"),
            member(0xA1, 2, Role::Data, 1, "/dev/b"),
            member(0xA1, 3, Role::ParityP, 0, "/dev/p"),
            member(0xA1, 1, Role::Data, 0, "/dev/a"),
        ];
        let scan = group(found);
        let roles: Vec<Role> = scan
            .pick(None)
            .unwrap()
            .iter()
            .map(|member| member.superblock.role)
            .collect();
        assert_eq!(
            roles,
            vec![Role::Data, Role::Data, Role::ParityP, Role::Log]
        );
    }

    #[test]
    fn a_uuid_is_shortened_for_reading() {
        assert_eq!(short("3898328e-a28c-41e4-9c5a-267701f28c12"), "3898328e");
        assert_eq!(short("ohne-striche"), "ohne");
        assert_eq!(short("keine"), "keine");
    }
}
