// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! `LogRecordHeader::decode` gegen beliebige Bytes.
//!
//! Wie beim Superblock zwei Pfade: einmal roh, einmal mit reparierter
//! Header-Pruefsumme, damit der Fuzzer hinter die CRC-Pruefung kommt.

#![no_main]

use ferrite_format::log::{LogRecordHeader, LOG_HEADER_SIZE, LOG_MAGIC};
use libfuzzer_sys::fuzz_target;

/// Offset der Header-Pruefsumme, `docs/FORMAT.md` Abschnitt 5.1.
const OFF_HEADER_CRC: usize = LOG_HEADER_SIZE - 4;

fuzz_target!(|data: &[u8]| {
    let _ = LogRecordHeader::decode(data);

    // Kuerzere Eingaben werden mit Nullbytes aufgefuellt, statt sie zu
    // verwerfen — sonst sieht der Fuzzer ueberwiegend den Laengencheck.
    let mut block = [0u8; LOG_HEADER_SIZE];
    let taken = data.len().min(LOG_HEADER_SIZE);
    block[..taken].copy_from_slice(&data[..taken]);
    block[..LOG_MAGIC.len()].copy_from_slice(LOG_MAGIC);
    let checksum = ferrite_format::checksum(&block[..OFF_HEADER_CRC]);
    block[OFF_HEADER_CRC..].copy_from_slice(&checksum.to_le_bytes());

    let Ok(header) = LogRecordHeader::decode(&block) else {
        return;
    };

    assert_eq!(
        LogRecordHeader::decode(&header.encode()).as_ref(),
        Ok(&header),
        "decode(encode(x)) != x"
    );

    // `payload_len` kommt ungeprueft von der Platte. Die Laengenrechnung
    // darauf darf nicht ueberlaufen — sie bestimmt, wo der naechste Record
    // im Ringpuffer gesucht wird.
    let on_disk = header.on_disk_len();
    assert_eq!(on_disk % 4096, 0);
    assert!(on_disk >= LOG_HEADER_SIZE + header.payload_len as usize);

    // Nutzdaten, die nicht zum Header passen, muessen als Fehler zurueckkommen
    // und nicht als Panik.
    let _ = header.verify_payload(data.get(LOG_HEADER_SIZE..).unwrap_or(&[]));
    let _ = header.verify_payload(&[]);
});
