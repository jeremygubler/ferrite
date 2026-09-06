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
//! Der Passthrough steht ebenfalls. Auf einem Kernel ab 6.9 bekommt der
//! Kernel beim `OPEN` den Dateideskriptor der Datei auf dem Branch hinterlegt
//! und bedient Lesen und Schreiben danach selbst — dieser Prozess sieht kein
//! Byte mehr. Fehlt eine der Voraussetzungen, laeuft alles durch ihn:
//! langsamer, aber richtig. Der Rueckfall ist deshalb kein Fehlerpfad,
//! sondern der zweite gewoehnliche Ausgang.
//!
//! Es braucht dafuer dreierlei, und jedes einzelne scheitert still:
//! `FUSE_PASSTHROUGH` in `flags2` der `INIT`-Antwort, `FUSE_INIT_EXT` in
//! `flags` — ohne das sieht der Kernel `flags2` gar nicht an — und ein
//! `max_stack_depth` groesser null. Ist eines falsch, gelingt der Mount, und
//! nur der Datendurchsatz bleibt zurueck. Deshalb zaehlt
//! [`Counters`](server::Counters) mit, und deshalb pruefen die Mount-Tests
//! nicht den Inhalt, sondern die Zahl der `READ`-Anfragen, die hier ankamen.
//!
//! # Erweiterte Attribute
//!
//! Die Frage war, welcher Branch zaehlt, wenn ein Verzeichnis auf mehreren
//! liegt. Die Antwort ist dieselbe wie bei `chmod`: **gelesen wird vom
//! bedienenden Branch, geschrieben auf jeden, der den Namen traegt.** Fuer
//! eine Datei ist das genau einer. Und wenn ein Verzeichnis auf einer
//! zweiten Platte entsteht, kommen seine Attribute mit — sonst bekaeme eine
//! Datei je nach Platte andere Rechte, sobald eine Default-ACL im Spiel ist.
//!
//! Gefiltert wird kein Namensraum. Wer `trusted.*` oder `security.*` setzen
//! darf, entscheidet der Kernel im VFS anhand der Rechte des Aufrufers,
//! bevor die Anfrage hier ankommt; eine zweite Pruefung an dieser Stelle
//! waere eine, die irgendwann von der ersten abweicht.
//!
//! # POSIX-ACLs
//!
//! ACLs sind der Grund, warum ein NAS erweiterte Attribute braucht — Samba
//! legt seine Rechte dort ab. Sie laufen durch dieselben Handler wie jedes
//! andere Attribut und liegen deshalb ebenfalls auf jedem Branch, der den
//! Namen traegt.
//!
//! Angemeldet werden `FUSE_POSIX_ACL` **und** `FUSE_DONT_MASK`, und zwar
//! zusammen. Ohne das zweite Bit zieht der Kernel die `umask` weiter selbst
//! ab, und dann verliert jede Default-ACL gegen sie: Eine Datei in einem
//! Verzeichnis, das der Gruppe Schreibrecht vererben soll, entsteht als
//! `0600`. Das ist der stille Fall — `setfacl` gelingt, das Attribut liegt
//! auf der Platte, und trotzdem gilt es nicht.
//!
//! Mit den beiden Bits liegt die `umask` hier, und es gilt POSIX.1e: Traegt
//! das Elternverzeichnis eine Default-ACL, vergibt sie die Rechte; sonst
//! zieht die `umask` ab. Beide Wege stehen als Test, samt der Gegenprobe mit
//! abgeschalteter Aushandlung.
//!
//! # Was ein Pool nicht kann
//!
//! **Sperren** (`SETLK`, `GETLK`) beantwortet dieser Server nicht — und das
//! ist Absicht, siehe [`server`].
//!
//! # Voraussetzungen
//!
//! Linux, `/dev/fuse`, und das Recht einzuhaengen. Fuer den Passthrough
//! zusaetzlich Kernel ≥ 6.9 und `CAP_SYS_ADMIN` — das `ioctl`, das einen
//! Deskriptor hinterlegt, verlangt es.

pub mod abi;
pub mod backing;
pub mod connection;
pub mod inode;
pub mod server;

pub use backing::BranchRoot;
pub use connection::{Connection, MountOptions};
pub use inode::InodeTable;
pub use server::{Counters, PoolFs};

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
