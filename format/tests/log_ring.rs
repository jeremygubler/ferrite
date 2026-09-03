// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Tests gegen Abschnitt 5.1 und 5.2 von `docs/FORMAT.md`.
//!
//! Das ist der Absturzpfad. Die Tests, auf die es ankommt, sind nicht die, in
//! denen alles heil ist, sondern die, in denen im Ringpuffer noch Reste einer
//! frueheren Runde liegen — genau das hinterlaesst ein Stromausfall.

use ferrite_format::log::ring::{LogRing, LogWriter, ReplayStop};
use ferrite_format::log::{ChainBreak, LogRecordHeader, RecordType, LOG_SECTOR_SIZE};
use ferrite_format::FormatError;

const GENERATION: u64 = 7;

/// Ein Write-Record mit `generation` gesetzt und einer Nutzlast, die aus `seq`
/// folgt — damit ein vertauschter Record auffaellt.
fn write_record(seq: u64, payload_len: usize) -> (LogRecordHeader, Vec<u8>) {
    let payload = vec![(seq & 0xFF) as u8; payload_len];
    let mut header = LogRecordHeader::write(seq, 0, seq * 4096, &payload);
    header.generation = GENERATION;
    (header, payload)
}

fn checkpoint(seq: u64) -> (LogRecordHeader, Vec<u8>) {
    let mut header = LogRecordHeader::checkpoint(seq);
    header.generation = GENERATION;
    (header, Vec::new())
}

/// Schreibt eine Folge von Records in eine frische Region.
fn log_with(sectors: usize, records: &[(LogRecordHeader, Vec<u8>)]) -> Vec<u8> {
    let mut region = vec![0u8; sectors * LOG_SECTOR_SIZE];
    {
        let mut writer = LogWriter::new(&mut region).unwrap();
        for (header, payload) in records {
            writer.append(header, payload).unwrap();
        }
    }
    region
}

fn replayed_seqs(region: &[u8]) -> Vec<u64> {
    LogRing::new(region)
        .unwrap()
        .replay(GENERATION)
        .map(|record| record.header.seq)
        .collect()
}

#[test]
fn replays_a_contiguous_run() {
    let records: Vec<_> = (1..=5).map(|seq| write_record(seq, 100)).collect();
    let region = log_with(32, &records);

    let ring = LogRing::new(&region).unwrap();
    let mut replay = ring.replay(GENERATION);

    let seen: Vec<_> = replay.by_ref().collect();
    assert_eq!(seen.len(), 5);
    for (index, record) in seen.iter().enumerate() {
        let seq = index as u64 + 1;
        assert_eq!(record.header.seq, seq);
        assert_eq!(record.payload, vec![(seq & 0xFF) as u8; 100]);
        record.header.verify_payload(record.payload).unwrap();
    }
    assert_eq!(replay.accepted_count(), 5);
    assert_eq!(replay.last_accepted_seq(), Some(5));
}

#[test]
fn an_empty_log_replays_nothing() {
    let region = vec![0u8; 8 * LOG_SECTOR_SIZE];
    let ring = LogRing::new(&region).unwrap();
    let mut replay = ring.replay(GENERATION);
    assert!(replay.next().is_none());
    assert_eq!(replay.accepted_count(), 0);
}

// --- Schritt 1 und 2 -----------------------------------------------------

#[test]
fn scan_finds_every_written_header() {
    let records: Vec<_> = (1..=4).map(|seq| write_record(seq, 8000)).collect();
    let region = log_with(32, &records);

    // 64 + 8000 rundet auf 8192 auf, jeder Record belegt also zwei Sektoren.
    // Der Header steht im ersten, der zweite traegt nur Nutzdaten.
    let found: Vec<_> = LogRing::new(&region).unwrap().scan().collect();
    assert_eq!(found.len(), 4);
    assert_eq!(
        found.iter().map(|(sector, _)| *sector).collect::<Vec<_>>(),
        vec![0, 2, 4, 6]
    );
}

#[test]
fn replay_starts_after_the_newest_checkpoint() {
    // Alles bis 3 ist persistent. Nur 4 und 5 duerfen noch einmal laufen.
    let mut records: Vec<_> = (1..=3).map(|seq| write_record(seq, 64)).collect();
    records.push(checkpoint(4));
    records.push(write_record(5, 64));
    records.push(write_record(6, 64));
    let region = log_with(32, &records);

    let ring = LogRing::new(&region).unwrap();
    assert_eq!(ring.newest_checkpoint().unwrap().1.seq, 4);
    assert_eq!(replayed_seqs(&region), vec![5, 6]);
}

#[test]
fn the_newest_checkpoint_wins_over_an_older_one() {
    let mut records: Vec<_> = vec![checkpoint(1)];
    records.push(write_record(2, 64));
    records.push(checkpoint(3));
    records.push(write_record(4, 64));
    let region = log_with(32, &records);

    let ring = LogRing::new(&region).unwrap();
    assert_eq!(ring.newest_checkpoint().unwrap().1.seq, 3);
    assert_eq!(replayed_seqs(&region), vec![4]);
}

#[test]
fn without_a_checkpoint_the_replay_starts_at_the_lowest_sequence() {
    // Abschnitt 5.2 Schritt 2, zweiter Satz. Der Record mit der niedrigsten
    // `seq` gehoert selbst schon zum Replay.
    let records: Vec<_> = (10..=13).map(|seq| write_record(seq, 64)).collect();
    let region = log_with(32, &records);

    let ring = LogRing::new(&region).unwrap();
    assert!(ring.newest_checkpoint().is_none());
    assert_eq!(ring.lowest_sequence().unwrap().1.seq, 10);
    assert_eq!(replayed_seqs(&region), vec![10, 11, 12, 13]);
}

#[test]
fn a_checkpoint_at_the_end_of_the_sequence_space_replays_nothing() {
    // Auf `seq == u64::MAX` kann kein Nachfolger folgen.
    let region = log_with(8, &[checkpoint(u64::MAX)]);
    let mut replay = LogRing::new(&region).unwrap().replay(GENERATION);
    assert!(replay.next().is_none());
    assert_eq!(replay.stop(), Some(ReplayStop::RingExhausted));
}

// --- Abschnitt 5.1: Umbruch am Ende --------------------------------------

#[test]
fn a_record_that_does_not_fit_wraps_to_the_start() {
    // Region: 8 Sektoren. Drei Records zu je drei Sektoren (64 + 10000 rundet
    // auf 12288 auf) passen nicht — der dritte muss vorn wieder anfangen, davor
    // steht ein Padding.
    let records: Vec<_> = (1..=3).map(|seq| write_record(seq, 10000)).collect();
    let region = log_with(8, &records);

    let padding: Vec<_> = LogRing::new(&region)
        .unwrap()
        .scan()
        .filter(|(_, header)| header.record_type == RecordType::Padding)
        .collect();
    assert_eq!(padding.len(), 1, "genau ein Padding-Record");
    let (sector, header) = &padding[0];
    assert_eq!(*sector, 6, "Padding steht hinter den ersten beiden Records");
    // Zwei Sektoren blieben uebrig, davon 64 Bytes fuer den eigenen Header.
    assert_eq!(header.payload_len as usize, 2 * LOG_SECTOR_SIZE - 64);
    assert_eq!(header.on_disk_len(), 2 * LOG_SECTOR_SIZE);
}

#[test]
fn the_replay_follows_the_wrap() {
    let records: Vec<_> = (1..=3).map(|seq| write_record(seq, 10000)).collect();
    let region = log_with(8, &records);

    // Record 3 landet nach dem Umbruch bei Offset 0 und ueberschreibt dabei
    // Record 1 — so arbeitet ein Ringpuffer. Wann das erlaubt ist, entscheidet
    // der Checkpoint, und das ist Sache der Engine, nicht dieses Moduls.
    // Uebrig bleiben Record 2 und 3, und der Replay muss sie in dieser
    // Reihenfolge liefern, obwohl 3 physisch vor 2 liegt.
    assert_eq!(replayed_seqs(&region), vec![2, 3]);
}

#[test]
fn append_zeroes_the_rest_of_the_last_sector() {
    // Abschnitt 5.1: Ohne das Nullen bliebe im Rest des Sektors stehen, was
    // eine fruehere Runde dort hinterlassen hat — und der Scan sieht jeden
    // Sektor an.
    let mut region = vec![0xFFu8; 8 * LOG_SECTOR_SIZE];
    let (header, payload) = write_record(1, 10);
    {
        let mut writer = LogWriter::new(&mut region).unwrap();
        writer.append(&header, &payload).unwrap();
    }
    assert!(region[64 + 10..LOG_SECTOR_SIZE].iter().all(|&b| b == 0));
    // Ausserhalb des Records bleibt alles unberuehrt.
    assert!(region[LOG_SECTOR_SIZE..].iter().all(|&b| b == 0xFF));
}

// --- Schritt 4: der Punkt, an dem still Daten verlorengehen ---------------

#[test]
fn a_stale_record_from_the_previous_round_stops_the_replay() {
    // Der Fall, um den es geht. Der Ringpuffer ist einmal voll gelaufen, dann
    // stuerzt die Maschine ab, waehrend vorne neu geschrieben wird. Hinter dem
    // letzten frischen Record steht ein alter, in sich vollkommen gueltiger
    // Record aus der ersten Runde. Wer den mitnimmt, schreibt alte Daten ueber
    // neue.
    let mut region = vec![0u8; 8 * LOG_SECTOR_SIZE];
    {
        let mut writer = LogWriter::new(&mut region).unwrap();
        // Erste Runde fuellt den Ring genau aus: sieben Writes und ein
        // Checkpoint, je ein Sektor.
        for seq in 1..=7 {
            let (header, payload) = write_record(seq, 64);
            writer.append(&header, &payload).unwrap();
        }
        let (header, payload) = checkpoint(8);
        writer.append(&header, &payload).unwrap();
        assert_eq!(writer.head(), 0, "der Ring ist einmal voll");

        // Zweite Runde: 9 und 10 landen vorn, dann stirbt die Maschine. In
        // Sektor 2 steht noch die alte 3 aus der ersten Runde.
        for seq in 9..=10 {
            let (header, payload) = write_record(seq, 64);
            writer.append(&header, &payload).unwrap();
        }
    }

    // Der Checkpoint sagt: alles bis 8 ist persistent, es geht bei 9 weiter.
    // 9 und 10 sind echt. Der alte Record in Sektor 2 ist fuer sich vollkommen
    // gueltig — gueltige Pruefsumme, passende Generation. Wer ihn anwendet,
    // schreibt Daten aus der ersten Runde ueber die aus der zweiten.
    let ring = LogRing::new(&region).unwrap();
    let mut replay = ring.replay(GENERATION);
    let seqs: Vec<_> = replay.by_ref().map(|r| r.header.seq).collect();
    assert_eq!(seqs, vec![9, 10]);
    assert_eq!(
        replay.stop(),
        Some(ReplayStop::Chain(ChainBreak::SequenceGap {
            expected: 11,
            found: 3
        }))
    );
}

#[test]
fn nothing_is_accepted_after_the_chain_broke() {
    // Hinter der Luecke steht ein Record, der fuer sich gueltig ist und die
    // Kette sogar fortsetzen wuerde. Er darf trotzdem nicht mehr durch.
    let mut region = vec![0u8; 16 * LOG_SECTOR_SIZE];
    {
        let mut writer = LogWriter::new(&mut region).unwrap();
        for seq in [1u64, 2, 4, 5] {
            let (header, payload) = write_record(seq, 64);
            writer.append(&header, &payload).unwrap();
        }
    }

    let ring = LogRing::new(&region).unwrap();
    let mut replay = ring.replay(GENERATION);
    let seqs: Vec<_> = replay.by_ref().map(|r| r.header.seq).collect();
    assert_eq!(seqs, vec![1, 2]);
    assert_eq!(
        replay.stop(),
        Some(ReplayStop::Chain(ChainBreak::SequenceGap {
            expected: 3,
            found: 4
        }))
    );
}

#[test]
fn a_torn_header_ends_the_replay() {
    let records: Vec<_> = (1..=4).map(|seq| write_record(seq, 64)).collect();
    let mut region = log_with(16, &records);
    // Der dritte Record beginnt im dritten Sektor. Ein einzelnes gekipptes Bit
    // im Header reicht.
    region[2 * LOG_SECTOR_SIZE + 9] ^= 0x01;

    let ring = LogRing::new(&region).unwrap();
    let mut replay = ring.replay(GENERATION);
    let seqs: Vec<_> = replay.by_ref().map(|r| r.header.seq).collect();
    assert_eq!(seqs, vec![1, 2]);
    assert_eq!(
        replay.stop(),
        Some(ReplayStop::NoHeader {
            offset: 2 * LOG_SECTOR_SIZE
        })
    );
}

#[test]
fn a_corrupt_payload_ends_the_replay() {
    let records: Vec<_> = (1..=4).map(|seq| write_record(seq, 64)).collect();
    let mut region = log_with(16, &records);
    // Header heil, Nutzdaten kaputt — der halbe Write eines Absturzes.
    region[2 * LOG_SECTOR_SIZE + 64 + 10] ^= 0xFF;

    let ring = LogRing::new(&region).unwrap();
    let mut replay = ring.replay(GENERATION);
    let seqs: Vec<_> = replay.by_ref().map(|r| r.header.seq).collect();
    assert_eq!(seqs, vec![1, 2]);
    assert_eq!(
        replay.stop(),
        Some(ReplayStop::Chain(ChainBreak::CorruptPayload))
    );
}

#[test]
fn a_record_from_another_generation_ends_the_replay() {
    let mut records: Vec<_> = (1..=2).map(|seq| write_record(seq, 64)).collect();
    let (mut header, payload) = write_record(3, 64);
    header.generation = GENERATION - 1;
    records.push((header, payload));
    records.push(write_record(4, 64));
    let region = log_with(16, &records);

    let ring = LogRing::new(&region).unwrap();
    let mut replay = ring.replay(GENERATION);
    let seqs: Vec<_> = replay.by_ref().map(|r| r.header.seq).collect();
    assert_eq!(seqs, vec![1, 2]);
    assert_eq!(
        replay.stop(),
        Some(ReplayStop::Chain(ChainBreak::GenerationMismatch {
            expected: GENERATION,
            found: GENERATION - 1
        }))
    );
}

#[test]
fn a_record_that_claims_to_run_past_the_end_is_refused() {
    // Ein Header, dessen `payload_len` ueber das Ende der Region hinausreicht.
    // Nach Abschnitt 5.1 kann das nicht sein — und der Replay darf daran
    // weder paniken noch ueber den Rand lesen.
    let mut region = log_with(4, &[write_record(1, 64)]);
    let mut header = LogRecordHeader::write(1, 0, 0, &[]);
    header.generation = GENERATION;
    header.payload_len = 1_000_000;
    region[..64].copy_from_slice(&header.encode());

    let ring = LogRing::new(&region).unwrap();
    let mut replay = ring.replay(GENERATION);
    assert!(replay.next().is_none());
    assert_eq!(replay.stop(), Some(ReplayStop::RecordPastEnd { offset: 0 }));
}

#[test]
fn a_ring_full_of_valid_records_terminates() {
    // Jeder Sektor traegt einen gueltigen, fortlaufenden Record. Der Lauf muss
    // nach einer Runde enden und darf nicht ewig im Kreis gehen.
    let records: Vec<_> = (1..=8).map(|seq| write_record(seq, 64)).collect();
    let region = log_with(8, &records);

    let ring = LogRing::new(&region).unwrap();
    let mut replay = ring.replay(GENERATION);
    let seqs: Vec<_> = replay.by_ref().map(|r| r.header.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(replay.stop(), Some(ReplayStop::RingExhausted));
}

// --- Regionsgroesse und Grenzfaelle --------------------------------------

#[test]
fn rejects_a_region_that_is_not_a_multiple_of_the_sector_size() {
    let region = vec![0u8; LOG_SECTOR_SIZE + 1];
    assert!(matches!(
        LogRing::new(&region),
        Err(FormatError::InvalidField {
            field: "log_region",
            ..
        })
    ));
}

#[test]
fn rejects_an_empty_region() {
    assert!(matches!(
        LogRing::new(&[]),
        Err(FormatError::InvalidField {
            field: "log_region",
            ..
        })
    ));
}

#[test]
fn rejects_a_record_larger_than_the_region() {
    let mut region = vec![0u8; 2 * LOG_SECTOR_SIZE];
    let mut writer = LogWriter::new(&mut region).unwrap();
    let (header, payload) = write_record(1, 3 * LOG_SECTOR_SIZE);
    assert!(matches!(
        writer.append(&header, &payload),
        Err(FormatError::InvalidField {
            field: "payload_len",
            ..
        })
    ));
}

#[test]
fn rejects_a_payload_that_does_not_match_the_header() {
    let mut region = vec![0u8; 8 * LOG_SECTOR_SIZE];
    let mut writer = LogWriter::new(&mut region).unwrap();
    let (header, payload) = write_record(1, 100);
    assert!(matches!(
        writer.append(&header, &payload[..50]),
        Err(FormatError::InvalidField {
            field: "payload_len",
            ..
        })
    ));
}

#[test]
fn the_head_must_stay_on_a_sector_boundary() {
    let mut region = vec![0u8; 8 * LOG_SECTOR_SIZE];
    let mut writer = LogWriter::new(&mut region).unwrap();
    assert!(writer.set_head(4 * LOG_SECTOR_SIZE).is_ok());
    assert!(writer.set_head(LOG_SECTOR_SIZE + 1).is_err());
    assert!(writer.set_head(8 * LOG_SECTOR_SIZE).is_err());
}

#[test]
fn a_record_that_exactly_fills_the_region_needs_no_padding() {
    // Grenzfall: Der Record endet genau am Ende. Ein Padding waere hier falsch,
    // es passt keines mehr hin.
    let mut region = vec![0u8; 2 * LOG_SECTOR_SIZE];
    {
        let mut writer = LogWriter::new(&mut region).unwrap();
        let (header, payload) = write_record(1, 2 * LOG_SECTOR_SIZE - 64);
        assert_eq!(writer.append(&header, &payload).unwrap(), 0);
        assert_eq!(writer.head(), 0, "der Kopf springt zurueck an den Anfang");
    }
    let padding_count = LogRing::new(&region)
        .unwrap()
        .scan()
        .filter(|(_, header)| header.record_type == RecordType::Padding)
        .count();
    assert_eq!(padding_count, 0);
}
