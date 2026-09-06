// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Die Meldung, mit der der Broker arbeitet, und die Zuordnung von
//! Geraetenamen zu Data-Slots.
//!
//! Auch dies rechnet nur. Kein I/O, keine Uhrzeit, kein Zufall — Regel 8 gilt
//! sinngemaess.

/// Ein Bereich eines Data-Slots, dessen Inhalt nicht mehr stimmt.
///
/// Der Offset ist der in der Payload-Region des Members, also derselbe, den
/// ein Gast auf dem Blockgeraet benutzt. Warum das dasselbe ist, steht im
/// Modulkopf des Crates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DamageReport {
    pub slot_index: u16,
    pub offset: u64,
    pub len: usize,
}

impl DamageReport {
    /// Das erste Byte hinter dem Bereich.
    ///
    /// Saettigend: Ein Offset aus einer Kernelzeile ist ungeprueft, und ein
    /// Ueberlauf hier ergaebe einen Bereich, der vor sich selbst endet.
    pub fn end(&self) -> u64 {
        self.offset.saturating_add(self.len as u64)
    }
}

/// Fasst Meldungen zusammen, die denselben Slot betreffen und aneinander
/// stossen oder sich ueberlappen.
///
/// Ein Scrub meldet pro Sektor. Ohne diesen Schritt liest der Broker fuer
/// jeden einzelnen 4-KiB-Block alle Members einmal durch — bei einer
/// angefressenen Datei von einem Megabyte sind das 256 Durchlaeufe statt
/// einem. Die Rechnung ist identisch, nur die Anzahl der Lesevorgaenge nicht.
///
/// Bereiche verschiedener Slots werden nie zusammengefasst: Sie liegen auf
/// verschiedenen Platten, und ein gemeinsamer Bereich waere eine Erfindung.
pub fn coalesce(reports: &mut Vec<DamageReport>) {
    if reports.len() < 2 {
        return;
    }
    reports.sort_unstable();

    let mut merged: Vec<DamageReport> = Vec::with_capacity(reports.len());
    for report in reports.iter().copied() {
        match merged.last_mut() {
            Some(last) if last.slot_index == report.slot_index && report.offset <= last.end() => {
                let end = last.end().max(report.end());
                // Die Laenge kann auf einem 32-Bit-Ziel groesser werden als
                // `usize`. Dann wird nicht zusammengefasst — lieber zwei
                // Reparaturen als eine abgeschnittene.
                match usize::try_from(end - last.offset) {
                    Ok(len) => last.len = len,
                    Err(_) => merged.push(report),
                }
            }
            _ => merged.push(report),
        }
    }
    *reports = merged;
}

/// Welcher Geraetename gehoert zu welchem Data-Slot.
///
/// btrfs nennt das Geraet so, wie es beim Mounten hiess — meist
/// `/dev/ublkb0`. Verglichen wird zusaetzlich der letzte Pfadbestandteil,
/// damit `ublkb0` und `/dev/ublkb0` dieselbe Antwort geben. Weiter geht die
/// Nachsicht nicht: Ein Symlink oder ein `by-id`-Pfad ist ein anderer Name,
/// und ihn aufzuloesen hiesse, im Broker Dateisystem zu befragen.
#[derive(Debug, Clone, Default)]
pub struct SlotMap {
    entries: Vec<(String, u16)>,
}

impl SlotMap {
    pub fn new() -> Self {
        SlotMap::default()
    }

    /// Traegt ein Geraet ein. Ein zweiter Eintrag fuer denselben Namen
    /// ueberschreibt den ersten.
    pub fn insert(&mut self, device: impl Into<String>, slot_index: u16) {
        let device = device.into();
        match self.entries.iter_mut().find(|(name, _)| *name == device) {
            Some(entry) => entry.1 = slot_index,
            None => self.entries.push((device, slot_index)),
        }
    }

    /// Der Slot zu einem Geraetenamen, falls er zu diesem Array gehoert.
    pub fn slot_of(&self, device: &str) -> Option<u16> {
        let bare = basename(device);
        self.entries
            .iter()
            .find(|(name, _)| name == device || basename(name) == bare)
            .map(|(_, slot)| *slot)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Der letzte Pfadbestandteil, ohne Betriebssystem zu fragen.
fn basename(path: &str) -> &str {
    match path.rfind('/') {
        Some(position) => &path[position + 1..],
        None => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(slot_index: u16, offset: u64, len: usize) -> DamageReport {
        DamageReport {
            slot_index,
            offset,
            len,
        }
    }

    #[test]
    fn adjacent_ranges_of_one_slot_become_one() {
        let mut reports = vec![report(0, 4096, 4096), report(0, 8192, 4096)];
        coalesce(&mut reports);
        assert_eq!(reports, vec![report(0, 4096, 8192)]);
    }

    #[test]
    fn overlapping_ranges_become_one() {
        let mut reports = vec![report(0, 0, 8192), report(0, 4096, 8192)];
        coalesce(&mut reports);
        assert_eq!(reports, vec![report(0, 0, 12288)]);
    }

    #[test]
    fn a_range_contained_in_another_does_not_shorten_it() {
        let mut reports = vec![report(0, 0, 16384), report(0, 4096, 4096)];
        coalesce(&mut reports);
        assert_eq!(reports, vec![report(0, 0, 16384)]);
    }

    #[test]
    fn a_gap_keeps_two_ranges() {
        let mut reports = vec![report(0, 0, 4096), report(0, 8192, 4096)];
        coalesce(&mut reports);
        assert_eq!(reports, vec![report(0, 0, 4096), report(0, 8192, 4096)]);
    }

    #[test]
    fn ranges_of_different_slots_stay_apart() {
        let mut reports = vec![report(1, 0, 4096), report(0, 0, 4096)];
        coalesce(&mut reports);
        assert_eq!(
            reports,
            vec![report(0, 0, 4096), report(1, 0, 4096)],
            "verschiedene Platten, verschiedene Bereiche"
        );
    }

    #[test]
    fn an_unsorted_heap_comes_back_sorted_and_merged() {
        let mut reports = vec![
            report(1, 8192, 4096),
            report(0, 4096, 4096),
            report(1, 4096, 4096),
            report(0, 0, 4096),
        ];
        coalesce(&mut reports);
        assert_eq!(reports, vec![report(0, 0, 8192), report(1, 4096, 8192)]);
    }

    #[test]
    fn a_device_is_found_by_path_and_by_name() {
        let mut slots = SlotMap::new();
        slots.insert("/dev/ublkb0", 0);
        slots.insert("/dev/ublkb1", 1);

        assert_eq!(slots.slot_of("/dev/ublkb1"), Some(1));
        assert_eq!(slots.slot_of("ublkb1"), Some(1));
        assert_eq!(slots.slot_of("/dev/sda1"), None);
    }

    #[test]
    fn a_second_entry_for_the_same_device_replaces_the_first() {
        let mut slots = SlotMap::new();
        slots.insert("/dev/ublkb0", 0);
        slots.insert("/dev/ublkb0", 3);
        assert_eq!(slots.len(), 1);
        assert_eq!(slots.slot_of("/dev/ublkb0"), Some(3));
    }
}
