// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Paritaetsrechnung von Ferrite: Reed-Solomon P+Q ueber GF(2^8).
//!
//! Dieses Crate ist die ausfuehrbare Fassung von Abschnitt 2 des
//! Formatdokuments. Es rechnet und sonst nichts — kein I/O, keine
//! Konfiguration, keine Uhrzeit, kein Zufall aus der Umgebung. Damit laesst
//! sich die Rekonstruktion vollstaendig pruefen, ohne eine Platte anzufassen,
//! und deshalb steht sie vor der Engine.
//!
//! ```
//! use ferrite_parity::{compute_p, compute_q, reconstruct_two_from_pq, Slot};
//!
//! // Zwei Data-Slots ungleicher Laenge. Der kuerzere liest jenseits seines
//! // Endes als Nullbytes — das ist die Regel, die gemischte Plattengroessen
//! // erlaubt, kein Sonderfall.
//! let long = [1u8, 2, 3, 4];
//! let short = [9u8, 9];
//! let slots = [Slot::new(0, &long)?, Slot::new(1, &short)?];
//!
//! let mut p = [0u8; 4];
//! let mut q = [0u8; 4];
//! compute_p(2, &slots, &mut p)?;
//! compute_q(2, &slots, &mut q)?;
//!
//! // Beide Data-Members fallen aus, P und Q bleiben.
//! let mut recovered_long = [0u8; 4];
//! let mut recovered_short = [0u8; 2];
//! reconstruct_two_from_pq(2, 0, 1, &[], &p, &q, &mut recovered_long, &mut recovered_short)?;
//!
//! assert_eq!(recovered_long, long);
//! assert_eq!(recovered_short, short);
//! # Ok::<(), ferrite_parity::ParityError>(())
//! ```

pub mod error;
pub mod gf;

pub use error::{ParityError, Result};

/// Groesste zulaessige Anzahl Data-Slots, `docs/FORMAT.md` Abschnitt 2.
pub const MAX_DATA_SLOTS: u8 = 64;

/// Breite der Arbeitsscheibe in der Zwei-Slot-Rekonstruktion.
///
/// Nur ein Kompromiss zwischen Stack-Verbrauch und Cache-Lokalitaet: Die
/// beiden Zwischensummen brauchen zusammen 8 KiB Stack, dafuer laeuft die
/// Schleife ueber die Slots nicht fuer jedes einzelne Byte neu. Kein
/// On-Disk-Wert, jederzeit aenderbar.
const SCRATCH_WIDTH: usize = 4096;

/// Ein Data-Slot, wie er in die Paritaetsrechnung eingeht.
///
/// `data` **darf kuerzer sein** als der Parity-Block. Jenseits seines Endes
/// liest der Slot als Nullbytes. Das steckt hier im Typ und nicht in einem
/// Sonderfall beim Aufrufer, weil es keine Ausnahme ist, sondern die Regel:
/// Ohne sie waeren gemischte Plattengroessen nicht moeglich, und eine
/// Implementierung, die stattdessen abbricht, produziert falsche Paritaet
/// (`docs/FORMAT.md`, Abschnitt 2).
///
/// `index` ist der `slot_index` aus dem Superblock, nicht die Position im
/// uebergebenen Slice. Er allein bestimmt den Q-Koeffizienten `g^index`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot<'a> {
    index: u8,
    data: &'a [u8],
}

impl<'a> Slot<'a> {
    /// Neuer Slot. Der Index muss im von Abschnitt 2 zugelassenen Bereich
    /// liegen — geprueft wird hier und nicht in jeder Rechenfunktion.
    pub fn new(index: u8, data: &'a [u8]) -> Result<Self> {
        if index >= MAX_DATA_SLOTS {
            return Err(ParityError::SlotIndexOutOfRange { index });
        }
        Ok(Slot { index, data })
    }

    pub const fn index(&self) -> u8 {
        self.index
    }

    pub const fn data(&self) -> &'a [u8] {
        self.data
    }

    pub const fn len(&self) -> usize {
        self.data.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Byte an `offset`, jenseits des Slot-Endes null.
    #[inline]
    pub fn byte_at(&self, offset: usize) -> u8 {
        match self.data.get(offset) {
            Some(&byte) => byte,
            None => 0,
        }
    }

    /// Q-Koeffizient dieses Slots, `g^slot_index`.
    #[inline]
    pub fn coefficient(&self) -> u8 {
        gf::g_pow(self.index)
    }
}

/// `P[i] = D_0[i] ^ D_1[i] ^ … ^ D_n-1[i]`.
///
/// `out` gibt die Laenge der Paritaet vor. Kuerzere Slots werden mit Nullbytes
/// verlaengert; laengere sind ein Fehler, weil sie Bedingung 1 aus Abschnitt 2
/// verletzen (`payload_size(ParityP) >= max(payload_size(D_j))`).
///
/// `slots` muss **alle** `data_slot_count` Data-Slots enthalten, genau einmal
/// jeden. Ein vergessener Slot ergaebe sonst eine Paritaet, die zu nichts passt:
/// Sie sieht gueltig aus, und der Fehler faellt erst auf, wenn jemand daraus
/// rekonstruiert — also im Ernstfall.
pub fn compute_p(data_slot_count: u8, slots: &[Slot<'_>], out: &mut [u8]) -> Result<()> {
    check_complete_set(data_slot_count, &[], slots)?;
    check_slot_set(slots, out.len())?;
    out.fill(0);
    for slot in slots {
        xor_into(out, slot.data());
    }
    Ok(())
}

/// `Q[i] = Summe g^j * D_j[i]` in GF(2^8), `j` ist der `slot_index`.
///
/// Dieselbe Vollstaendigkeitsbedingung wie bei [`compute_p`].
pub fn compute_q(data_slot_count: u8, slots: &[Slot<'_>], out: &mut [u8]) -> Result<()> {
    check_complete_set(data_slot_count, &[], slots)?;
    check_slot_set(slots, out.len())?;

    let mut table = EMPTY_SLOTS;
    for slot in slots {
        table[usize::from(slot.index())] = slot.data();
    }
    q_into(out, &table, data_slot_count);
    Ok(())
}

/// Ein fehlender Data-Slot aus P und allen uebrigen Data-Slots.
///
/// `out.len()` ist die Payload-Groesse des ausgefallenen Members und bestimmt,
/// wie weit rekonstruiert wird. Ueberlebende duerfen laenger oder kuerzer sein.
pub fn reconstruct_from_p(
    data_slot_count: u8,
    target: u8,
    survivors: &[Slot<'_>],
    p: &[u8],
    out: &mut [u8],
) -> Result<()> {
    check_complete_set(data_slot_count, &[target], survivors)?;
    ensure_covers("p", p.len(), out.len())?;
    check_slot_set(survivors, p.len())?;

    out.copy_from_slice(&p[..out.len()]);
    for slot in survivors {
        xor_into(out, slot.data());
    }
    Ok(())
}

/// Ein fehlender Data-Slot aus Q und allen uebrigen Data-Slots.
pub fn reconstruct_from_q(
    data_slot_count: u8,
    target: u8,
    survivors: &[Slot<'_>],
    q: &[u8],
    out: &mut [u8],
) -> Result<()> {
    check_complete_set(data_slot_count, &[target], survivors)?;
    ensure_covers("q", q.len(), out.len())?;
    check_slot_set(survivors, q.len())?;

    // Zuerst alles Bekannte aus Q herausrechnen, uebrig bleibt g^target * D.
    // Der fehlende Slot steht nicht in der Tabelle und traegt damit nichts bei —
    // genau das ist hier gewollt.
    let mut table = EMPTY_SLOTS;
    for slot in survivors {
        table[usize::from(slot.index())] = slot.data();
    }
    q_into(out, &table, data_slot_count);
    xor_into(out, &q[..out.len()]);

    // g^target ist nie null, das Inverse existiert also immer. Zurueckgegeben
    // wird der Fehler trotzdem, statt ihn anzunehmen.
    let inverse = gf::inv(gf::g_pow(target)).ok_or(ParityError::DivisionByZero)?;
    let scale = gf::mul_table(inverse);
    for byte in out.iter_mut() {
        *byte = scale[usize::from(*byte)];
    }
    Ok(())
}

/// Zwei fehlende Data-Slots aus P und Q.
///
/// Loest pro Byte das Gleichungssystem `A = D_x ^ D_y` und
/// `B = g^x * D_x ^ g^y * D_y` nach `D_y` und `D_x` auf. Der Nenner
/// `g^x ^ g^y` ist nie null: `g` hat Ordnung 255, und bei hoechstens 64 Slots
/// sind alle `g^j` paarweise verschieden (Abschnitt 2).
#[allow(clippy::too_many_arguments)]
pub fn reconstruct_two_from_pq(
    data_slot_count: u8,
    first: u8,
    second: u8,
    survivors: &[Slot<'_>],
    p: &[u8],
    q: &[u8],
    out_first: &mut [u8],
    out_second: &mut [u8],
) -> Result<()> {
    if first == second {
        return Err(ParityError::SameSlotTwice { index: first });
    }
    check_complete_set(data_slot_count, &[first, second], survivors)?;

    let span = out_first.len().max(out_second.len());
    ensure_covers("p", p.len(), span)?;
    ensure_covers("q", q.len(), span)?;
    check_slot_set(survivors, p.len())?;
    check_slot_set(survivors, q.len())?;

    let coefficient_first = gf::g_pow(first);
    let coefficient_second = gf::g_pow(second);
    let inverse =
        gf::inv(coefficient_first ^ coefficient_second).ok_or(ParityError::DivisionByZero)?;

    // Statt `mul(B ^ mul(g^x, A), c)` je Byte einmal ausmultipliziert:
    //
    //     D_y = c·B ^ (c·g^x)·A
    //
    // Beide Faktoren sind ueber den ganzen Block konstant, also wird daraus je
    // ein Load statt einer vollen Multiplikation.
    let scale_q = gf::mul_table(inverse);
    let scale_p = gf::mul_table(gf::mul(coefficient_first, inverse));

    let mut scratch_p = [0u8; SCRATCH_WIDTH];
    let mut scratch_q = [0u8; SCRATCH_WIDTH];

    let mut base = 0usize;
    while base < span {
        let width = SCRATCH_WIDTH.min(span - base);
        let sum_p = &mut scratch_p[..width];
        let sum_q = &mut scratch_q[..width];

        sum_p.copy_from_slice(&p[base..base + width]);
        for slot in survivors {
            xor_into(sum_p, window(slot.data(), base, width));
        }

        // Die Scheibe ist hier schon geschnitten, deshalb bekommt `q_into`
        // eine Tabelle aus den Ausschnitten und rechnet ab Offset 0.
        let mut table = EMPTY_SLOTS;
        for slot in survivors {
            table[usize::from(slot.index())] = window(slot.data(), base, width);
        }
        q_into(sum_q, &table, data_slot_count);
        xor_into(sum_q, &q[base..base + width]);

        for offset in 0..width {
            let value_second =
                scale_q[usize::from(sum_q[offset])] ^ scale_p[usize::from(sum_p[offset])];
            let value_first = sum_p[offset] ^ value_second;
            // Der kuerzere der beiden Slots endet frueher; dahinter ist sein
            // Wert per Zero-Extension null und wird nicht geschrieben.
            if let Some(cell) = out_first.get_mut(base + offset) {
                *cell = value_first;
            }
            if let Some(cell) = out_second.get_mut(base + offset) {
                *cell = value_second;
            }
        }

        base += width;
    }
    Ok(())
}

/// Ein Data-Slot **und** der P-Member fehlen: Der Data-Slot kommt aus Q, P
/// danach aus allen Data-Slots.
pub fn reconstruct_data_and_p(
    data_slot_count: u8,
    target: u8,
    survivors: &[Slot<'_>],
    q: &[u8],
    out_target: &mut [u8],
    out_p: &mut [u8],
) -> Result<()> {
    reconstruct_from_q(data_slot_count, target, survivors, q, out_target)?;

    let recovered: &[u8] = out_target;
    check_slot_set(survivors, out_p.len())?;
    ensure_covers("out_p", out_p.len(), recovered.len())?;

    out_p.fill(0);
    for slot in survivors {
        xor_into(out_p, slot.data());
    }
    xor_into(out_p, recovered);
    Ok(())
}

/// Ein Data-Slot **und** der Q-Member fehlen: Der Data-Slot kommt aus P, Q
/// danach aus allen Data-Slots.
pub fn reconstruct_data_and_q(
    data_slot_count: u8,
    target: u8,
    survivors: &[Slot<'_>],
    p: &[u8],
    out_target: &mut [u8],
    out_q: &mut [u8],
) -> Result<()> {
    reconstruct_from_p(data_slot_count, target, survivors, p, out_target)?;

    let recovered: &[u8] = out_target;
    check_slot_set(survivors, out_q.len())?;
    ensure_covers("out_q", out_q.len(), recovered.len())?;

    let mut table = EMPTY_SLOTS;
    for slot in survivors {
        table[usize::from(slot.index())] = slot.data();
    }
    // Der eben wiederhergestellte Slot gehoert mit in die Summe.
    table[usize::from(target)] = recovered;
    q_into(out_q, &table, data_slot_count);
    Ok(())
}

/// XOR von `data` in `out`.
///
/// Die Zip-Schleife **ist** die Zero-Extension: Wo `data` endet, hoert sie auf,
/// und `out` bleibt dort unveraendert — genau das, was ein XOR mit Nullbytes
/// taete. Kein `if`, keine Sonderbehandlung.
#[inline]
fn xor_into(out: &mut [u8], data: &[u8]) {
    for (target, &byte) in out.iter_mut().zip(data) {
        *target ^= byte;
    }
}

/// Nutzdaten aller Slots, nach `slot_index` abgelegt.
///
/// Ein Index ohne Slot traegt einen leeren Slice. Das ist keine
/// Sonderbehandlung, sondern genau die Zero-Extension: Wer nichts beitraegt,
/// beitraegt Nullbytes.
type SlotTable<'a> = [&'a [u8]; MAX_DATA_SLOTS as usize];

const EMPTY_SLOTS: SlotTable<'static> = [&[]; MAX_DATA_SLOTS as usize];

/// `Q[i] = Summe g^j * D_j[i]`, nach dem Horner-Schema.
///
/// Statt fuer jedes Byte `g^j` aus der Tabelle zu holen, wird der
/// Zwischenstand einmal je Slot verdoppelt:
///
/// ```text
/// Q = D_0 ^ g(D_1 ^ g(D_2 ^ … ^ g·D_n-1))
/// ```
///
/// Dieselbe Summe, aber ohne Table-Lookup — und damit vektorisierbar. Die
/// Slots muessen dafuer in absteigender Index-Reihenfolge durchlaufen werden,
/// nicht in der Reihenfolge des uebergebenen Slices; genau dafuer ist
/// [`SlotTable`] da.
///
/// Gerechnet wird in Scheiben. Ohne sie liefe der Akkumulator `count` Mal ueber
/// die volle Blockgroesse — bei 16 MiB Parity-Bloecken waere das
/// speicherbandbreitenbegrenzt statt rechenbegrenzt. So bleibt er im Cache.
fn q_into(out: &mut [u8], table: &SlotTable<'_>, data_slot_count: u8) {
    let mut base = 0usize;
    while base < out.len() {
        let width = SCRATCH_WIDTH.min(out.len() - base);
        let chunk = &mut out[base..base + width];
        chunk.fill(0);
        for index in (0..usize::from(data_slot_count)).rev() {
            gf::double_in_place(chunk);
            xor_into(chunk, window(table[index], base, width));
        }
        base += width;
    }
}

/// Ausschnitt `base..base + width` von `data`, gekuerzt am Ende des Slots.
#[inline]
fn window(data: &[u8], base: usize, width: usize) -> &[u8] {
    if base >= data.len() {
        return &[];
    }
    &data[base..data.len().min(base + width)]
}

fn ensure_covers(what: &'static str, got: usize, need: usize) -> Result<()> {
    if got < need {
        return Err(ParityError::BufferTooSmall { what, need, got });
    }
    Ok(())
}

/// Keine doppelten Indizes, kein Slot laenger als die Paritaet.
fn check_slot_set(slots: &[Slot<'_>], parity_len: usize) -> Result<()> {
    for (position, slot) in slots.iter().enumerate() {
        if slot.len() > parity_len {
            return Err(ParityError::SlotLongerThanParity {
                index: slot.index(),
                slot_len: slot.len(),
                parity_len,
            });
        }
        if slots[..position]
            .iter()
            .any(|other| other.index() == slot.index())
        {
            return Err(ParityError::DuplicateSlot {
                index: slot.index(),
            });
        }
    }
    Ok(())
}

/// Prueft, dass `present` **genau** die Data-Slots ohne die verlorenen sind.
///
/// Bei `compute_p`/`compute_q` ist `lost` leer, dann heisst das: alle Slots.
///
/// Ohne diese Pruefung liefe ein Aufruf mit einem vergessenen Slot durch und
/// erzeugte plausible, aber falsche Bytes — eine Paritaet, die zu nichts passt,
/// oder eine Rekonstruktion, die Muell auf die Platte schreibt. Das ist der
/// Fehler, den man erst Monate spaeter am Dateisystem bemerkt.
fn check_complete_set(data_slot_count: u8, lost: &[u8], present: &[Slot<'_>]) -> Result<()> {
    if data_slot_count == 0 || data_slot_count > MAX_DATA_SLOTS {
        return Err(ParityError::InvalidSlotCount {
            count: data_slot_count,
        });
    }
    for &index in lost {
        if index >= data_slot_count {
            return Err(ParityError::SlotIndexOutOfRange { index });
        }
    }
    for (position, slot) in present.iter().enumerate() {
        if slot.index() >= data_slot_count {
            return Err(ParityError::SlotIndexOutOfRange {
                index: slot.index(),
            });
        }
        if lost.contains(&slot.index()) {
            return Err(ParityError::LostSlotAmongSurvivors {
                index: slot.index(),
            });
        }
        if present[..position]
            .iter()
            .any(|other| other.index() == slot.index())
        {
            return Err(ParityError::DuplicateSlot {
                index: slot.index(),
            });
        }
    }
    // Alle Indizes sind verschieden, kleiner als `data_slot_count` und keiner
    // ist verloren — dann heisst die richtige Anzahl auch: vollstaendig.
    let expected = data_slot_count as usize - lost.len();
    if present.len() != expected {
        return Err(ParityError::IncompleteSlotSet {
            expected,
            got: present.len(),
        });
    }
    Ok(())
}
