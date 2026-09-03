//! `ChainValidator` gegen eine aus dem Fuzz-Input abgeleitete Folge von
//! Records.
//!
//! Die Invariante, an der alles haengt: **Sobald einmal `StopReplay` kam, darf
//! nie wieder `Accept` kommen** — egal wie gueltig ein spaeterer Record fuer
//! sich aussieht. Nach einem Absturz liegen im Ringpuffer intakte Records aus
//! einer frueheren Runde. Wer die mitnimmt, schreibt beim Replay alte Daten
//! ueber neue. `docs/FORMAT.md` Abschnitt 5.2, Schritt 4.

#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use ferrite_format::log::{ChainValidator, ChainVerdict, LogRecordHeader, RecordType};
use libfuzzer_sys::fuzz_target;

const MAX_RECORDS: usize = 64;
const MAX_PAYLOAD: usize = 1024;

fn run(u: &mut Unstructured) -> arbitrary::Result<()> {
    let generation = u64::arbitrary(u)?;
    let start_seq = u64::arbitrary(u)?;

    let mut chain = ChainValidator::new(generation, start_seq);
    let mut stopped = false;
    let mut accepted = 0u64;
    let mut last_accepted: Option<u64> = None;
    let mut expected = start_seq;

    for _ in 0..MAX_RECORDS {
        if u.is_empty() {
            break;
        }
        let knobs = u8::arbitrary(u)?;

        // Die Sequenznummer wird meist relativ zur Erwartung gezogen. Rein
        // zufaellige u64 wuerden den erwarteten Wert praktisch nie treffen,
        // und damit bliebe der interessante Teil des Validators ungetestet.
        let offset = u8::arbitrary(u)? as u64;
        let seq = match knobs & 0b11 {
            0 => expected,
            1 => expected.wrapping_add(offset),
            2 => expected.wrapping_sub(offset),
            _ => u64::arbitrary(u)?,
        };
        let record_generation = if knobs & 0b100 == 0 {
            generation
        } else {
            u64::arbitrary(u)?
        };

        let payload_len = usize::from(u16::arbitrary(u)?) % MAX_PAYLOAD;
        let payload = u.bytes(payload_len)?.to_vec();

        let mut header =
            LogRecordHeader::write(seq, u16::arbitrary(u)?, u64::arbitrary(u)?, &payload);
        header.generation = record_generation;
        header.commit_unix = u64::arbitrary(u)?;
        header.record_type = match knobs >> 4 & 0b11 {
            0 | 1 => RecordType::Write,
            2 => RecordType::Checkpoint,
            _ => RecordType::Padding,
        };
        if knobs & 0b1000 != 0 {
            // Torn write: Der Header ist heil, die Nutzdaten sind es nicht.
            header.payload_crc32c = u32::arbitrary(u)?;
        }

        // Der Weg ueber die Platte gehoert dazu: Was der Validator sieht, hat
        // vorher einen Encode/Decode-Zyklus hinter sich.
        let bytes = header.encode();
        let decoded = LogRecordHeader::decode(&bytes).expect("eigenes Encoding muss lesbar sein");
        assert_eq!(decoded, header);

        match chain.offer(&decoded, &payload) {
            ChainVerdict::Accept => {
                assert!(
                    !stopped,
                    "Accept nach StopReplay — Replay wuerde alte Daten ueber neue schreiben"
                );
                accepted += 1;
                last_accepted = Some(seq);
                expected = seq.wrapping_add(1);
                // Abschnitt 5.2: Auf `seq == u64::MAX` kann kein Nachfolger
                // folgen. Der Record selbst wird noch angenommen, danach ist
                // die Kette zu Ende — alles Weitere waere ein Record aus einer
                // frueheren Runde des Ringpuffers.
                if seq == u64::MAX {
                    stopped = true;
                }
            }
            ChainVerdict::StopReplay(_) => {
                stopped = true;
            }
        }

        assert_eq!(chain.is_broken(), stopped);
        assert_eq!(chain.accepted_count(), accepted);
        assert_eq!(chain.last_accepted_seq(), last_accepted);
    }

    Ok(())
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let _ = run(&mut u);
});
