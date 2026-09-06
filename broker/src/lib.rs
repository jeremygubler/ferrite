// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Der Repair-Broker, Meilenstein 4.
//!
//! # Wozu er da ist
//!
//! Pruefsummen ohne Redundanz koennen nur melden. Redundanz ohne Pruefsummen
//! merkt nichts. Ferrite hat beides, aber in zwei verschiedenen Schichten: Die
//! Pruefsummen liegen bei btrfs auf dem jeweiligen Data-Member (Regel 7 aus
//! `CLAUDE.md`), die Redundanz liegt eine Ebene tiefer in der Paritaet. Dieses
//! Crate ist das Stueck dazwischen — es nimmt btrfs' Befund entgegen und holt
//! den Inhalt aus der Paritaet zurueck.
//!
//! # Woher der Befund kommt
//!
//! Aus dem Kernel-Ringpuffer. btrfs meldet einen Pruefsummenfehler in der Form
//!
//! ```text
//! BTRFS warning (device ublkb0): checksum error at logical 22020096 on dev
//! /dev/ublkb0, physical 22020096, root 5, inode 257, offset 0, length 4096,
//! links 1 (path: datei)
//! ```
//!
//! Entscheidend ist `physical`: der Offset **auf dem Blockgeraet**, das Ferrite
//! bereitstellt. Da das ublk-Target den Offset eines Gastes unveraendert an
//! `ArrayWriter::read` weiterreicht, ist dieser Offset zugleich der Offset in
//! der Payload-Region des Members. Das ist die tragende Annahme der ganzen
//! Zuordnung, und sie steht deshalb hier und nicht als Kommentar in einer
//! Schleife.
//!
//! Diese Zeilen erzeugt der **Scrub**. Ein gewoehnlicher Lesefehler nennt nur
//! den Offset innerhalb der Datei, nicht den auf der Platte; ihn umzurechnen
//! hiesse, den Chunk-Baum von btrfs zu lesen. Der Weg ist offen, aber er ist
//! nicht dieser hier — und ein Broker, der so tut, als kenne er den Offset,
//! repariert an der falschen Stelle.
//!
//! # Warum eine gefaelschte Meldung nichts anrichtet
//!
//! `/dev/kmsg` ist beschreibbar. Wer eine Meldung erfindet, erreicht damit
//! einen Reparaturversuch auf einem Bereich, der in Ordnung ist. Der endet in
//! [`Repair::AlreadyIntact`](ferrite_engine::Repair::AlreadyIntact): Die
//! Rekonstruktion aus P und aus Q ergibt genau das, was schon dasteht, und
//! geschrieben wird nichts. Kosten: etwas I/O. Schaden: keiner. Diese
//! Eigenschaft folgt daraus, dass `ArrayWriter::repair` die Rekonstruktion
//! gegenpruefen muss, statt ihr zu glauben.
//!
//! # Was hier bewusst fehlt
//!
//! Der Broker startet keinen Scrub und oeffnet kein Array. Beides ist
//! Betriebsfuehrung und gehoert in `ctl/`; hier steht die Mechanik, die sich
//! ohne Daemon testen laesst.

pub mod btrfs;
pub mod damage;
pub mod error;
/// Der Kernel-Ringpuffer. Braucht `/dev/kmsg` und damit Linux.
#[cfg(target_os = "linux")]
pub mod kmsg;
pub mod repair;

pub use btrfs::{parse_scrub_error, ScrubError};
pub use damage::{coalesce, DamageReport, SlotMap};
pub use error::{BrokerError, Result};
pub use repair::{Classified, RepairBroker, RepairStats};
