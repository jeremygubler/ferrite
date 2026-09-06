// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Was ein Mensch ueber ein Array lesen soll — und wie streng es zu bewerten
//! ist.
//!
//! Auch dies rechnet nur. Die Superbloecke kommen als Parameter herein, der
//! Text geht als `String` hinaus; kein Geraet, keine Ausgabe, keine Uhrzeit.
//! Deshalb laesst sich jede Zeile dieses Berichts pruefen, ohne eine Platte zu
//! haben — auch die, die einen Ausfall beschreibt.

use ferrite_format::assemble;
use ferrite_format::superblock::{MemberState, Role, Superblock};

/// Ein Geraet, so wie es angesehen wurde.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub device: String,
    /// `None`, wenn dort kein gueltiger Ferrite-Superblock liegt.
    pub superblock: Option<Superblock>,
}

/// Wie es um das Array steht.
///
/// Die Reihenfolge ist die Rangfolge: `Broken` schlaegt `Degraded` schlaegt
/// `Healthy`. Der Rueckgabewert des Programms haengt daran, und ein Werkzeug,
/// das bei einem fehlenden Slot `0` meldet, faellt in jedem Monitoring durch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Health {
    Healthy,
    /// Nutzbar, aber ohne die volle Redundanz: ein Member `Stale` oder mitten
    /// im Rebuild.
    Degraded,
    /// Laesst sich so nicht zusammensetzen.
    Broken,
}

impl Health {
    pub fn exit_code(self) -> u8 {
        match self {
            Health::Healthy => 0,
            Health::Degraded => 1,
            Health::Broken => 2,
        }
    }
}

/// Der fertige Bericht.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub text: String,
    pub health: Health,
}

/// Baut den Bericht ueber die angesehenen Geraete.
pub fn status(seen: &[Seen]) -> Report {
    let mut text = String::new();
    let mut health = Health::Healthy;

    let superblocks: Vec<Superblock> = seen
        .iter()
        .filter_map(|entry| entry.superblock.clone())
        .collect();

    if superblocks.is_empty() {
        text.push_str("Auf keinem der angegebenen Geraete liegt ein Ferrite-Superblock.\n");
        return Report {
            text,
            health: Health::Broken,
        };
    }

    // Geraete ohne Superblock sind kein Fehler des Arrays, aber der Aufrufer
    // hat sie genannt und soll erfahren, dass sie nicht dazugehoeren.
    for entry in seen.iter().filter(|entry| entry.superblock.is_none()) {
        text.push_str(&format!(
            "  {}: kein Ferrite-Superblock — gehoert nicht dazu\n",
            entry.device
        ));
    }

    let first = &superblocks[0];
    text.push_str(&format!(
        "Array {}\n  Blockgroesse {}, {} Data-Slots\n",
        first.array_uuid,
        size(first.parity_block_size()),
        first.data_slot_count
    ));

    let mut rows: Vec<(u8, u16, String)> = Vec::new();
    for entry in seen {
        let Some(superblock) = &entry.superblock else {
            continue;
        };
        // Ein Superblock aus einem **anderen** Array. Das ist der Fall, in dem
        // jemand die Platte eines fremden Pools angeschlossen hat, und er
        // gehoert deutlich gemeldet: Wer ihn uebersieht, nimmt sie versehentlich
        // in dieses Array auf.
        if superblock.array_uuid != first.array_uuid {
            health = health.max(Health::Broken);
            rows.push((
                4,
                0,
                format!(
                    "  {}: gehoert zu Array {} — nicht zu diesem\n",
                    entry.device, superblock.array_uuid
                ),
            ));
            continue;
        }

        let state = state_of(superblock);
        if superblock.role == Role::Data && superblock.member_state != MemberState::Clean {
            health = health.max(Health::Degraded);
        }
        rows.push((
            order_of(superblock.role),
            superblock.slot_index,
            format!(
                "  {:<9} {:<20} {:<24} {}\n",
                name_of(superblock),
                entry.device,
                state,
                size(superblock.payload_size)
            ),
        ));
    }

    rows.sort_by_key(|(role, slot, _)| (*role, *slot));
    for (_, _, line) in rows {
        text.push_str(&line);
    }

    // Und zum Schluss die Frage, die der Aufrufer wirklich hat: Laesst sich
    // daraus ein Array machen? `assemble` ist dieselbe Pruefung, die auch das
    // Oeffnen macht — hier wird nichts nachgebaut.
    match assemble(&superblocks) {
        Ok(_) => {
            let summary = match health {
                Health::Healthy => "Alle Members in Ordnung.",
                Health::Degraded => {
                    "Das Array laeuft degradiert: mindestens ein Member traegt keine gueltigen \
                     Daten. Reads werden rekonstruiert; ein weiterer Ausfall kann mehr kosten, \
                     als die Paritaet abdeckt."
                }
                Health::Broken => "Fremde Members in der Liste.",
            };
            text.push_str(&format!("\n{summary}\n"));
        }
        Err(error) => {
            health = Health::Broken;
            text.push_str(&format!(
                "\nDiese Geraete ergeben kein Array: {error}\n\
                 Fehlt eine Platte in der Liste, oder ist sie ausgefallen?\n"
            ));
        }
    }

    Report { text, health }
}

/// Wie ein Member in der Liste heisst.
fn name_of(superblock: &Superblock) -> String {
    match superblock.role {
        Role::Data => format!("Slot {}", superblock.slot_index),
        Role::ParityP => "ParityP".to_string(),
        Role::ParityQ => "ParityQ".to_string(),
        Role::Log => "Log".to_string(),
    }
}

/// Data zuerst, dann P, Q, Log — dieselbe Reihenfolge wie ueberall im Projekt.
fn order_of(role: Role) -> u8 {
    match role {
        Role::Data => 0,
        Role::ParityP => 1,
        Role::ParityQ => 2,
        Role::Log => 3,
    }
}

fn state_of(superblock: &Superblock) -> String {
    match superblock.member_state {
        MemberState::Clean => "in Ordnung".to_string(),
        MemberState::Stale => "unbrauchbar, wartet auf Rebuild".to_string(),
        MemberState::Rebuilding => {
            // `rebuild_progress` steht in **Bytes**, nicht in Bloecken
            // (Abschnitt 2.1). Wer die Einheit verwechselt, zeigt einen
            // Fortschritt von 0 %, waehrend die halbe Platte schon steht.
            let block = superblock.parity_block_size();
            let done = superblock.rebuild_progress.min(superblock.payload_size);
            let percent = done
                .saturating_mul(100)
                .checked_div(superblock.payload_size)
                .unwrap_or(100);
            format!(
                "Rebuild bei {percent} % ({} von {} Bloecken)",
                done / block,
                superblock.payload_size / block
            )
        }
    }
}

/// Ein Geraet, so wie `create` es einplant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub device: String,
    pub role: Role,
    pub slot_index: u16,
    /// Die Groesse des Geraets, wie sie das Betriebssystem meldet.
    pub device_size: u64,
    /// Was davon Nutzdaten werden — abgerundet auf ganze Parity-Bloecke.
    pub payload_size: u64,
    /// Liegt dort schon ein Ferrite-Superblock?
    pub occupied: bool,
}

/// Was `create` vorhat, als Text.
///
/// **Steht vor jedem Schreibvorgang.** Ein Kommando, das Platten
/// ueberschreibt, soll vorher zeigen, welche — und zwar so, dass ein falscher
/// Buchstabe im Geraetenamen ins Auge faellt, bevor er wirkt.
pub fn plan(entries: &[Planned], block_size_log2: u8, confirmed: bool) -> String {
    let mut text = String::new();
    text.push_str(&format!(
        "Vorhaben: {} Data-Slots, Blockgroesse {}\n",
        entries
            .iter()
            .filter(|entry| entry.role == Role::Data)
            .count(),
        size(1u64 << block_size_log2)
    ));

    for entry in entries {
        text.push_str(&format!(
            "  {:<9} {:<20} {:>10} Geraet, {:>10} nutzbar{}\n",
            match entry.role {
                Role::Data => format!("Slot {}", entry.slot_index),
                Role::ParityP => "ParityP".to_string(),
                Role::ParityQ => "ParityQ".to_string(),
                Role::Log => "Log".to_string(),
            },
            entry.device,
            size(entry.device_size),
            size(entry.payload_size),
            if entry.occupied {
                "  ← traegt bereits ein Ferrite-Array"
            } else {
                ""
            }
        ));
    }

    if !entries.iter().any(|entry| entry.role == Role::ParityQ) {
        text.push_str(
            "\nOhne ParityQ ueberlebt das Array genau einen Plattenausfall.\n\
             Mit --parity-q sind es zwei.\n",
        );
    }

    text.push_str(if confirmed {
        "\nDiese Geraete werden jetzt beschrieben. Alles, was darauf liegt, ist danach weg.\n"
    } else {
        "\nEs wurde nichts geschrieben. Zum Ausfuehren dasselbe Kommando mit --yes.\n"
    });
    text
}

/// Eine Groesse, wie ein Mensch sie liest.
///
/// Binaerpraefixe, weil Plattengroessen im Superblock in Bytes stehen und
/// jedes Werkzeug darunter — `blockdev`, `statvfs`, `btrfs` — ebenfalls
/// binaer rechnet. Wer hier auf Dezimalpraefixe wechselte, produzierte
/// Zahlen, die zu keinem anderen Ausdruck passen.
pub fn size(bytes: u64) -> String {
    const UNITS: [(&str, u64); 5] = [
        ("TiB", 1 << 40),
        ("GiB", 1 << 30),
        ("MiB", 1 << 20),
        ("KiB", 1 << 10),
        ("B", 1),
    ];
    for (unit, factor) in UNITS {
        if bytes >= factor {
            // Eine Nachkommastelle: Zwei taeuschen eine Genauigkeit vor, die
            // bei Plattengroessen niemanden interessiert, null verwischt den
            // Unterschied zwischen 1,9 und 2,0 TiB.
            let whole = bytes / factor;
            let tenth = (bytes % factor) * 10 / factor;
            return if tenth == 0 || *unit == *"B" {
                format!("{whole} {unit}")
            } else {
                format!("{whole},{tenth} {unit}")
            };
        }
    }
    "0 B".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrite_format::Uuid;

    fn member(role: Role, slot: u16, state: MemberState) -> Superblock {
        let mut superblock = Superblock::new(
            Uuid::from_random_bytes([0xA1; 16]),
            Uuid::from_random_bytes([role as u8 * 16 + slot as u8 + 1; 16]),
            role,
            2,
            8 << 20,
        );
        superblock.slot_index = slot;
        superblock.member_state = state;
        superblock
    }

    fn seen(device: &str, superblock: Superblock) -> Seen {
        Seen {
            device: device.to_string(),
            superblock: Some(superblock),
        }
    }

    fn healthy_array() -> Vec<Seen> {
        vec![
            seen("/dev/a", member(Role::Data, 0, MemberState::Clean)),
            seen("/dev/b", member(Role::Data, 1, MemberState::Clean)),
            seen("/dev/p", member(Role::ParityP, 0, MemberState::Clean)),
            seen("/dev/l", member(Role::Log, 0, MemberState::Clean)),
        ]
    }

    #[test]
    fn a_healthy_array_reports_zero() {
        let report = status(&healthy_array());
        assert_eq!(report.health, Health::Healthy);
        assert_eq!(report.health.exit_code(), 0);
        assert!(report.text.contains("Alle Members in Ordnung"));
    }

    #[test]
    fn every_member_appears_with_its_device() {
        let report = status(&healthy_array());
        for device in ["/dev/a", "/dev/b", "/dev/p", "/dev/l"] {
            assert!(report.text.contains(device), "{device} fehlt im Bericht");
        }
        assert!(report.text.contains("Slot 0"));
        assert!(report.text.contains("ParityP"));
        assert!(report.text.contains("Log"));
    }

    #[test]
    fn a_stale_member_makes_the_array_degraded() {
        let mut members = healthy_array();
        members[1] = seen("/dev/b", member(Role::Data, 1, MemberState::Stale));

        let report = status(&members);
        assert_eq!(report.health, Health::Degraded);
        assert_eq!(report.health.exit_code(), 1);
        assert!(report.text.contains("wartet auf Rebuild"));
        assert!(
            report.text.contains("laeuft degradiert"),
            "der Zustand muss im Klartext dastehen, nicht nur im Rueckgabewert"
        );
    }

    #[test]
    fn a_missing_slot_is_not_an_array() {
        // Die Platte wurde gar nicht erst angegeben — oder sie antwortet
        // nicht mehr. Beides sieht von hier gleich aus, und beides heisst:
        // So laesst sich nichts zusammensetzen.
        let mut members = healthy_array();
        members.remove(1);

        let report = status(&members);
        assert_eq!(report.health, Health::Broken);
        assert_eq!(report.health.exit_code(), 2);
        assert!(report.text.contains("ergeben kein Array"));
    }

    #[test]
    fn a_disk_from_another_array_is_named_and_not_silently_ignored() {
        // Der gefaehrliche Fall: Jemand haengt die Platte eines fremden Pools
        // an. Wer sie uebersieht, nimmt sie versehentlich auf.
        let mut foreign = member(Role::Data, 0, MemberState::Clean);
        foreign.array_uuid = Uuid::from_random_bytes([0xFF; 16]);

        let mut members = healthy_array();
        members.push(seen("/dev/fremd", foreign));

        let report = status(&members);
        assert_eq!(report.health, Health::Broken);
        assert!(report.text.contains("/dev/fremd"));
        assert!(report.text.contains("gehoert zu Array"));
    }

    #[test]
    fn a_device_without_a_superblock_is_mentioned() {
        let mut members = healthy_array();
        members.push(Seen {
            device: "/dev/leer".to_string(),
            superblock: None,
        });

        let report = status(&members);
        assert!(report.text.contains("/dev/leer"));
        assert!(report.text.contains("kein Ferrite-Superblock"));
        assert_eq!(
            report.health,
            Health::Healthy,
            "ein fremdes Geraet in der Liste macht das Array nicht kaputt"
        );
    }

    #[test]
    fn nothing_at_all_is_reported_as_such() {
        let report = status(&[Seen {
            device: "/dev/leer".to_string(),
            superblock: None,
        }]);
        assert_eq!(report.health, Health::Broken);
        assert!(report.text.contains("keinem der angegebenen Geraete"));
    }

    #[test]
    fn a_rebuild_shows_how_far_it_got() {
        let mut rebuilding = member(Role::Data, 1, MemberState::Rebuilding);
        // 8 MiB Payload bei 64-KiB-Bloecken sind 128 Bloecke. Der Fortschritt
        // steht in Bytes: 2 MiB sind 32 Bloecke und damit ein Viertel.
        rebuilding.rebuild_progress = 2 << 20;

        let mut members = healthy_array();
        members[1] = seen("/dev/b", rebuilding);

        let report = status(&members);
        assert_eq!(report.health, Health::Degraded);
        assert!(
            report
                .text
                .contains("Rebuild bei 25 % (32 von 128 Bloecken)"),
            "der Fortschritt fehlt oder rechnet in der falschen Einheit: {}",
            report.text
        );
    }

    #[test]
    fn a_rebuild_that_just_started_is_not_reported_as_finished() {
        // Der Fehler, der bei der falschen Einheit entsteht: Wer Bytes fuer
        // Bloecke haelt, sieht bei 0 von 128 Bloecken dasselbe wie bei 128.
        let mut rebuilding = member(Role::Data, 1, MemberState::Rebuilding);
        rebuilding.rebuild_progress = 0;

        let mut members = healthy_array();
        members[1] = seen("/dev/b", rebuilding);
        assert!(status(&members)
            .text
            .contains("Rebuild bei 0 % (0 von 128 Bloecken)"));
    }

    #[test]
    fn the_members_come_out_in_a_fixed_order() {
        // Die Eingabereihenfolge ist die der Kommandozeile und damit beliebig.
        // Ein Bericht, der sie uebernimmt, sieht bei jedem Aufruf anders aus.
        let mut shuffled = healthy_array();
        shuffled.reverse();
        assert_eq!(status(&shuffled).text, status(&healthy_array()).text);
    }

    #[test]
    fn sizes_are_written_the_way_the_other_tools_write_them() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(512), "512 B");
        assert_eq!(size(1 << 10), "1 KiB");
        assert_eq!(size(64 << 10), "64 KiB");
        assert_eq!(size(1 << 30), "1 GiB");
        assert_eq!(size((1 << 30) + (1 << 29)), "1,5 GiB");
        assert_eq!(size(2 << 40), "2 TiB");
    }

    fn planned(device: &str, role: Role, slot: u16, occupied: bool) -> Planned {
        Planned {
            device: device.to_string(),
            role,
            slot_index: slot,
            device_size: 2 << 30,
            payload_size: (2 << 30) - (2 << 20),
            occupied,
        }
    }

    fn simple_plan() -> Vec<Planned> {
        vec![
            planned("/dev/a", Role::Data, 0, false),
            planned("/dev/b", Role::Data, 1, false),
            planned("/dev/p", Role::ParityP, 0, false),
            planned("/dev/l", Role::Log, 0, false),
        ]
    }

    #[test]
    fn the_plan_names_every_device_before_anything_is_written() {
        let text = plan(&simple_plan(), 16, false);
        for device in ["/dev/a", "/dev/b", "/dev/p", "/dev/l"] {
            assert!(text.contains(device), "{device} fehlt im Plan");
        }
        assert!(text.contains("2 Data-Slots"));
        assert!(text.contains("64 KiB"));
    }

    #[test]
    fn without_yes_the_plan_says_that_nothing_happened() {
        let text = plan(&simple_plan(), 16, false);
        assert!(
            text.contains("nichts geschrieben"),
            "der Trockenlauf muss sich als solcher zu erkennen geben"
        );
        assert!(text.contains("--yes"));
    }

    #[test]
    fn with_yes_the_plan_says_what_is_about_to_be_lost() {
        let text = plan(&simple_plan(), 16, true);
        assert!(text.contains("ist danach weg"));
        assert!(!text.contains("nichts geschrieben"));
    }

    #[test]
    fn a_device_that_already_carries_an_array_is_marked_in_the_plan() {
        let mut entries = simple_plan();
        entries[0].occupied = true;
        let text = plan(&entries, 16, false);
        assert!(text.contains("traegt bereits ein Ferrite-Array"));
    }

    #[test]
    fn a_plan_without_q_says_what_that_costs() {
        // Die Entscheidung faellt genau hier, und danach nur noch mit einem
        // neuen Array. Sie darf nicht unerwaehnt durchlaufen.
        assert!(plan(&simple_plan(), 16, false).contains("genau einen Plattenausfall"));

        let mut entries = simple_plan();
        entries.push(planned("/dev/q", Role::ParityQ, 0, false));
        assert!(!plan(&entries, 16, false).contains("genau einen Plattenausfall"));
    }

    #[test]
    fn a_size_just_below_a_unit_does_not_round_up_into_it() {
        // 1023 MiB sind nicht 1 GiB. Wer hier rundet, zeigt eine volle Platte
        // als leer an.
        assert_eq!(size((1 << 30) - 1), "1023,9 MiB");
    }
}
