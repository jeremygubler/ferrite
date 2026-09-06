// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Die FUSE-Schale: der Pool als eingehaengtes Dateisystem.
//!
//! # Warum von Hand und nicht mit einem Framework
//!
//! Dieselbe Ueberlegung wie beim ublk-Target. Der Passthrough — der Kernel
//! reicht Lesen und Schreiben direkt an die Datei auf dem Branch weiter,
//! statt sie durch diesen Prozess zu schicken — ist die Eigenschaft, wegen
//! der der Pool ueberhaupt tragbar ist. Er haengt an einem `ioctl` und an
//! einem Flag im `OPEN`, und beides muss durch die Bindung hindurch
//! erreichbar sein. Eine Bindung, die den Datenpfad selbst in der Hand haelt,
//! kann ihn nicht abgeben.
//!
//! Was hier steht, ist deshalb duenn und vollstaendig sichtbar: die ABI aus
//! `linux/fuse.h`, `/dev/fuse`, `mount(2)`, eine Schleife.
//!
//! # Was steht und was fehlt
//!
//! Der Lesepfad steht: einhaengen, nachschlagen, Attribute, Verzeichnisse
//! vereinigt auflisten, oeffnen, lesen, Symlinks, `statfs`. Ein Pool laesst
//! sich damit einhaengen und benutzen.
//!
//! Der Schreibpfad fehlt noch, und mit ihm der Passthrough. Beides ist eine
//! eigene Aenderung — ein halb gebauter Schreibpfad in einem Dateisystem ist
//! schlimmer als keiner.
//!
//! # Voraussetzungen
//!
//! Linux, `/dev/fuse`, und das Recht einzuhaengen. Fuer den spaeteren
//! Passthrough zusaetzlich Kernel ≥ 6.9.

pub mod abi;
pub mod backing;
pub mod connection;
pub mod inode;
pub mod server;

pub use backing::BranchRoot;
pub use connection::{Connection, MountOptions};
pub use inode::InodeTable;
pub use server::PoolFs;

use std::path::Path;

use crate::error::Result;

/// Haengt einen Pool ein und bedient ihn, bis er ausgehaengt wird.
///
/// Kehrt zurueck, wenn der Kernel die Verbindung schliesst — also nach einem
/// `umount`. Ein Fehler unterwegs beendet die Schleife und haengt beim
/// Aufraeumen aus; ein Einhaengepunkt ohne Server dahinter laesst sonst jeden
/// Zugriff darauf haengen.
pub fn mount_and_serve(
    mountpoint: &Path,
    branches: Vec<BranchRoot>,
    options: &MountOptions,
) -> Result<()> {
    let connection = Connection::mount(mountpoint, options)?;
    let mut filesystem = PoolFs::new(branches);
    filesystem.run(&connection)
}
