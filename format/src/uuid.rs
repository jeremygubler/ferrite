// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

use core::fmt;

/// 16 Byte Identifier, on-disk exakt so gespeichert wie im Speicher.
///
/// Bewusst kein `uuid`-Crate: das Format-Crate soll dependency-frei bleiben,
/// und gebraucht wird hier nur Bytes rein, Bytes raus, plus eine lesbare
/// Darstellung fuer Logs und CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Uuid(pub [u8; 16]);

impl Uuid {
    pub const NIL: Uuid = Uuid([0u8; 16]);

    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Uuid(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    pub fn is_nil(&self) -> bool {
        self.0 == [0u8; 16]
    }

    /// Erzeugt eine UUID v4 aus 16 Bytes Zufall.
    ///
    /// Die Zufallsquelle liegt beim Aufrufer — dieses Crate macht kein I/O.
    pub fn from_random_bytes(mut bytes: [u8; 16]) -> Self {
        bytes[6] = (bytes[6] & 0x0F) | 0x40; // Version 4
        bytes[8] = (bytes[8] & 0x3F) | 0x80; // Variante RFC 4122
        Uuid(bytes)
    }

    /// Parst die kanonische Form `8-4-4-4-12`. Gross-/Kleinschreibung egal.
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 36 {
            return None;
        }
        let mut out = [0u8; 16];
        let mut index = 0usize;
        let mut position = 0usize;
        while position < 36 {
            if matches!(position, 8 | 13 | 18 | 23) {
                if bytes[position] != b'-' {
                    return None;
                }
                position += 1;
                continue;
            }
            let high = hex_value(bytes[position])?;
            let low = hex_value(*bytes.get(position + 1)?)?;
            out[index] = (high << 4) | low;
            index += 1;
            position += 2;
        }
        if index != 16 {
            return None;
        }
        Some(Uuid(out))
    }
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, byte) in self.0.iter().enumerate() {
            if matches!(index, 4 | 6 | 8 | 10) {
                f.write_str("-")?;
            }
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_parse_roundtrip() {
        let uuid = Uuid::from_random_bytes([
            0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66,
            0x77, 0x88,
        ]);
        let text = uuid.to_string();
        assert_eq!(text.len(), 36);
        assert_eq!(Uuid::parse(&text), Some(uuid));
        assert_eq!(Uuid::parse(&text.to_uppercase()), Some(uuid));
    }

    #[test]
    fn version_and_variant_are_set() {
        let uuid = Uuid::from_random_bytes([0xFF; 16]);
        assert_eq!(uuid.0[6] & 0xF0, 0x40);
        assert_eq!(uuid.0[8] & 0xC0, 0x80);
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!(Uuid::parse(""), None);
        assert_eq!(Uuid::parse("not-a-uuid"), None);
        assert_eq!(Uuid::parse("0123456789abcdef0123456789abcdef"), None);
        assert_eq!(Uuid::parse("01234567+89ab-cdef-0123-456789abcdef"), None);
        assert_eq!(Uuid::parse("0123456z-89ab-cdef-0123-456789abcdef"), None);
    }
}
