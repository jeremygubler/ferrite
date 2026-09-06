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
//! Lesen und Schreiben stehen: einhaengen, nachschlagen, Attribute,
//! Verzeichnisse vereinigt auflisten, oeffnen, lesen, schreiben, anlegen,
//! loeschen, umbenennen, Rechte und Zeitstempel setzen, Symlinks, Hardlinks,
//! `statfs`. Wohin ein neues Objekt gehoert, entscheidet
//! [`place`](crate::place) — die Regeln aus `policy` gelten hier also
//! wirklich und nicht nur auf dem Papier.
//!
//! Es fehlt der Passthrough. Bis dahin geht jedes Byte durch diesen Prozess;
//! richtig ist das Ergebnis auch so, nur langsamer.
//!
//! # Was ein Pool nicht kann
//!
//! **Erweiterte Attribute** (`getxattr` und Verwandte) beantwortet er mit
//! `ENOSYS`. Sie tragen unter anderem POSIX-ACLs, und eine ACL, die nur auf
//! einem von mehreren Branches eines Verzeichnisses liegt, gilt je nachdem,
//! welcher gerade bedient. Das gehoert entschieden, bevor es gebaut wird.
//!
//! **Sperren** (`SETLK`, `GETLK`) ebenso: Eine Sperre ueber Platten hinweg
//! braucht eine Stelle, die sie fuehrt.
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
use crate::policy::SharePolicy;

/// Haengt einen Pool ein und bedient ihn, bis er ausgehaengt wird.
///
/// Kehrt zurueck, wenn der Kernel die Verbindung schliesst — also nach einem
/// `umount`. Ein Fehler unterwegs beendet die Schleife und haengt beim
/// Aufraeumen aus; ein Einhaengepunkt ohne Server dahinter laesst sonst jeden
/// Zugriff darauf haengen.
pub fn mount_and_serve(
    mountpoint: &Path,
    branches: Vec<BranchRoot>,
    policy: SharePolicy,
    options: &MountOptions,
) -> Result<()> {
    let connection = Connection::mount(mountpoint, options)?;
    let mut filesystem = PoolFs::new(branches, policy);
    filesystem.run(&connection)
}
