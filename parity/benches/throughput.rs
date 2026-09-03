// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Durchsatz der Paritaetsrechnung.
//!
//! Beantwortet eine Frage, die vor Meilenstein 2 offen ist: Wie viele Nutzdaten
//! schafft ein Kern pro Sekunde? Davon haengt ab, ob die Engine die
//! Paritaetsrechnung parallelisieren muss, um mit den Platten mitzuhalten — und
//! ob SIMD ueberhaupt lohnt.
//!
//! Gemessen wird in **Nutzdaten**, also `slots * block_len`, nicht in erzeugter
//! Paritaet. Das ist die Groesse, die mit dem Plattendurchsatz zu vergleichen
//! ist.
//!
//! Kein Harness, keine Dependency, `Instant` nur hier und nicht im Crate —
//! `parity` bleibt ohne Uhr. Aufruf:
//!
//! ```text
//! cargo bench -p ferrite-parity
//! ```

use std::hint::black_box;
use std::time::{Duration, Instant};

use ferrite_parity::{compute_p, compute_q, reconstruct_from_p, reconstruct_two_from_pq, Slot};

/// Ein Parity-Block der Standardgroesse aus `Superblock::new`.
const BLOCK: usize = 64 * 1024;
/// Mindestdauer je Messung. Kuerzer, und die Uhr dominiert das Ergebnis.
const MIN_DURATION: Duration = Duration::from_millis(300);

/// Derselbe feste LCG wie in den Tests: reproduzierbare Daten ohne Dependency.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed)
    }
    fn fill(&mut self, target: &mut [u8]) {
        for byte in target.iter_mut() {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *byte = ((self.0 >> 33) & 0xFF) as u8;
        }
    }
}

/// Fuehrt `body` so oft aus, bis [`MIN_DURATION`] erreicht ist, und liefert die
/// Zeit je Durchlauf.
fn measure(mut body: impl FnMut()) -> Duration {
    // Einmal warmlaufen, damit Tabellen und Caches nicht in die Messung fallen.
    body();

    let mut rounds = 1u32;
    loop {
        let start = Instant::now();
        for _ in 0..rounds {
            body();
        }
        let elapsed = start.elapsed();
        if elapsed >= MIN_DURATION {
            return elapsed / rounds;
        }
        // Hochtasten statt raten. Der Faktor 4 haelt die Anlaufzeit kurz.
        rounds = rounds.saturating_mul(4);
    }
}

fn throughput_mb_s(bytes: usize, per_round: Duration) -> f64 {
    bytes as f64 / per_round.as_secs_f64() / (1024.0 * 1024.0)
}

fn report(label: &str, slots: usize, bytes: usize, per_round: Duration) {
    println!(
        "  {label:<28} {slots:>2} Slots  {:>8.2} ms  {:>9.1} MB/s",
        per_round.as_secs_f64() * 1000.0,
        throughput_mb_s(bytes, per_round)
    );
}

fn main() {
    println!("Paritaetsdurchsatz, Blockgroesse {} KiB", BLOCK / 1024);
    println!("Durchsatz bezogen auf Nutzdaten (slots * block_len), ein Kern.\n");

    let mut rng = Lcg::new(0x9E37_79B9_7F4A_7C15);

    for slot_count in [4usize, 8, 16, 32] {
        let payloads: Vec<Vec<u8>> = (0..slot_count)
            .map(|_| {
                let mut buffer = vec![0u8; BLOCK];
                rng.fill(&mut buffer);
                buffer
            })
            .collect();
        let slots: Vec<Slot<'_>> = payloads
            .iter()
            .enumerate()
            .map(|(index, bytes)| Slot::new(index as u8, bytes).expect("Index unter 64"))
            .collect();
        let data_bytes = slot_count * BLOCK;

        let mut p = vec![0u8; BLOCK];
        let mut q = vec![0u8; BLOCK];
        compute_p(slot_count as u8, &slots, &mut p).expect("P");
        compute_q(slot_count as u8, &slots, &mut q).expect("Q");

        println!("--- {slot_count} Data-Slots ---");

        let mut out = vec![0u8; BLOCK];
        let per_round = measure(|| {
            compute_p(slot_count as u8, black_box(&slots), black_box(&mut out)).expect("P");
        });
        report("compute_p", slot_count, data_bytes, per_round);

        let per_round = measure(|| {
            compute_q(slot_count as u8, black_box(&slots), black_box(&mut out)).expect("Q");
        });
        report("compute_q", slot_count, data_bytes, per_round);

        // P und Q zusammen — so faellt es in der Engine an, wenn ein Block
        // dreckig ist und neu gerechnet wird.
        let per_round = measure(|| {
            compute_p(slot_count as u8, black_box(&slots), black_box(&mut p)).expect("P");
            compute_q(slot_count as u8, black_box(&slots), black_box(&mut q)).expect("Q");
        });
        report("compute_p + compute_q", slot_count, data_bytes, per_round);

        // Rekonstruktion eines Slots: der Lesepfad im degradierten Betrieb.
        let survivors: Vec<Slot<'_>> = payloads
            .iter()
            .enumerate()
            .skip(1)
            .map(|(index, bytes)| Slot::new(index as u8, bytes).expect("Index unter 64"))
            .collect();
        let per_round = measure(|| {
            reconstruct_from_p(
                slot_count as u8,
                0,
                black_box(&survivors),
                black_box(&p),
                black_box(&mut out),
            )
            .expect("Rekonstruktion aus P");
        });
        report("reconstruct_from_p", slot_count, data_bytes, per_round);

        // Zwei Slots aus P und Q: der teuerste Fall, den das Layout kennt.
        let survivors: Vec<Slot<'_>> = payloads
            .iter()
            .enumerate()
            .skip(2)
            .map(|(index, bytes)| Slot::new(index as u8, bytes).expect("Index unter 64"))
            .collect();
        let mut second = vec![0u8; BLOCK];
        let per_round = measure(|| {
            reconstruct_two_from_pq(
                slot_count as u8,
                0,
                1,
                black_box(&survivors),
                black_box(&p),
                black_box(&q),
                black_box(&mut out),
                black_box(&mut second),
            )
            .expect("Rekonstruktion zweier Slots");
        });
        report("reconstruct_two_from_pq", slot_count, data_bytes, per_round);

        println!();
    }
}
