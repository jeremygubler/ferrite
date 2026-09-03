//! Recovery ueber eine Log-Region aus beliebigen Bytes,
//! `docs/FORMAT.md` Abschnitt 5.2.
//!
//! Das ist der Absturzpfad. Was hier hereinkommt, ist genau das, was ein
//! Stromausfall hinterlaesst: halbe Records, Reste aus frueheren Runden des
//! Ringpuffers, Header mitten in Nutzdaten.
//!
//! Zwei Invarianten:
//!
//! 1. Der Replay terminiert und liest nie ueber den Rand.
//! 2. Die gelieferten Records bilden eine **lueckenlos aufsteigende** Kette,
//!    und jeder von ihnen traegt die erwartete Generation und eine Nutzlast,
//!    die zu seiner Pruefsumme passt. Ein Record, der diese Kette verletzt,
//!    darf nie erscheinen — an dieser Regel haengt, ob ein Replay nach einem
//!    Absturz alte Daten ueber neue schreibt.

#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use ferrite_format::log::ring::{LogRing, LogWriter};
use ferrite_format::log::{LogRecordHeader, RecordType, LOG_SECTOR_SIZE};
use libfuzzer_sys::fuzz_target;

/// Genug Sektoren fuer Umbrueche, klein genug fuer viele Laeufe pro Sekunde.
const MAX_SECTORS: usize = 16;
const MAX_RECORDS: usize = 24;

/// Baut aus dem Fuzz-Input eine Log-Region.
///
/// Zwei Wege, beide gebraucht: einmal roh, damit der Scan auf Muell trifft, und
/// einmal ueber den `LogWriter`, damit ueberhaupt gueltige Ketten entstehen.
/// Ohne den zweiten Weg kaeme der Fuzzer nie an einer gueltigen Header-CRC
/// vorbei und der Replay liefe nie ueber mehr als einen Record.
fn build(u: &mut Unstructured) -> arbitrary::Result<(Vec<u8>, u64)> {
    let sectors = 1 + usize::from(u8::arbitrary(u)?) % MAX_SECTORS;
    let mut region = vec![0u8; sectors * LOG_SECTOR_SIZE];
    let generation = u64::arbitrary(u)?;

    // Zuerst Rohbytes hineinstreuen. Was der Writer spaeter ueberschreibt,
    // ueberschreibt er; der Rest bleibt als Altlast liegen.
    let noise = u.arbitrary_len::<u8>()?.min(region.len());
    let bytes = u.bytes(noise)?;
    region[..bytes.len()].copy_from_slice(bytes);

    {
        let mut writer =
            LogWriter::new(&mut region).map_err(|_| arbitrary::Error::IncorrectFormat)?;
        let mut seq = u64::arbitrary(u)?;

        for _ in 0..MAX_RECORDS {
            if u.is_empty() {
                break;
            }
            let knobs = u8::arbitrary(u)?;
            let payload_len = usize::from(u16::arbitrary(u)?) % (3 * LOG_SECTOR_SIZE);
            let payload = u.bytes(payload_len)?.to_vec();

            let mut header = LogRecordHeader::write(seq, u16::arbitrary(u)?, 0, &payload);
            header.generation = if knobs & 1 == 0 {
                generation
            } else {
                u64::arbitrary(u)?
            };
            if knobs & 2 != 0 {
                // Checkpoint statt Write: setzt den Startpunkt des Replays.
                header = LogRecordHeader::checkpoint(seq);
                header.generation = generation;
            }
            if knobs & 4 != 0 {
                // Torn write: Header heil, Nutzdaten kaputt.
                header.payload_crc32c = u32::arbitrary(u)?;
            }

            let payload = if header.record_type == RecordType::Checkpoint {
                Vec::new()
            } else {
                payload
            };
            if writer.append(&header, &payload).is_err() {
                break;
            }

            // Meist fortlaufend, manchmal mit Sprung — sonst entstuende nie
            // eine Luecke, und die Kettenregel bliebe ungetestet.
            seq = if knobs & 8 == 0 {
                seq.wrapping_add(1)
            } else {
                seq.wrapping_add(u64::from(u8::arbitrary(u)?))
            };
        }
    }

    Ok((region, generation))
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let Ok((region, generation)) = build(&mut u) else {
        return;
    };

    let Ok(ring) = LogRing::new(&region) else {
        return;
    };

    // Schritt 1 und 2 duerfen an keiner Eingabe paniken.
    let _ = ring.scan().count();
    let _ = ring.newest_checkpoint();
    let _ = ring.lowest_sequence();

    let mut replay = ring.replay(generation);
    let mut previous: Option<u64> = None;
    let mut count = 0usize;

    for record in replay.by_ref() {
        count += 1;
        assert!(
            count <= region.len() / LOG_SECTOR_SIZE,
            "mehr Records als Sektoren — der Lauf dreht im Kreis"
        );

        // Der Record muss vollstaendig in der Region liegen.
        let end = record.offset + record.header.on_disk_len();
        assert!(end <= region.len(), "Record reicht ueber das Ende hinaus");
        assert_eq!(record.payload.len(), record.header.payload_len as usize);

        // Abschnitt 5.2 Schritt 3: Generation, Pruefsumme, lueckenlose Folge.
        assert_eq!(record.header.generation, generation);
        record
            .header
            .verify_payload(record.payload)
            .expect("angewendeter Record muss zu seiner Pruefsumme passen");
        assert_ne!(
            record.header.record_type,
            RecordType::Padding,
            "Padding nimmt an der Kette nicht teil"
        );
        if let Some(last) = previous {
            assert_eq!(
                record.header.seq,
                last.wrapping_add(1),
                "Luecke in der Sequenz — der Replay haette hier abbrechen muessen"
            );
            assert_ne!(last, u64::MAX, "nach u64::MAX kann kein Record mehr folgen");
        }
        previous = Some(record.header.seq);
    }

    assert!(
        replay.stop().is_some(),
        "ein beendeter Lauf hat einen Grund"
    );
    assert_eq!(replay.accepted_count(), count as u64);
    assert_eq!(replay.last_accepted_seq(), previous);
});
