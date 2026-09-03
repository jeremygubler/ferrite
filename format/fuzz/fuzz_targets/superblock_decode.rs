// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! `Superblock::decode` gegen beliebige Bytes.
//!
//! Zwei Pfade pro Durchlauf. Der rohe Puffer prueft, dass Magic-, Pruefsummen-
//! und Laengenpruefung nicht paniken. Die Kopie mit reparierter Pruefsumme
//! prueft den Teil dahinter: Ohne sie kaeme der Fuzzer nie an `validate()`
//! vorbei, weil er die CRC-32C nie zufaellig trifft.

#![no_main]

use ferrite_format::superblock::{Superblock, SUPERBLOCK_MAGIC, SUPERBLOCK_SIZE};
use libfuzzer_sys::fuzz_target;

/// Offset der Pruefsumme im Superblock, `docs/FORMAT.md` Abschnitt 4.
const OFF_CRC: usize = SUPERBLOCK_SIZE - 4;

fuzz_target!(|data: &[u8]| {
    let _ = Superblock::decode(data);

    // Kuerzere Eingaben werden mit Nullbytes aufgefuellt, statt sie zu
    // verwerfen. Wer hier auf exakt 4096 Bytes besteht, bekommt vom Fuzzer fast
    // nur den Laengencheck zu sehen: Er muesste die Maximallaenge zufaellig
    // genau treffen, und ohne Coverage-Belohnung dafuer tut er es nicht.
    let mut block = [0u8; SUPERBLOCK_SIZE];
    let taken = data.len().min(SUPERBLOCK_SIZE);
    block[..taken].copy_from_slice(&data[..taken]);
    block[..SUPERBLOCK_MAGIC.len()].copy_from_slice(SUPERBLOCK_MAGIC);
    let checksum = ferrite_format::checksum(&block[..OFF_CRC]);
    block[OFF_CRC..].copy_from_slice(&checksum.to_le_bytes());

    let Ok(superblock) = Superblock::decode(&block) else {
        return;
    };

    // Was `decode` durchgelassen hat, ist per Definition gueltig und muss sich
    // deshalb wieder kodieren lassen. Ein Fehler hier hiesse, dass `decode`
    // und `validate` verschiedene Regeln durchsetzen.
    let re_encoded = superblock
        .encode()
        .expect("dekodierter Superblock muss kodierbar sein");
    assert_eq!(
        Superblock::decode(&re_encoded).as_ref(),
        Ok(&superblock),
        "decode(encode(x)) != x"
    );

    // Abgeleitete Werte duerfen an keiner gueltigen Eingabe paniken.
    let _ = superblock.parity_block_size();
    let _ = superblock.parity_block_count();
    let _ = superblock.access_mode();
    // Die Groessenrechnung aus Abschnitt 3 arbeitet mit Werten, die
    // ungeprueft von der Platte kommen — sie darf an keiner Kombination
    // ueberlaufen.
    let _ = superblock.payload_end();
    for device_size in [
        0,
        u64::MAX,
        superblock.payload_offset,
        superblock.payload_size,
        superblock.payload_end().unwrap_or(u64::MAX),
    ] {
        let _ = Superblock::backup_offset(device_size);
        let _ = superblock.fits_on_device(device_size);
    }
    let _ = Superblock::select(data, &re_encoded);
    let _ = Superblock::select(&re_encoded, data);
});
