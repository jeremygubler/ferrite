// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Meldet, was der Flush-Test aus Abschnitt 5.3 zu einem Geraet sagt.
//!
//! ```text
//! cargo run -p ferrite-engine --example flush-report -- /dev/sda
//! ```
//!
//! Schreibt nichts. Die Schreibprobe zerstoert den Bereich, auf den sie zeigt,
//! und gehoert deshalb ins Anlegen eines Arrays, nicht in eine Diagnose.
//!
//! Der Sinn ist, dass die Entscheidung nachvollziehbar ist statt begruendet:
//! Ausgegeben werden die Fakten, auf denen sie beruht, und nicht nur ihr
//! Ergebnis.

use ferrite_engine::{check_flush, MemberDevice, WriteMode};

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("Aufruf: flush-report <Geraet>");
        std::process::exit(2);
    };

    let device = match MemberDevice::open_read_only(&path) {
        Ok(device) => device,
        Err(error) => {
            eprintln!("{path}: {error}");
            std::process::exit(1);
        }
    };

    let check = check_flush(&device, None);
    let facts = &check.facts;

    println!("Geraet          {path}");
    println!("Groesse         {} Bytes", device.size());
    println!("Art             {:?}", facts.kind);
    println!("Schreibcache    {:?}", facts.write_cache);
    println!("Virtualisiert   {:?}", facts.virtualized);
    println!("FLUSH ok        {}", facts.flush_succeeded);
    println!("Schreibprobe    {:?}", facts.write_read_back);
    println!();
    println!("Ergebnis        {:?}", check.verdict);
    println!("Begruendung     {}", check.reason);
    println!(
        "Betriebsmodus   {}",
        match check.write_mode() {
            WriteMode::WriteBack => "Write-Back (Bestaetigung, sobald der Log-Record durable ist)",
            WriteMode::WriteThrough =>
                "Write-Through (Bestaetigung erst nach Data-Member und Paritaet)",
        }
    );
}
