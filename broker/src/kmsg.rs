// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Der Kernel-Ringpuffer als Quelle der Befunde.
//!
//! `/dev/kmsg` hat drei Eigenheiten, die ein gewoehnlicher Reader nicht kennt
//! und die deshalb hier ausdruecklich behandelt werden:
//!
//! 1. **Ein `read` liefert genau einen Datensatz**, nicht so viele Bytes wie
//!    hineinpassen. Zeilenweises Puffern waere hier falsch.
//! 2. **`EPIPE` ist kein Fehler, sondern eine Auskunft**: Der Datensatz, an
//!    dem wir standen, wurde ueberschrieben, waehrend wir zurueckhingen. Die
//!    Leseposition springt dabei auf den aeltesten noch vorhandenen Datensatz
//!    — es geht also weiter, nur mit einer Luecke. Verschwiegen wird sie
//!    nicht: [`KmsgReader::overrun`] zaehlt sie mit.
//! 3. **`SEEK_END` bedeutet hinter den letzten Datensatz.** Ohne das laese ein
//!    frisch gestarteter Broker den gesamten Rueckstand seit dem Boot und
//!    reparierte Bereiche, die laengst in Ordnung sind.
//!
//! Geoeffnet wird nicht-blockierend. Ein Broker, der im `read` haengt, ist
//! einer, der sich nicht mehr beenden laesst.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;

use crate::error::{io_error, Result};

/// Groesse des Lesepuffers.
///
/// Der Kernel schneidet einen Datensatz auf `PRINTKRB_RECORD_MAX` ab, das sind
/// derzeit 1024 Bytes Text plus Praefix und Fortsetzungszeilen. 8 KiB sind
/// reichlich; ein zu kleiner Puffer ergaebe `EINVAL` und keinen kurzen Read.
const BUFFER: usize = 8192;

/// Liest Meldungen aus `/dev/kmsg`.
#[derive(Debug)]
pub struct KmsgReader {
    file: File,
    buffer: Vec<u8>,
    overrun: u64,
}

impl KmsgReader {
    /// Oeffnet den Ringpuffer und stellt sich hinter den letzten Datensatz.
    ///
    /// Das ist die Betriebsart: Was vor dem Start des Brokers passiert ist,
    /// geht ihn nichts an.
    pub fn open() -> Result<Self> {
        let mut reader = Self::open_from_start()?;
        reader
            .file
            .seek(SeekFrom::End(0))
            .map_err(io_error("ans Ende des Kernel-Ringpuffers springen"))?;
        Ok(reader)
    }

    /// Oeffnet den Ringpuffer am aeltesten noch vorhandenen Datensatz.
    ///
    /// Fuer den Fall, dass der Befund schon dasteht — etwa wenn ein Scrub
    /// gelaufen ist, bevor der Broker startete.
    pub fn open_from_start() -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open("/dev/kmsg")
            .map_err(io_error("/dev/kmsg oeffnen"))?;
        Ok(KmsgReader {
            file,
            buffer: vec![0u8; BUFFER],
            overrun: 0,
        })
    }

    /// Wieviele Datensaetze verlorengingen, weil der Puffer ueberlief.
    ///
    /// Ein Wert ungleich null heisst: Es kann ein Befund darunter gewesen
    /// sein. Der naechste Scrub findet ihn wieder — aber wissen soll man es.
    pub fn overrun(&self) -> u64 {
        self.overrun
    }

    /// Die naechste Meldung, oder `None`, wenn gerade keine da ist.
    ///
    /// Blockiert nicht. `None` heisst „im Moment nichts“ und nicht „nie
    /// wieder“ — der Aufrufer entscheidet, wann er wieder fragt.
    pub fn next_message(&mut self) -> Result<Option<String>> {
        loop {
            match self.file.read(&mut self.buffer) {
                Ok(0) => return Ok(None),
                Ok(read) => return Ok(Some(message_of(&self.buffer[..read]))),
                Err(error) => match error.kind() {
                    // Nicht-blockierend und nichts da.
                    ErrorKind::WouldBlock => return Ok(None),
                    ErrorKind::Interrupted => continue,
                    _ if error.raw_os_error() == Some(libc::EPIPE) => {
                        // Der Puffer ist uns davongelaufen. Die Position steht
                        // jetzt auf dem aeltesten vorhandenen Datensatz, also
                        // noch einmal von vorn — mit einem Strich in der
                        // Rechnung.
                        self.overrun += 1;
                        continue;
                    }
                    _ => return Err(io_error("Kernel-Ringpuffer lesen")(error)),
                },
            }
        }
    }

    /// Alles, was gerade dasteht.
    ///
    /// Bricht nach `limit` Meldungen ab, damit ein volllaufender Puffer den
    /// Aufrufer nicht endlos beschaeftigt.
    pub fn drain(&mut self, limit: usize) -> Result<Vec<String>> {
        let mut messages = Vec::new();
        while messages.len() < limit {
            match self.next_message()? {
                Some(message) => messages.push(message),
                None => break,
            }
        }
        Ok(messages)
    }
}

/// Schaelt den Text aus einem Datensatz.
///
/// Das Format ist `prio,seq,zeit,flags[,...];text\n` mit optionalen
/// Fortsetzungszeilen, die mit einem Leerzeichen beginnen. Gebraucht wird nur
/// der Text bis zum ersten Zeilenumbruch — die Schluessel-Wert-Paare dahinter
/// tragen zur Frage nichts bei.
fn message_of(record: &[u8]) -> String {
    let text = match record.iter().position(|byte| *byte == b';') {
        Some(position) => &record[position + 1..],
        // Kein Semikolon: kein Praefix, das sich abtrennen liesse. Dann lieber
        // der ganze Datensatz als gar nichts — der Parser sortiert ihn aus,
        // wenn er nichts hergibt.
        None => record,
    };
    let end = text
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap_or(text.len());
    String::from_utf8_lossy(&text[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prefix_is_stripped() {
        let record = b"4,1234,56789,-;BTRFS warning (device ublkb0): checksum error\n";
        assert_eq!(
            message_of(record),
            "BTRFS warning (device ublkb0): checksum error"
        );
    }

    #[test]
    fn continuation_lines_are_dropped() {
        let record = b"6,42,1,-;etwas\n SUBSYSTEM=block\n DEVICE=b8:1\n";
        assert_eq!(message_of(record), "etwas");
    }

    #[test]
    fn a_record_without_prefix_is_kept_whole() {
        assert_eq!(message_of(b"kein Praefix\n"), "kein Praefix");
    }

    #[test]
    fn a_record_without_newline_is_kept_whole() {
        assert_eq!(message_of(b"4,1,1,-;abgeschnitten"), "abgeschnitten");
    }

    #[test]
    fn an_empty_record_yields_an_empty_message() {
        assert_eq!(message_of(b""), "");
    }
}
