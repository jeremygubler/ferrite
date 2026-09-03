// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! GF(2^8), `docs/FORMAT.md` Abschnitt 2.
//!
//! Reduktionspolynom `x^8 + x^4 + x^3 + x^2 + 1` (`0x11D`), Generator
//! `g = 0x02`. Dasselbe Feld, das Linux `md` fuer RAID6 benutzt — damit passen
//! fremde Testvektoren und spaeter fremde SIMD-Kernel darauf.
//!
//! Die Tabellen entstehen zur Compile-Zeit. Kein `lazy_static`, keine
//! Initialisierung zur Laufzeit, kein Zustand: Das Feld ist eine Konstante des
//! Formats und nichts, was ein Programm erst herstellen muesste.

/// Reduktionspolynom, `x^8 + x^4 + x^3 + x^2 + 1`.
pub const POLYNOMIAL: u16 = 0x11D;

/// Generator der multiplikativen Gruppe.
pub const GENERATOR: u8 = 0x02;

/// Ordnung der multiplikativen Gruppe: `g^255 == 1`.
pub const ORDER: u16 = 255;

/// `EXP[i] == g^i`. Doppelt gefuehrt, damit die Multiplikation ohne Modulo
/// auskommt: Der groesste vorkommende Index ist `254 + 254 == 508`.
const EXP: [u8; 512] = build_exp();

/// `LOG[g^i] == i`. `LOG[0]` ist unbelegt und wird nie gelesen — die
/// Multiplikation faengt die Null vorher ab.
const LOG: [u8; 256] = build_log();

const fn build_exp() -> [u8; 512] {
    let mut table = [0u8; 512];
    let mut value: u16 = 1;
    let mut power = 0usize;
    while power < 255 {
        table[power] = value as u8;
        value <<= 1;
        if value & 0x100 != 0 {
            value ^= POLYNOMIAL;
        }
        power += 1;
    }
    let mut index = 255usize;
    while index < 512 {
        table[index] = table[index - 255];
        index += 1;
    }
    table
}

const fn build_log() -> [u8; 256] {
    let exp = build_exp();
    let mut table = [0u8; 256];
    let mut power = 0usize;
    while power < 255 {
        table[exp[power] as usize] = power as u8;
        power += 1;
    }
    table
}

/// Produkt zweier Feldelemente.
#[inline]
pub fn mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        return 0;
    }
    EXP[LOG[a as usize] as usize + LOG[b as usize] as usize]
}

/// `g^exponent`.
///
/// Der Exponent ist der `slot_index`, nicht die Position im uebergebenen
/// Slice. Der Rest modulo 255 ist rechnerisch neutral (`g^255 == g^0 == 1`) und
/// nur da, damit die Funktion fuer jedes `u8` definiert ist.
#[inline]
pub fn g_pow(exponent: u8) -> u8 {
    EXP[(exponent as u16 % ORDER) as usize]
}

/// Multiplikatives Inverses, `None` fuer die Null.
#[inline]
pub fn inv(a: u8) -> Option<u8> {
    if a == 0 {
        return None;
    }
    Some(EXP[(ORDER - LOG[a as usize] as u16) as usize])
}

/// Quotient `a / b`, `None` fuer `b == 0`.
#[inline]
pub fn div(a: u8, b: u8) -> Option<u8> {
    if b == 0 {
        return None;
    }
    if a == 0 {
        return Some(0);
    }
    let exponent = LOG[a as usize] as u16 + ORDER - LOG[b as usize] as u16;
    Some(EXP[(exponent % ORDER) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fester LCG wie in `format/tests/roundtrip.rs`: reproduzierbar, ohne
    /// Test-Dependency, in Millisekunden durch.
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
        fn byte(&mut self) -> u8 {
            (self.next_u64() & 0xFF) as u8
        }
    }

    #[test]
    fn zero_and_one_behave() {
        for a in 0u8..=255 {
            assert_eq!(mul(a, 0), 0);
            assert_eq!(mul(0, a), 0);
            assert_eq!(mul(a, 1), a);
            assert_eq!(mul(1, a), a);
        }
    }

    #[test]
    fn multiplication_is_commutative() {
        for a in 0u8..=255 {
            for b in 0u8..=255 {
                assert_eq!(mul(a, b), mul(b, a), "a={a} b={b}");
            }
        }
    }

    #[test]
    fn multiplication_is_associative() {
        let mut rng = Lcg::new(0x9E3779B9);
        for _ in 0..200_000 {
            let (a, b, c) = (rng.byte(), rng.byte(), rng.byte());
            assert_eq!(mul(mul(a, b), c), mul(a, mul(b, c)), "a={a} b={b} c={c}");
        }
        // Der Generator und das Reduktionspolynom als feste dritte Faktoren:
        // an ihnen haengt die Q-Rechnung.
        for c in [GENERATOR, 0x1D, 0xFF] {
            for a in 0u8..=255 {
                for b in 0u8..=255 {
                    assert_eq!(mul(mul(a, b), c), mul(a, mul(b, c)));
                }
            }
        }
    }

    #[test]
    fn multiplication_distributes_over_addition() {
        // Addition im Feld ist XOR. Ohne Distributivitaet waere die gesamte
        // Rekonstruktion falsch, weil sie Summen umsortiert.
        for a in 0u8..=255 {
            for b in 0u8..=255 {
                let c = a.wrapping_add(b).wrapping_mul(31);
                assert_eq!(mul(a, b ^ c), mul(a, b) ^ mul(a, c), "a={a} b={b} c={c}");
            }
        }
    }

    #[test]
    fn every_nonzero_element_has_an_inverse() {
        assert_eq!(inv(0), None);
        for a in 1u8..=255 {
            let inverse = inv(a).expect("jedes Element ausser null ist invertierbar");
            assert_eq!(mul(a, inverse), 1, "a={a}");
            assert_eq!(inv(inverse), Some(a), "a={a}");
        }
    }

    #[test]
    fn division_inverts_multiplication() {
        assert_eq!(div(7, 0), None);
        for a in 0u8..=255 {
            for b in 1u8..=255 {
                assert_eq!(div(mul(a, b), b), Some(a), "a={a} b={b}");
            }
        }
    }

    #[test]
    fn generator_has_order_255() {
        // Das ist die Eigenschaft, aus der Abschnitt 2 folgert, dass alle g^j
        // bei bis zu 64 Slots paarweise verschieden sind. Faellt sie, faellt
        // die Zwei-Slot-Rekonstruktion.
        let mut seen = [false; 256];
        let mut value = 1u8;
        for power in 0..255u16 {
            assert!(!seen[value as usize], "g^{power} wiederholt sich zu frueh");
            seen[value as usize] = true;
            assert_eq!(g_pow(power as u8), value, "g^{power}");
            if power > 0 {
                assert_ne!(value, 1, "Ordnung kleiner als 255 bei {power}");
            }
            value = mul(value, GENERATOR);
        }
        assert_eq!(value, 1, "g^255 muss 1 sein");

        // Genau die 255 Elemente ungleich null wurden getroffen.
        assert!(!seen[0]);
        assert_eq!(seen.iter().filter(|&&hit| hit).count(), 255);
    }

    #[test]
    fn slot_coefficients_are_pairwise_distinct() {
        // Abschnitt 2: Bei bis zu 64 Data-Slots sind alle g^j verschieden und
        // ungleich null. Ohne das waere die Nenner-Differenz g^x ^ g^y null.
        for x in 0u8..64 {
            assert_ne!(g_pow(x), 0);
            for y in 0u8..64 {
                if x != y {
                    assert_ne!(g_pow(x), g_pow(y), "g^{x} == g^{y}");
                }
            }
        }
    }
}
