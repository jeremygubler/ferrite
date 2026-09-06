// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Schreibt die Beispielkonfiguration auf die Standardausgabe.
//!
//! Damit `packaging/ferrite.conf.example` nicht abgeschrieben ist, sondern
//! aus derselben Konstante kommt, die `the_example_parses` prueft:
//!
//! ```text
//! cargo run -p ferrite-ctl --example dump_config > packaging/ferrite.conf.example
//! ```

fn main() {
    print!("{}", ferrite_ctl::config::EXAMPLE);
}
