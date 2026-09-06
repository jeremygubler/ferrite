// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Der Zugriff auf einen einzelnen Branch — das btrfs eines Data-Members.
//!
//! Was `std` anbietet, wird von dort genommen: `symlink_metadata`,
//! `read_dir`, `read_link`, `File`. Der Rest geht nicht anders — `std` kennt
//! weder `statvfs` noch `mknod`, `lchown`, `utimensat` oder `symlink` mit
//! Zielpruefung. Diese Aufrufe stehen deshalb hier zusammen und nirgends
//! sonst: Jeder ist eine Zeile `unsafe`, und sie alle an einer Stelle zu
//! haben heisst, dass man sie an einer Stelle nachlesen kann.
//!
//! # Warum `symlink_metadata` und nicht `metadata`
//!
//! Ein Symlink im Pool soll ein Symlink bleiben. Wer ihm hier folgte, zeigte
//! dem Kernel die Zieldatei und liesse ihn `readlink` nie stellen — und ein
//! Symlink, der auf `/etc` zeigt, waere damit ein Loch aus dem Pool heraus.

use std::collections::HashMap;
use std::ffi::CString;
use std::fs::{File, Metadata};
use std::os::unix::fs::{DirEntryExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::branch::BranchId;
use crate::error::{PoolError, Result};
use crate::fuse::abi::Attr;
use crate::merge::EntryKind;

/// Ein Branch samt dem Verzeichnis, unter dem er im Dateisystem haengt.
#[derive(Debug, Clone)]
pub struct BranchRoot {
    pub id: BranchId,
    /// Das Wurzelverzeichnis dieses Shares auf diesem Member — also der
    /// Einhaengepunkt des btrfs plus der Name des Shares.
    pub root: PathBuf,
}

impl BranchRoot {
    pub fn new(id: BranchId, root: impl Into<PathBuf>) -> Self {
        BranchRoot {
            id,
            root: root.into(),
        }
    }

    /// Der Pfad im Dateisystem zu einem Pfad im Pool.
    ///
    /// Der Pool-Pfad muss vorher durch [`depth_of`](crate::depth_of) gegangen
    /// sein. Dann kann er kein `..` enthalten, und das Ergebnis liegt sicher
    /// unterhalb der Wurzel.
    pub fn resolve(&self, relative: &str) -> PathBuf {
        if relative.is_empty() {
            self.root.clone()
        } else {
            self.root.join(relative)
        }
    }
}

/// Was ein Branch an einer Stelle traegt, ohne einem Symlink zu folgen.
pub fn look(branch: &BranchRoot, relative: &str) -> Option<Metadata> {
    std::fs::symlink_metadata(branch.resolve(relative)).ok()
}

/// Die Art eines Eintrags aus seinen Metadaten.
pub fn kind_of(metadata: &Metadata) -> EntryKind {
    let kind = metadata.file_type();
    if kind.is_dir() {
        EntryKind::Directory
    } else if kind.is_file() {
        EntryKind::File
    } else if kind.is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::Other
    }
}

/// Ein Eintrag, wie ihn ein Branch auflistet.
#[derive(Debug, Clone)]
pub struct RawEntry {
    pub name: String,
    pub kind: EntryKind,
    /// Die Inode-Nummer des darunterliegenden Dateisystems.
    ///
    /// Kommt aus dem `d_ino` des Verzeichniseintrags und kostet damit keinen
    /// eigenen Systemaufruf. Sie dient allein der Anzeige — die Identitaet
    /// eines Objekts im Pool ist seine Nodeid, siehe [`attr_of`].
    pub ino: u64,
}

/// Listet ein Verzeichnis eines Branches auf.
///
/// Ein Branch, der das Verzeichnis gar nicht hat, liefert eine leere Liste und
/// keinen Fehler: Im Pool ist das der Normalfall, denn ein Verzeichnis muss
/// nicht auf jeder Platte liegen.
pub fn list(branch: &BranchRoot, relative: &str) -> Vec<RawEntry> {
    let Ok(entries) = std::fs::read_dir(branch.resolve(relative)) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let kind = match entry.file_type() {
                Ok(kind) if kind.is_dir() => EntryKind::Directory,
                Ok(kind) if kind.is_file() => EntryKind::File,
                Ok(kind) if kind.is_symlink() => EntryKind::Symlink,
                Ok(_) => EntryKind::Other,
                Err(_) => return None,
            };
            Some(RawEntry {
                // Kein `to_string_lossy`: Ein veraenderter Name zeigte auf
                // einen Pfad, den es nicht gibt. Siehe den Modulkopf von
                // `server` — solche Namen bleiben unsichtbar, statt falsch
                // zu sein.
                name: entry.file_name().into_string().ok()?,
                kind,
                ino: entry.ino(),
            })
        })
        .collect()
}

/// Baut die FUSE-Attribute aus den Metadaten eines Branches.
///
/// # Warum `ino` die Nodeid ist und nicht die Inode-Nummer der Platte
///
/// Zwei Members sind zwei Dateisysteme, und deren Inode-Nummern sind voneinander
/// unabhaengig — dieselbe Zahl kommt auf jeder Platte vor. Wer sie
/// durchreichte, zeigte im Pool zwei verschiedene Dateien mit derselben
/// Inode-Nummer.
///
/// Das ist kein Schoenheitsfehler. `tar`, `rsync` und `cp -a` erkennen
/// Hardlinks daran, dass zwei Eintraege dieselbe Inode-Nummer haben; sie
/// speichern den zweiten dann als Verweis auf den ersten. Beim Auspacken
/// stuende dort der Inhalt der falschen Datei — ein Datenverlust, der erst
/// beim Wiederherstellen auffiele.
///
/// Die Nodeid ist dagegen ueber den ganzen Pool eindeutig und wird nie
/// wiederverwendet.
pub fn attr_of(metadata: &Metadata, nodeid: u64) -> Attr {
    Attr {
        ino: nodeid,
        size: metadata.size(),
        blocks: metadata.blocks(),
        atime: metadata.atime(),
        mtime: metadata.mtime(),
        ctime: metadata.ctime(),
        atimensec: metadata.atime_nsec() as u32,
        mtimensec: metadata.mtime_nsec() as u32,
        ctimensec: metadata.ctime_nsec() as u32,
        mode: metadata.mode(),
        // Hardlinks ueber Branch-Grenzen kann es nicht geben, und innerhalb
        // eines Branches zaehlt das darunterliegende Dateisystem richtig.
        nlink: metadata.nlink() as u32,
        uid: metadata.uid(),
        gid: metadata.gid(),
        rdev: metadata.rdev() as u32,
        blksize: metadata.blksize() as u32,
    }
}

/// Oeffnet eine Datei auf einem Branch mit den Flags des Gastes.
///
/// `O_CREAT` und `O_EXCL` werden **nicht** durchgereicht: Anlegen ist eine
/// eigene Operation, weil erst der Pool entscheidet, auf welchen Branch die
/// neue Datei gehoert.
pub fn open(branch: &BranchRoot, relative: &str, flags: i32) -> Result<File> {
    let mut options = std::fs::OpenOptions::new();
    match flags & libc::O_ACCMODE {
        libc::O_WRONLY => options.write(true),
        libc::O_RDWR => options.read(true).write(true),
        _ => options.read(true),
    };
    let passed = flags & (libc::O_APPEND | libc::O_TRUNC | libc::O_NOATIME | libc::O_NOFOLLOW);
    options.custom_flags(passed);
    options
        .open(branch.resolve(relative))
        .map_err(io_error("Datei oeffnen"))
}

/// Legt eine neue Datei an und oeffnet sie.
///
/// `O_EXCL`: Ob die Datei schon da ist, hat der Pool ueber alle Branches
/// geprueft — aber zwischen Pruefung und Anlegen kann jemand dazwischenkommen.
/// Ohne `O_EXCL` ueberschriebe dieser Aufruf sie stillschweigend.
pub fn create_file(branch: &BranchRoot, relative: &str, mode: u32, flags: i32) -> Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create_new(true).mode(mode);
    let passed = flags & (libc::O_APPEND | libc::O_NOATIME);
    options.custom_flags(passed);
    options
        .open(branch.resolve(relative))
        .map_err(io_error("Datei anlegen"))
}

/// Legt ein Verzeichnis an, ohne Vorfahren.
pub fn make_dir(branch: &BranchRoot, relative: &str, mode: u32) -> Result<()> {
    let path = c_path(&branch.resolve(relative))?;
    check(
        unsafe { libc::mkdir(path.as_ptr(), mode as libc::mode_t) },
        "Verzeichnis anlegen",
    )
}

/// Legt einen FIFO oder Socket an.
///
/// Geraeteknoten bleiben aussen vor: Der Pool haengt mit `MS_NODEV`, dort
/// waeren sie ohnehin wirkungslos — aber auf der Platte laegen sie weiter, und
/// wer die Platte spaeter direkt einhaengt, faende ein Geraet, das er nicht
/// angelegt hat.
pub fn make_node(branch: &BranchRoot, relative: &str, mode: u32) -> Result<()> {
    let path = c_path(&branch.resolve(relative))?;
    check(
        unsafe { libc::mknod(path.as_ptr(), mode as libc::mode_t, 0) },
        "Knoten anlegen",
    )
}

pub fn make_symlink(branch: &BranchRoot, relative: &str, target: &[u8]) -> Result<()> {
    let target = CString::new(target).map_err(|_| PoolError::InvalidPath {
        reason: "Nullbyte im Symlink-Ziel",
    })?;
    let path = c_path(&branch.resolve(relative))?;
    check(
        unsafe { libc::symlink(target.as_ptr(), path.as_ptr()) },
        "Symlink anlegen",
    )
}

pub fn make_link(branch: &BranchRoot, from: &str, to: &str) -> Result<()> {
    let from = c_path(&branch.resolve(from))?;
    let to = c_path(&branch.resolve(to))?;
    check(
        unsafe { libc::link(from.as_ptr(), to.as_ptr()) },
        "Hardlink anlegen",
    )
}

pub fn remove_file(branch: &BranchRoot, relative: &str) -> Result<()> {
    let path = c_path(&branch.resolve(relative))?;
    check(unsafe { libc::unlink(path.as_ptr()) }, "Datei loeschen")
}

pub fn remove_dir(branch: &BranchRoot, relative: &str) -> Result<()> {
    let path = c_path(&branch.resolve(relative))?;
    check(
        unsafe { libc::rmdir(path.as_ptr()) },
        "Verzeichnis loeschen",
    )
}

pub fn move_within(branch: &BranchRoot, from: &str, to: &str) -> Result<()> {
    let from = c_path(&branch.resolve(from))?;
    let to = c_path(&branch.resolve(to))?;
    check(
        unsafe { libc::rename(from.as_ptr(), to.as_ptr()) },
        "umbenennen",
    )
}

pub fn set_mode(branch: &BranchRoot, relative: &str, mode: u32) -> Result<()> {
    let path = c_path(&branch.resolve(relative))?;
    check(
        unsafe { libc::chmod(path.as_ptr(), mode as libc::mode_t) },
        "Modus setzen",
    )
}

/// Setzt Eigentuemer und Gruppe, **ohne** einem Symlink zu folgen.
///
/// `lchown` und nicht `chown`: Sonst aenderte ein `chown` auf einen Symlink
/// den Eigentuemer seines Ziels — und das Ziel kann ausserhalb des Pools
/// liegen.
pub fn set_owner(
    branch: &BranchRoot,
    relative: &str,
    uid: Option<u32>,
    gid: Option<u32>,
) -> Result<()> {
    let path = c_path(&branch.resolve(relative))?;
    // `-1` heisst „unveraendert lassen".
    let uid = uid.unwrap_or(u32::MAX);
    let gid = gid.unwrap_or(u32::MAX);
    check(
        unsafe { libc::lchown(path.as_ptr(), uid, gid) },
        "Eigentuemer setzen",
    )
}

pub fn truncate(branch: &BranchRoot, relative: &str, size: u64) -> Result<()> {
    let path = c_path(&branch.resolve(relative))?;
    check(
        unsafe { libc::truncate(path.as_ptr(), size as libc::off_t) },
        "Groesse setzen",
    )
}

/// „Diesen Zeitstempel nicht anfassen."
const UTIME_OMIT: i64 = (1 << 30) - 2;
/// „Diesen Zeitstempel auf jetzt setzen."
const UTIME_NOW: i64 = (1 << 30) - 1;

/// Eine Zeitangabe fuer [`set_times`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timestamp {
    Keep,
    Now,
    At(i64, u32),
}

impl Timestamp {
    fn to_timespec(self) -> libc::timespec {
        match self {
            Timestamp::Keep => libc::timespec {
                tv_sec: 0,
                tv_nsec: UTIME_OMIT,
            },
            Timestamp::Now => libc::timespec {
                tv_sec: 0,
                tv_nsec: UTIME_NOW,
            },
            Timestamp::At(seconds, nanos) => libc::timespec {
                tv_sec: seconds,
                tv_nsec: i64::from(nanos),
            },
        }
    }
}

pub fn set_times(
    branch: &BranchRoot,
    relative: &str,
    atime: Timestamp,
    mtime: Timestamp,
) -> Result<()> {
    let path = c_path(&branch.resolve(relative))?;
    let times = [atime.to_timespec(), mtime.to_timespec()];
    check(
        unsafe {
            libc::utimensat(
                libc::AT_FDCWD,
                path.as_ptr(),
                times.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        },
        "Zeitstempel setzen",
    )
}

/// Uebertraegt Modus, Eigentuemer und Gruppe von einem Branch auf einen
/// anderen.
///
/// Gebraucht, wenn ein Verzeichnis auf einem zweiten Branch entsteht, weil
/// dort eine neue Datei hingehoert. Ohne das truege dieselbe Verzeichnis auf
/// zwei Platten verschiedene Rechte — und welche gelten, haengt davon ab,
/// welcher Branch die Auskunft gerade bedient.
pub fn clone_metadata(from: &Metadata, to: &BranchRoot, relative: &str) -> Result<()> {
    set_mode(to, relative, from.mode() & 0o7777)?;
    set_owner(to, relative, Some(from.uid()), Some(from.gid()))
}

// --- Erweiterte Attribute -------------------------------------------------
//
// Durchweg die `l`-Varianten: Ein Symlink im Pool traegt eigene Attribute,
// und wer ihm folgte, schriebe sie auf das Ziel — womoeglich ausserhalb des
// Pools.

/// Der Wert eines erweiterten Attributs.
pub fn get_xattr(branch: &BranchRoot, relative: &str, name: &CString) -> Result<Vec<u8>> {
    let path = c_path(&branch.resolve(relative))?;
    // Zwei Aufrufe, und dazwischen kann sich der Wert aendern. Deshalb die
    // Schleife: Wurde er zwischen Messen und Holen laenger, sagt der zweite
    // Aufruf `ERANGE`, und wir messen neu, statt einen halben Wert zu
    // liefern.
    loop {
        let needed =
            unsafe { libc::lgetxattr(path.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
        if needed < 0 {
            return Err(last_error("Attribut messen"));
        }
        let mut value = vec![0u8; needed as usize];
        let read = unsafe {
            libc::lgetxattr(
                path.as_ptr(),
                name.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
            )
        };
        if read >= 0 {
            value.truncate(read as usize);
            return Ok(value);
        }
        if errno() != libc::ERANGE {
            return Err(last_error("Attribut lesen"));
        }
    }
}

/// Die Namen aller erweiterten Attribute, mit Nullbyte getrennt.
pub fn list_xattr(branch: &BranchRoot, relative: &str) -> Result<Vec<u8>> {
    let path = c_path(&branch.resolve(relative))?;
    loop {
        let needed = unsafe { libc::llistxattr(path.as_ptr(), std::ptr::null_mut(), 0) };
        if needed < 0 {
            return Err(last_error("Attributliste messen"));
        }
        let mut names = vec![0u8; needed as usize];
        let read =
            unsafe { libc::llistxattr(path.as_ptr(), names.as_mut_ptr().cast(), names.len()) };
        if read >= 0 {
            names.truncate(read as usize);
            return Ok(names);
        }
        if errno() != libc::ERANGE {
            return Err(last_error("Attributliste lesen"));
        }
    }
}

/// Setzt ein erweitertes Attribut.
pub fn set_xattr(
    branch: &BranchRoot,
    relative: &str,
    name: &CString,
    value: &[u8],
    flags: i32,
) -> Result<()> {
    let path = c_path(&branch.resolve(relative))?;
    check(
        unsafe {
            libc::lsetxattr(
                path.as_ptr(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                flags,
            )
        },
        "Attribut setzen",
    )
}

/// Entfernt ein erweitertes Attribut.
pub fn remove_xattr(branch: &BranchRoot, relative: &str, name: &CString) -> Result<()> {
    let path = c_path(&branch.resolve(relative))?;
    check(
        unsafe { libc::lremovexattr(path.as_ptr(), name.as_ptr()) },
        "Attribut entfernen",
    )
}

/// Der Name der Default-ACL eines Verzeichnisses.
pub const POSIX_ACL_DEFAULT: &str = "system.posix_acl_default";

/// Traegt dieses Verzeichnis eine Default-ACL?
///
/// Entscheidet, ob die `umask` beim Anlegen gilt: Hat das Elternverzeichnis
/// eine Default-ACL, vergibt **sie** die Rechte, und die `umask` zieht nichts
/// mehr ab. Genau so steht es in POSIX.1e, und genau so macht es jedes
/// Dateisystem unter uns auch — nur muss es hier entschieden werden, weil der
/// Kernel die `umask` mit `FUSE_POSIX_ACL` nicht mehr selbst anwendet.
pub fn has_default_acl(branch: &BranchRoot, relative: &str) -> bool {
    let Ok(path) = c_path(&branch.resolve(relative)) else {
        return false;
    };
    let Ok(name) = CString::new(POSIX_ACL_DEFAULT) else {
        return false;
    };
    let size = unsafe { libc::lgetxattr(path.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
    size >= 0
}

/// Uebertraegt alle erweiterten Attribute von einem Branch auf einen anderen.
///
/// Gebraucht, wenn ein Verzeichnis auf einem zweiten Branch entsteht. Ohne
/// das truegen die beiden Kopien verschiedene Attribute — und bei einer
/// Default-ACL heisst das, dass Dateien je nach Branch mit verschiedenen
/// Rechten entstehen.
///
/// # Warum ein Fehlschlag hier keiner ist
///
/// Nicht jedes Dateisystem nimmt jedes Attribut an, und `trusted.*` braucht
/// `CAP_SYS_ADMIN`. Ein Verzeichnis deswegen gar nicht erst anzulegen waere
/// schlimmer als eines ohne alle Attribute — die Datei, um die es eigentlich
/// ging, koennte dann nirgends hin. Was nicht ging, steht in der
/// Rueckgabe: die Zahl der uebertragenen Attribute und die der gescheiterten.
pub fn copy_xattrs(from: &BranchRoot, to: &BranchRoot, relative: &str) -> (usize, usize) {
    let Ok(names) = list_xattr(from, relative) else {
        return (0, 0);
    };
    let (mut copied, mut failed) = (0, 0);
    for name in names.split(|byte| *byte == 0) {
        if name.is_empty() {
            continue;
        }
        let Ok(name) = CString::new(name) else {
            continue;
        };
        let Ok(value) = get_xattr(from, relative, &name) else {
            failed += 1;
            continue;
        };
        match set_xattr(to, relative, &name, &value, 0) {
            Ok(()) => copied += 1,
            Err(_) => failed += 1,
        }
    }
    (copied, failed)
}

/// Was `statvfs` ueber einen Branch sagt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Space {
    pub total: u64,
    pub free: u64,
    /// Das Dateisystem ist nur lesend eingehaengt.
    ///
    /// Ein solcher Branch nimmt nichts Neues auf. Wer das erst beim Schreiben
    /// merkt, hat die Datei schon platziert und muss zuruecknehmen, was auf
    /// halbem Weg entstanden ist.
    pub read_only: bool,
}

/// Der freie und der gesamte Platz eines Branches, in Bytes.
///
/// `statvfs` ist der einzige Aufruf hier, fuer den `std` nichts anbietet.
pub fn space(branch: &BranchRoot) -> Result<Space> {
    let path = c_path(&branch.root)?;
    let mut raw: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut raw) } != 0 {
        let error = std::io::Error::last_os_error();
        return Err(PoolError::Io {
            what: "freien Platz ermitteln",
            kind: error.kind(),
            raw_os_error: error.raw_os_error(),
        });
    }

    // `f_frsize` ist die Groesse eines Blocks in `f_blocks`, `f_bsize` nur die
    // bevorzugte Blockgroesse fuer I/O. Wer die beiden verwechselt, rechnet
    // auf manchen Dateisystemen um Groessenordnungen daneben.
    let unit = if raw.f_frsize > 0 {
        raw.f_frsize as u64
    } else {
        raw.f_bsize as u64
    };
    Ok(Space {
        total: (raw.f_blocks as u64).saturating_mul(unit),
        // `f_bavail` und nicht `f_bfree`: Der fuer Root reservierte Teil steht
        // einem Share nicht zur Verfuegung.
        free: (raw.f_bavail as u64).saturating_mul(unit),
        read_only: raw.f_flag & libc::ST_RDONLY != 0,
    })
}

/// Die Auflistungen aller Branches, in der Form, die `merge_listing` erwartet.
pub type Listings = Vec<(BranchId, Vec<(String, EntryKind)>)>;

/// Die Zuordnung von Namen zu Inode-Nummern je Branch, fuer eine Auflistung.
pub type InoMap = HashMap<(BranchId, String), u64>;

/// Liest ein Verzeichnis auf allen Branches und trennt dabei ab, was
/// [`merge_listing`](crate::merge_listing) braucht, von dem, was nur die
/// Anzeige braucht.
pub fn list_all(branches: &[BranchRoot], relative: &str) -> (Listings, InoMap) {
    let mut listings = Vec::with_capacity(branches.len());
    let mut inos = InoMap::new();

    for branch in branches {
        let entries = list(branch, relative);
        let mut names = Vec::with_capacity(entries.len());
        for entry in entries {
            inos.insert((branch.id, entry.name.clone()), entry.ino);
            names.push((entry.name, entry.kind));
        }
        listings.push((branch.id, names));
    }
    (listings, inos)
}

/// Ein Pfad als nullterminierte Zeichenkette fuer die Systemaufrufe, fuer die
/// `std` nichts anbietet.
fn c_path(path: &Path) -> Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes()).map_err(|_| PoolError::InvalidPath {
        reason: "Nullbyte im Pfad",
    })
}

/// Wandelt den Rueckgabewert eines Systemaufrufs in ein `Result`.
fn check(result: libc::c_int, what: &'static str) -> Result<()> {
    if result != 0 {
        let error = std::io::Error::last_os_error();
        return Err(PoolError::Io {
            what,
            kind: error.kind(),
            raw_os_error: error.raw_os_error(),
        });
    }
    Ok(())
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn last_error(what: &'static str) -> PoolError {
    let error = std::io::Error::last_os_error();
    PoolError::Io {
        what,
        kind: error.kind(),
        raw_os_error: error.raw_os_error(),
    }
}

fn io_error(what: &'static str) -> impl FnOnce(std::io::Error) -> PoolError {
    move |error| PoolError::Io {
        what,
        kind: error.kind(),
        raw_os_error: error.raw_os_error(),
    }
}

/// Der `errno`-Wert zu einem Fehler des Betriebssystems, als **negative** Zahl
/// fuer die FUSE-Antwort.
pub fn errno_of(error: &PoolError) -> i32 {
    match error {
        PoolError::Io { raw_os_error, .. } => -raw_os_error.unwrap_or(libc::EIO),
        PoolError::InvalidPath { .. } => -libc::EINVAL,
        PoolError::NoBranch => -libc::ENOSPC,
        PoolError::NoSpace { .. } => -libc::ENOSPC,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_root_itself_resolves_to_the_root() {
        let branch = BranchRoot::new(BranchId(0), "/mnt/data0/Filme");
        assert_eq!(branch.resolve(""), Path::new("/mnt/data0/Filme"));
    }

    #[test]
    fn a_relative_path_hangs_below_the_root() {
        let branch = BranchRoot::new(BranchId(0), "/mnt/data0/Filme");
        assert_eq!(
            branch.resolve("2026/film.mkv"),
            Path::new("/mnt/data0/Filme/2026/film.mkv")
        );
    }

    #[test]
    fn a_missing_directory_lists_as_empty_and_not_as_an_error() {
        // Im Pool ist das der Normalfall: Ein Verzeichnis muss nicht auf
        // jeder Platte liegen.
        let branch = BranchRoot::new(BranchId(0), "/gibt/es/nicht");
        assert!(list(&branch, "").is_empty());
        assert!(look(&branch, "").is_none());
    }

    #[test]
    fn every_error_maps_to_a_negative_errno() {
        assert_eq!(errno_of(&PoolError::NoBranch), -libc::ENOSPC);
        assert_eq!(
            errno_of(&PoolError::NoSpace {
                needed: 0,
                min_free: 0
            }),
            -libc::ENOSPC
        );
        assert_eq!(
            errno_of(&PoolError::InvalidPath { reason: "x" }),
            -libc::EINVAL
        );
        assert_eq!(
            errno_of(&PoolError::Io {
                what: "x",
                kind: std::io::ErrorKind::NotFound,
                raw_os_error: Some(libc::ENOENT)
            }),
            -libc::ENOENT
        );
    }

    #[test]
    fn an_io_error_without_an_errno_becomes_eio() {
        // Kommt bei Fehlern vor, die `std` selbst erzeugt. Ein `0` als errno
        // hiesse fuer den Kernel „alles in Ordnung".
        assert_eq!(
            errno_of(&PoolError::Io {
                what: "x",
                kind: std::io::ErrorKind::Other,
                raw_os_error: None
            }),
            -libc::EIO
        );
    }
}
