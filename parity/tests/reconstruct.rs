// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Rekonstruktionstests gegen Abschnitt 2 von `docs/FORMAT.md`.
//!
//! Der Zufall laeuft ueber denselben festen LCG wie in
//! `format/tests/roundtrip.rs`: reproduzierbar, ohne Test-Dependency, in
//! Millisekunden durch. Geprueft wird nicht, ob die Rekonstruktion *plausibel*
//! aussieht, sondern ob sie byteweise das Original ergibt — alles andere waere
//! in einem Speicherprojekt wertlos.

use ferrite_parity::{
    compute_p, compute_q, reconstruct_data_and_p, reconstruct_data_and_q, reconstruct_from_p,
    reconstruct_from_q, reconstruct_two_from_pq, ParityError, Slot,
};

struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed)
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 1
    }
    fn below(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound
    }
    fn bytes(&mut self, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        for byte in out.iter_mut() {
            *byte = (self.next_u64() & 0xFF) as u8;
        }
        out
    }
}

/// Ein Array aus `data.len()` Data-Slots samt frisch gerechneter Paritaet.
struct Array {
    data: Vec<Vec<u8>>,
    p: Vec<u8>,
    q: Vec<u8>,
}

impl Array {
    /// `data` in der Reihenfolge der Slot-Indizes, Laengen duerfen abweichen.
    fn new(data: Vec<Vec<u8>>) -> Self {
        let parity_len = data.iter().map(Vec::len).max().unwrap_or(0);
        let mut p = vec![0u8; parity_len];
        let mut q = vec![0u8; parity_len];
        {
            let slots = slots_of(&data, &[]);
            compute_p(data.len() as u8, &slots, &mut p).expect("P muss rechenbar sein");
            compute_q(data.len() as u8, &slots, &mut q).expect("Q muss rechenbar sein");
        }
        Array { data, p, q }
    }

    fn count(&self) -> u8 {
        self.data.len() as u8
    }

    fn survivors_without(&self, lost: &[u8]) -> Vec<Slot<'_>> {
        slots_of(&self.data, lost)
    }
}

fn slots_of<'a>(data: &'a [Vec<u8>], lost: &[u8]) -> Vec<Slot<'a>> {
    data.iter()
        .enumerate()
        .filter(|(index, _)| !lost.contains(&(*index as u8)))
        .map(|(index, bytes)| Slot::new(index as u8, bytes).expect("Index unter 64"))
        .collect()
}

/// Alle fuenf Rekonstruktionsfaelle fuer dieses Array durchspielen.
fn check_all_reconstructions(array: &Array, label: &str) {
    let count = array.count();

    for lost in 0..count {
        let original = &array.data[lost as usize];
        let survivors = array.survivors_without(&[lost]);

        // Ein fehlender Data-Slot aus P.
        let mut out = vec![0u8; original.len()];
        reconstruct_from_p(count, lost, &survivors, &array.p, &mut out)
            .expect("Rekonstruktion aus P");
        assert_eq!(&out, original, "{label}: Slot {lost} aus P");

        // Ein fehlender Data-Slot aus Q.
        let mut out = vec![0u8; original.len()];
        reconstruct_from_q(count, lost, &survivors, &array.q, &mut out)
            .expect("Rekonstruktion aus Q");
        assert_eq!(&out, original, "{label}: Slot {lost} aus Q");

        // Ein Data-Slot plus P: Data kommt aus Q, P danach neu.
        let mut out = vec![0u8; original.len()];
        let mut rebuilt_p = vec![0u8; array.p.len()];
        reconstruct_data_and_p(count, lost, &survivors, &array.q, &mut out, &mut rebuilt_p)
            .expect("Rekonstruktion Data + P");
        assert_eq!(&out, original, "{label}: Slot {lost} aus Q (mit P-Verlust)");
        assert_eq!(
            rebuilt_p, array.p,
            "{label}: P nach Verlust von Slot {lost}"
        );

        // Ein Data-Slot plus Q: Data kommt aus P, Q danach neu.
        let mut out = vec![0u8; original.len()];
        let mut rebuilt_q = vec![0u8; array.q.len()];
        reconstruct_data_and_q(count, lost, &survivors, &array.p, &mut out, &mut rebuilt_q)
            .expect("Rekonstruktion Data + Q");
        assert_eq!(&out, original, "{label}: Slot {lost} aus P (mit Q-Verlust)");
        assert_eq!(
            rebuilt_q, array.q,
            "{label}: Q nach Verlust von Slot {lost}"
        );
    }

    // Zwei fehlende Data-Slots aus P und Q, alle Paare.
    for first in 0..count {
        for second in (first + 1)..count {
            let survivors = array.survivors_without(&[first, second]);
            let original_first = &array.data[first as usize];
            let original_second = &array.data[second as usize];

            let mut out_first = vec![0u8; original_first.len()];
            let mut out_second = vec![0u8; original_second.len()];
            reconstruct_two_from_pq(
                count,
                first,
                second,
                &survivors,
                &array.p,
                &array.q,
                &mut out_first,
                &mut out_second,
            )
            .expect("Rekonstruktion zweier Slots");

            assert_eq!(
                &out_first, original_first,
                "{label}: Paar ({first},{second})"
            );
            assert_eq!(
                &out_second, original_second,
                "{label}: Paar ({first},{second})"
            );
        }
    }
}

#[test]
fn parity_matches_the_definition_from_section_two() {
    // Direkt gegen die Formel gerechnet, nicht gegen die Implementierung.
    // Die Slots 1 und 2 sind leer — ein Array aus vier Members, von denen zwei
    // noch nichts tragen. Sie muessen trotzdem uebergeben werden.
    let a = [0x01u8, 0x02, 0x03];
    let b = [0x10u8, 0x20, 0x30];
    let empty: [u8; 0] = [];
    let slots = [
        Slot::new(0, &a).unwrap(),
        Slot::new(1, &empty).unwrap(),
        Slot::new(2, &empty).unwrap(),
        Slot::new(3, &b).unwrap(), // Slot-Index 3, nicht Position 1
    ];

    let mut p = [0u8; 3];
    compute_p(4, &slots, &mut p).unwrap();
    assert_eq!(p, [0x11, 0x22, 0x33]);

    let mut q = [0u8; 3];
    compute_q(4, &slots, &mut q).unwrap();
    for offset in 0..3 {
        let expected = ferrite_parity::gf::mul(ferrite_parity::gf::g_pow(0), a[offset])
            ^ ferrite_parity::gf::mul(ferrite_parity::gf::g_pow(3), b[offset]);
        assert_eq!(q[offset], expected, "Q an Offset {offset}");
    }
}

#[test]
fn q_uses_the_slot_index_not_the_position() {
    // Derselbe Inhalt, einmal auf Slot 0 und einmal auf Slot 5. Q muss sich
    // unterscheiden, sonst haengt der Koeffizient an der falschen Groesse.
    let payload = [0xABu8, 0xCD, 0xEF, 0x01];
    let empty: [u8; 0] = [];

    let mut q_low = [0u8; 4];
    compute_q(1, &[Slot::new(0, &payload).unwrap()], &mut q_low).unwrap();

    // Sechs Slots, die Nutzdaten liegen auf dem letzten. Nur der Index
    // unterscheidet die beiden Faelle.
    let high: Vec<Slot<'_>> = (0..5)
        .map(|index| Slot::new(index, &empty).unwrap())
        .chain(std::iter::once(Slot::new(5, &payload).unwrap()))
        .collect();
    let mut q_high = [0u8; 4];
    compute_q(6, &high, &mut q_high).unwrap();

    assert_eq!(q_low, payload, "g^0 ist 1, Q muss die Daten selbst sein");
    assert_ne!(q_low, q_high);
}

#[test]
fn equal_length_slots_survive_every_erasure() {
    let mut rng = Lcg::new(0x5EED_0001);
    for count in 4..=8usize {
        let len = 1 + rng.below(2000) as usize;
        let data: Vec<Vec<u8>> = (0..count).map(|_| rng.bytes(len)).collect();
        let array = Array::new(data);
        check_all_reconstructions(&array, &format!("{count} Slots, je {len} Bytes"));
    }
}

#[test]
fn unequal_length_slots_survive_every_erasure() {
    // Der eigentliche Punkt des Layouts: gemischte Plattengroessen. Ein Slot
    // ist immer leer, einer immer der laengste.
    let mut rng = Lcg::new(0x5EED_0002);
    for count in 4..=8usize {
        let longest = 1 + rng.below(3000) as usize;
        let mut data: Vec<Vec<u8>> = (0..count)
            .map(|_| {
                let len = rng.below(longest as u64 + 1) as usize;
                rng.bytes(len)
            })
            .collect();
        data[0] = Vec::new();
        data[count - 1] = rng.bytes(longest);

        let array = Array::new(data);
        check_all_reconstructions(&array, &format!("{count} Slots, ungleich lang"));
    }
}

#[test]
fn a_slot_of_length_zero_round_trips() {
    // Ein leerer Slot ist jenseits von Offset 0 ganz Zero-Extension. Er darf
    // die Rechnung nicht veraendern und muss sich rekonstruieren lassen.
    let a = [1u8, 2, 3, 4, 5];
    let empty: [u8; 0] = [];
    let c = [9u8, 8, 7];

    let slots = [
        Slot::new(0, &a).unwrap(),
        Slot::new(1, &empty).unwrap(),
        Slot::new(2, &c).unwrap(),
    ];

    // Von Hand gerechnet, ohne den leeren Slot. Kommt dasselbe heraus, hat er
    // zur Summe nichts beigetragen — genau das verlangt die Zero-Extension.
    let mut p = [0u8; 5];
    compute_p(3, &slots, &mut p).unwrap();
    for (offset, &actual) in p.iter().enumerate() {
        let expected = a.get(offset).copied().unwrap_or(0) ^ c.get(offset).copied().unwrap_or(0);
        assert_eq!(actual, expected, "P an Offset {offset}");
    }

    let mut q = [0u8; 5];
    compute_q(3, &slots, &mut q).unwrap();
    for (offset, &actual) in q.iter().enumerate() {
        let expected = ferrite_parity::gf::mul(
            ferrite_parity::gf::g_pow(0),
            a.get(offset).copied().unwrap_or(0),
        ) ^ ferrite_parity::gf::mul(
            ferrite_parity::gf::g_pow(2),
            c.get(offset).copied().unwrap_or(0),
        );
        assert_eq!(actual, expected, "Q an Offset {offset}");
    }

    let array = Array::new(vec![a.to_vec(), Vec::new(), c.to_vec()]);
    check_all_reconstructions(&array, "mit leerem Slot");
}

#[test]
fn an_all_zero_slot_is_reconstructed_as_zero() {
    // Ein Slot, der nur Nullen enthaelt, ist rechnerisch nicht von einem
    // fehlenden zu unterscheiden — genau deshalb muss er getestet werden.
    let mut rng = Lcg::new(0x5EED_0003);
    let mut data: Vec<Vec<u8>> = (0..5).map(|_| rng.bytes(777)).collect();
    data[2] = vec![0u8; 777];

    let array = Array::new(data);
    check_all_reconstructions(&array, "mit Nullslot");

    let survivors = array.survivors_without(&[2]);
    let mut out = vec![0xFFu8; 777];
    reconstruct_from_p(5, 2, &survivors, &array.p, &mut out).unwrap();
    assert!(out.iter().all(|&byte| byte == 0));
}

#[test]
fn a_zero_slot_next_to_a_shorter_neighbour_is_reconstructed() {
    // Nullslot und Zero-Extension zusammen: Der Nullslot ist laenger als sein
    // Nachbar, beide liefern jenseits ihres Endes dasselbe.
    let a = [0u8; 64];
    let b = [7u8; 16];
    let array = Array::new(vec![a.to_vec(), b.to_vec()]);
    check_all_reconstructions(&array, "Nullslot neben kuerzerem Slot");
}

#[test]
fn parity_is_at_least_as_long_as_every_data_slot() {
    // Bedingung 1 aus Abschnitt 2. Ein zu kurzer Parity-Member ist kein
    // Randfall, den man abschneiden darf — die Paritaet waere falsch.
    let long = [1u8; 100];
    let slots = [Slot::new(0, &long).unwrap()];
    let mut too_short = [0u8; 50];
    assert!(matches!(
        compute_p(1, &slots, &mut too_short),
        Err(ParityError::SlotLongerThanParity {
            index: 0,
            slot_len: 100,
            parity_len: 50
        })
    ));
}

#[test]
fn an_incomplete_survivor_set_is_refused() {
    // Der gefaehrlichste Aufruferfehler: Ein Slot wird vergessen, die
    // Rekonstruktion laeuft durch und liefert plausiblen Muell.
    let array = Array::new(vec![vec![1u8; 32], vec![2u8; 32], vec![3u8; 32]]);
    let mut incomplete = array.survivors_without(&[0]);
    incomplete.pop();

    let mut out = [0u8; 32];
    assert!(matches!(
        reconstruct_from_p(3, 0, &incomplete, &array.p, &mut out),
        Err(ParityError::IncompleteSlotSet {
            expected: 2,
            got: 1
        })
    ));
}

#[test]
fn the_lost_slot_must_not_appear_among_the_survivors() {
    let array = Array::new(vec![vec![1u8; 16], vec![2u8; 16]]);
    let all = array.survivors_without(&[]);

    let mut out = [0u8; 16];
    assert!(matches!(
        reconstruct_from_p(2, 0, &all, &array.p, &mut out),
        Err(ParityError::LostSlotAmongSurvivors { index: 0 })
    ));
}

#[test]
fn duplicate_slot_indices_are_refused() {
    let payload = [1u8; 8];
    let slots = [
        Slot::new(2, &payload).unwrap(),
        Slot::new(2, &payload).unwrap(),
    ];
    let mut p = [0u8; 8];
    assert!(matches!(
        compute_p(3, &slots, &mut p),
        Err(ParityError::DuplicateSlot { index: 2 })
    ));
}

#[test]
fn slot_indices_beyond_the_format_limit_are_refused() {
    let payload = [0u8; 4];
    assert!(matches!(
        Slot::new(64, &payload),
        Err(ParityError::SlotIndexOutOfRange { index: 64 })
    ));
    assert!(Slot::new(63, &payload).is_ok());
}

#[test]
fn the_same_slot_cannot_be_lost_twice() {
    let array = Array::new(vec![vec![1u8; 8], vec![2u8; 8], vec![3u8; 8]]);
    let survivors = array.survivors_without(&[1]);
    let mut first = [0u8; 8];
    let mut second = [0u8; 8];
    assert!(matches!(
        reconstruct_two_from_pq(
            3,
            1,
            1,
            &survivors,
            &array.p,
            &array.q,
            &mut first,
            &mut second
        ),
        Err(ParityError::SameSlotTwice { index: 1 })
    ));
}

#[test]
fn a_parity_buffer_that_is_too_short_is_refused() {
    let array = Array::new(vec![vec![1u8; 64], vec![2u8; 64]]);
    let survivors = array.survivors_without(&[0]);
    let mut out = [0u8; 64];
    assert!(matches!(
        reconstruct_from_p(2, 0, &survivors, &array.p[..32], &mut out),
        Err(ParityError::BufferTooSmall {
            what: "p",
            need: 64,
            got: 32
        })
    ));
}

#[test]
fn reconstruction_spans_more_than_one_scratch_window() {
    // Die Zwei-Slot-Rekonstruktion arbeitet in Scheiben. Ein Array, das
    // mehrere davon fuellt und an keiner Scheibengrenze endet, prueft, dass die
    // Schleife die Raender richtig setzt.
    let mut rng = Lcg::new(0x5EED_0004);
    let long = 4096 * 2 + 1234;
    let data = vec![
        rng.bytes(long),
        rng.bytes(4096),
        rng.bytes(long - 7),
        rng.bytes(1),
    ];
    let array = Array::new(data);
    check_all_reconstructions(&array, "ueber mehrere Scheiben");
}

#[test]
fn computing_parity_over_an_incomplete_slot_set_is_refused() {
    // Die Gegenseite zu `an_incomplete_survivor_set_is_refused`: Wer beim
    // Rechnen einen Slot vergisst, bekommt eine Paritaet, die zu nichts passt.
    // Sie sieht gueltig aus und faellt erst auf, wenn jemand daraus
    // rekonstruiert — also genau dann, wenn es darauf ankommt.
    let a = [1u8; 16];
    let b = [2u8; 16];
    let slots = [Slot::new(0, &a).unwrap(), Slot::new(1, &b).unwrap()];

    let mut p = [0u8; 16];
    assert!(matches!(
        compute_p(3, &slots, &mut p),
        Err(ParityError::IncompleteSlotSet {
            expected: 3,
            got: 2
        })
    ));

    let mut q = [0u8; 16];
    assert!(matches!(
        compute_q(3, &slots, &mut q),
        Err(ParityError::IncompleteSlotSet {
            expected: 3,
            got: 2
        })
    ));
}

#[test]
fn computing_parity_with_an_out_of_range_slot_count_is_refused() {
    let a = [1u8; 8];
    let slots = [Slot::new(0, &a).unwrap()];
    let mut p = [0u8; 8];
    assert!(matches!(
        compute_p(0, &slots, &mut p),
        Err(ParityError::InvalidSlotCount { count: 0 })
    ));
    assert!(matches!(
        compute_p(65, &slots, &mut p),
        Err(ParityError::InvalidSlotCount { count: 65 })
    ));
}

/// Q gegen die Definition aus Abschnitt 2, nicht gegen die Implementierung.
///
/// `compute_q` rechnet seit dem Horner-Umbau nicht mehr Byte fuer Byte mit
/// `g^j` aus der Tabelle, sondern verdoppelt einen Zwischenstand. Das ist
/// dieselbe Summe — dieser Test ist der Beweis, und er bleibt richtig, egal wie
/// die Funktion darunter arbeitet.
#[test]
fn q_matches_the_definition_over_many_shapes() {
    let mut rng = Lcg::new(0x0A17_C0DE);

    for count in 1..=12u8 {
        for round in 0..8 {
            // Ungleiche Laengen, darunter leere Slots und ein Fall, der ueber
            // mehrere Arbeitsscheiben laeuft.
            let longest = match round {
                0 => 1,
                1 => 4095,
                2 => 4096,
                3 => 4097,
                4 => 2 * 4096 + 1234,
                _ => 1 + rng.below(9000) as usize,
            };
            let payloads: Vec<Vec<u8>> = (0..count)
                .map(|slot| {
                    // Jeder dritte Slot bleibt leer.
                    if slot % 3 == 2 {
                        Vec::new()
                    } else {
                        let len = rng.below(longest as u64 + 1) as usize;
                        rng.bytes(len)
                    }
                })
                .collect();
            let parity_len = payloads.iter().map(Vec::len).max().unwrap_or(0).max(1);

            let slots = slots_of(&payloads, &[]);
            let mut actual = vec![0u8; parity_len];
            compute_q(count, &slots, &mut actual).unwrap();

            // Die Definition, direkt hingeschrieben: Summe g^j * D_j[i], wobei
            // D_j jenseits seines Endes null liest.
            let mut expected = vec![0u8; parity_len];
            for (index, payload) in payloads.iter().enumerate() {
                let coefficient = ferrite_parity::gf::g_pow(index as u8);
                for (offset, target) in expected.iter_mut().enumerate() {
                    let byte = payload.get(offset).copied().unwrap_or(0);
                    *target ^= ferrite_parity::gf::mul(coefficient, byte);
                }
            }

            assert_eq!(
                actual, expected,
                "count={count} round={round} longest={longest}"
            );
        }
    }
}

/// Dasselbe fuer den Fall, dass die Slots in verwuerfelter Reihenfolge kommen.
///
/// Das Horner-Schema laeuft absteigend nach `slot_index`. Wer stattdessen die
/// Reihenfolge des uebergebenen Slices nimmt, bekommt hier ein anderes Q.
#[test]
fn q_does_not_depend_on_the_order_of_the_slice() {
    let mut rng = Lcg::new(0x0A17_5EED);
    let count = 7u8;
    let payloads: Vec<Vec<u8>> = (0..count).map(|_| rng.bytes(5000)).collect();
    let parity_len = 5000;

    let ordered = slots_of(&payloads, &[]);
    let mut expected = vec![0u8; parity_len];
    compute_q(count, &ordered, &mut expected).unwrap();

    let mut shuffled = ordered.clone();
    shuffled.reverse();
    let mut actual = vec![0u8; parity_len];
    compute_q(count, &shuffled, &mut actual).unwrap();
    assert_eq!(actual, expected, "umgekehrte Reihenfolge");

    shuffled.swap(0, 4);
    shuffled.swap(1, 6);
    let mut actual = vec![0u8; parity_len];
    compute_q(count, &shuffled, &mut actual).unwrap();
    assert_eq!(actual, expected, "vertauschte Paare");
}

/// `Slot::byte_at` ist die Zero-Extension-Regel als ausfuehrbarer Satz.
///
/// Die Rechenpfade schneiden inzwischen Slices statt einzelne Bytes zu holen,
/// aber die Regel steht hier am Typ, und sie muss stimmen: Jenseits seines
/// Endes liest ein Slot Nullbytes — nicht Muell und keinen Fehler.
#[test]
fn a_slot_reads_as_zero_beyond_its_end() {
    let payload = [0xAAu8, 0xBB, 0xCC];
    let slot = Slot::new(0, &payload).unwrap();

    assert_eq!(slot.byte_at(0), 0xAA);
    assert_eq!(slot.byte_at(2), 0xCC);
    assert_eq!(slot.byte_at(3), 0, "erstes Byte hinter dem Ende");
    assert_eq!(slot.byte_at(usize::MAX), 0);

    // Ein leerer Slot liest ueberall null und ist damit der Grenzfall der
    // Regel, nicht ihre Ausnahme.
    let empty: [u8; 0] = [];
    let slot = Slot::new(3, &empty).unwrap();
    assert!(slot.is_empty());
    assert_eq!(slot.len(), 0);
    assert_eq!(slot.byte_at(0), 0);
}
