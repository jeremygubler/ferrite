//! CRC-32C (Castagnoli), reflektiert, Polynom `0x82F63B78`.
//!
//! Bewusst als reine Software-Tabelle. Die SSE4.2-/CRC32-Instruktionen gehoeren
//! in die Engine, nicht in das Format-Crate: hier zaehlt, dass das Ergebnis auf
//! jeder Architektur bitgleich ist und ohne `unsafe` auskommt.

const POLY: u32 = 0x82F6_3B78;

const TABLE: [u32; 256] = build_table();

const fn build_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ POLY
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

/// CRC-32C ueber `data`.
pub fn crc32c(data: &[u8]) -> u32 {
    update(0xFFFF_FFFF, data) ^ 0xFFFF_FFFF
}

/// Fortsetzbare Variante fuer Daten, die nicht am Stueck vorliegen.
///
/// Startwert ist `0xFFFF_FFFF`; das finale XOR muss der Aufrufer selbst
/// anwenden.
pub fn update(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        let index = ((crc ^ byte as u32) & 0xFF) as usize;
        crc = (crc >> 8) ^ TABLE[index];
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        // Standard-Testvektoren fuer CRC-32C.
        assert_eq!(crc32c(b""), 0x0000_0000);
        assert_eq!(crc32c(b"a"), 0xC1D0_4330);
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        assert_eq!(crc32c(&[0u8; 32]), 0x8A91_36AA);
    }

    #[test]
    fn streaming_matches_oneshot() {
        let data: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
        let oneshot = crc32c(&data);

        for split in [1usize, 7, 64, 1000, 4095] {
            let (head, tail) = data.split_at(split);
            let streamed = update(update(0xFFFF_FFFF, head), tail) ^ 0xFFFF_FFFF;
            assert_eq!(streamed, oneshot, "split at {split}");
        }
    }

    #[test]
    fn single_bit_flip_changes_crc() {
        let mut data = [0x5Au8; 512];
        let before = crc32c(&data);
        data[311] ^= 0x01;
        assert_ne!(crc32c(&data), before);
    }
}
