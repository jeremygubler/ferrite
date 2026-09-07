// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Schreibt die Handbuchseite auf die Standardausgabe.
//!
//! Damit `packaging/ferrite.8` nicht neben der Hilfe her gepflegt wird,
//! sondern aus ihr entsteht:
//!
//! ```text
//! cargo run -p ferrite-ctl --example dump_man > packaging/ferrite.8
//! ```
//!
//! `ctl/tests/packaging.rs` haelt die eingecheckte Datei dagegen.

fn main() {
    print!("{}", ferrite_ctl::man::page());
}
