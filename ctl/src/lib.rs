// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Das Werkzeug, mit dem ein Mensch ein Ferrite-Array bedient.
//!
//! # Wofuer es da ist
//!
//! Bis hierher war Ferrite eine Bibliothek: Wer ein Array anlegen wollte,
//! musste Rust schreiben. Das README verspricht aber, dass die Engine „als
//! Paket neben einem bestehenden Setup laeuft" und jeder sie testen kann —
//! und ein Speichersystem, das nur sein Entwickler starten kann, findet die
//! Fehler nicht, die es finden muss.
//!
//! # Aufbau
//!
//! Wie ueberall im Projekt: was rechnet, getrennt von dem, was I/O macht.
//! [`args`] zerlegt die Kommandozeile, [`report`] baut den Text ueber ein
//! Array — beide ohne ein Geraet anzufassen und deshalb vollstaendig
//! pruefbar. Nur [`run`] oeffnet Platten.
//!
//! # Kein Daemon
//!
//! Noch nicht. `create` und `status` sind Einzelaufrufe: Sie tun etwas und
//! kehren zurueck. Der laufende Betrieb — ublk-Geraete und der eingehaengte
//! Pool — braucht einen Prozess, der bleibt, und das ist der naechste Schritt.

pub mod args;
pub mod config;
/// Ferrite findet seine Platten selbst. Die Suche braucht ein Betriebssystem
/// mit Blockgeraeten, das Ordnen der Fundstellen nicht.
pub mod discover;
pub mod journal;
/// Die Handbuchseite, erzeugt aus derselben Quelle wie die Hilfe.
pub mod man;
/// Scrub, Ersatz und Rebuild. Braucht Geraete, aber weder ublk noch FUSE.
#[cfg(unix)]
pub mod repair;
pub mod report;
/// Die Aufrufe, die ein Geraet anfassen. Braucht ein Betriebssystem mit
/// Blockgeraeten.
#[cfg(unix)]
pub mod run;
/// Der laufende Betrieb. Braucht ublk und FUSE und damit Linux.
#[cfg(target_os = "linux")]
pub mod serve;

pub use args::{parse, ArgError, Command, CreatePlan, RunPlan, StatusRequest, HELP};
pub use report::{status, Health, Report, Seen};
