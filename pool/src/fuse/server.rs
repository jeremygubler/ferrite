// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Der Pool als Dateisystem: die Schleife und die Operationen.
//!
//! # Was hier entschieden wird und was nicht
//!
//! Nichts. Wohin ein neues Objekt gehoert, sagt [`place`](crate::place); was
//! ein Name bedeutet, der auf mehreren Branches vorkommt, sagt
//! [`resolve`](crate::resolve). Dieses Modul fuehrt die Antworten aus und
//! uebersetzt sie in das FUSE-Protokoll. Wer hier eine zweite Regel
//! einbaute, haette zwei — und die zweite waere die ungeprueft.
//!
//! # Ein Thread
//!
//! Die Schleife bedient eine Anfrage nach der anderen. Das ist fuer den
//! Metadatenpfad reichlich und fuer den Datenpfad zu wenig; der Datenpfad
//! gehoert aber ohnehin nicht hierher, sondern in den Passthrough, bei dem der
//! Kernel Lesen und Schreiben direkt an die Datei auf dem Branch weiterreicht
//! und dieser Server sie gar nicht mehr sieht. Mehr Threads waeren also Arbeit
//! an der Stelle, die verschwinden soll.
//!
//! # Die Grenze bei Dateinamen
//!
//! Pfade sind hier `str`, nicht Bytes. Unter Linux darf ein Dateiname jede
//! Bytefolge ausser `/` und `\0` sein, also auch eine, die kein UTF-8 ist.
//! Solche Namen weist dieser Server mit `EINVAL` zurueck und laesst sie beim
//! Auflisten aus. Das ist eine Luecke und kein Versehen: Sie **still** falsch
//! zu behandeln — etwa mit einer verlustbehafteten Umwandlung, die dann auf
//! einen Pfad zeigt, den es nicht gibt — waere schlimmer. Sie zu schliessen
//! heisst, die Pfade im ganzen Crate auf Bytes umzustellen, und das ist eine
//! eigene Aenderung.

use std::collections::HashMap;
use std::fs::{File, Metadata};
use std::os::unix::fs::FileExt;

use crate::branch::BranchId;
use crate::error::{PoolError, Result};
use crate::fuse::abi;
use crate::fuse::backing::{self, BranchRoot};
use crate::fuse::connection::{Connection, MAX_WRITE};
use crate::fuse::inode::{InodeTable, ROOT};
use crate::merge::{merge_listing, resolve, BranchEntry, EntryKind};
use crate::path::depth_of;

/// Eine Antwort: Nutzlast oder ein **negativer** `errno`.
type Answer = std::result::Result<Vec<u8>, i32>;

/// Wie lange der Kernel eine Auskunft zwischenspeichern darf, in Sekunden.
///
/// Kurz, und das ist Absicht: Ein Branch kann sich unter dem Pool aendern —
/// durch einen Rebuild, durch eine Reparatur, durch einen Betreiber, der eine
/// Platte direkt einhaengt. Eine lange Gueltigkeit zeigte dann Zahlen von
/// gestern. Null waere ehrlicher und macht jedes `ls` zu einem Sturm von
/// `LOOKUP`s.
const CACHE_SECONDS: u64 = 1;

/// Ein Eintrag in der Momentaufnahme eines geoeffneten Verzeichnisses.
#[derive(Debug, Clone)]
struct Listed {
    name: String,
    kind: EntryKind,
    ino: u64,
}

/// Der Pool als FUSE-Server.
#[derive(Debug)]
pub struct PoolFs {
    branches: Vec<BranchRoot>,
    inodes: InodeTable,
    files: HashMap<u64, File>,
    dirs: HashMap<u64, Vec<Listed>>,
    next_fh: u64,
}

impl PoolFs {
    pub fn new(branches: Vec<BranchRoot>) -> Self {
        PoolFs {
            branches,
            inodes: InodeTable::new(),
            files: HashMap::new(),
            dirs: HashMap::new(),
            next_fh: 1,
        }
    }

    /// Bedient Anfragen, bis der Pool ausgehaengt wird.
    pub fn run(&mut self, connection: &Connection) -> Result<()> {
        let mut buffer = vec![0u8; crate::fuse::connection::READ_BUFFER];
        loop {
            let Some(read) = connection.next_request(&mut buffer)? else {
                return Ok(());
            };
            let Some(header) = abi::InHeader::decode(&buffer[..read]) else {
                // Zu kurz fuer einen Kopf. Es gibt keine `unique`, an die eine
                // Antwort gehen koennte — also weiter.
                continue;
            };
            // `len` aus dem Kopf und nicht `read`: Der Kernel darf mehr
            // schicken, als er ankuendigt, aber nie weniger.
            let end = (header.len as usize).min(read);
            let data = &buffer[abi::IN_HEADER_SIZE..end];

            if let Some(answer) = self.dispatch(&header, data) {
                let (error, payload) = match answer {
                    Ok(payload) => (0, payload),
                    Err(errno) => (errno, Vec::new()),
                };
                connection.reply(header.unique, error, &payload)?;
            }
        }
    }

    /// `None` heisst: Auf diese Anfrage gehoert keine Antwort.
    fn dispatch(&mut self, header: &abi::InHeader, data: &[u8]) -> Option<Answer> {
        match header.opcode {
            abi::FUSE_INIT => Some(self.init(data)),
            abi::FUSE_DESTROY => Some(Ok(Vec::new())),

            // `FORGET` beantwortet man nicht. Wer es doch tut, schiebt dem
            // Kernel eine Antwort auf eine Anfrage unter, auf die er nicht
            // wartet — die naechste echte Antwort landet dann am falschen
            // Platz.
            abi::FUSE_FORGET => {
                if let Some(count) = abi::forget_nlookup(data) {
                    self.inodes.forget(header.nodeid, count);
                }
                None
            }
            abi::FUSE_BATCH_FORGET => {
                for (nodeid, count) in abi::batch_forget(data) {
                    self.inodes.forget(nodeid, count);
                }
                None
            }

            abi::FUSE_LOOKUP => Some(self.lookup(header.nodeid, data)),
            abi::FUSE_GETATTR => Some(self.getattr(header.nodeid)),
            abi::FUSE_READLINK => Some(self.readlink(header.nodeid)),
            abi::FUSE_STATFS => Some(self.statfs()),
            // Die Rechte prueft der Kernel selbst — der Mount laeuft mit
            // `default_permissions`.
            abi::FUSE_ACCESS => Some(Ok(Vec::new())),

            abi::FUSE_OPENDIR => Some(self.opendir(header.nodeid)),
            abi::FUSE_READDIR => Some(self.readdir(data)),
            abi::FUSE_RELEASEDIR => Some(self.releasedir(data)),

            abi::FUSE_OPEN => Some(self.open(header.nodeid, data)),
            abi::FUSE_READ => Some(self.read(data)),
            abi::FUSE_RELEASE => Some(self.release(data)),
            abi::FUSE_FLUSH | abi::FUSE_FSYNC | abi::FUSE_FSYNCDIR => Some(Ok(Vec::new())),

            // Alles andere kann dieser Server noch nicht. `ENOSYS` und nicht
            // `EIO`: Der Kernel merkt sich, dass es diese Operation nicht
            // gibt, und fragt bei manchen nicht wieder.
            _ => Some(Err(-libc::ENOSYS)),
        }
    }

    // --- Verbindungsaufbau ------------------------------------------------

    fn init(&mut self, data: &[u8]) -> Answer {
        let Some(request) = abi::InitIn::decode(data) else {
            return Err(-libc::EINVAL);
        };
        if request.major != abi::FUSE_KERNEL_VERSION {
            // Eine andere Hauptversion ist keine Verhandlungssache. Der Kernel
            // schickt danach seine eigene Version und versucht es erneut.
            return Err(-libc::EPROTO);
        }

        let minor = request.minor.min(abi::FUSE_KERNEL_MINOR_VERSION);
        // Nur anmelden, was der Kernel angeboten hat: Ein Bit zu setzen, das
        // er nicht kennt, ist eine Zusage an niemanden.
        let flags = request.flags & (abi::FUSE_ASYNC_READ | abi::FUSE_BIG_WRITES);
        Ok(abi::init_out(minor, request.max_readahead, flags, MAX_WRITE).into_bytes())
    }

    // --- Nachschlagen -----------------------------------------------------

    fn lookup(&mut self, parent: u64, data: &[u8]) -> Answer {
        let name = name_of(data)?;
        let path = self.child_path(parent, name)?;

        let found = self.look_all(&path);
        let resolution = resolve(&entries_of(&found));
        let Some(branch) = resolution.served_by() else {
            return Err(-libc::ENOENT);
        };
        let metadata = found
            .iter()
            .find(|(id, _)| *id == branch)
            .map(|(_, metadata)| metadata)
            .ok_or(-libc::ENOENT)?;

        let nodeid = self.inodes.lookup(&path);
        let attr = backing::attr_of(metadata, nodeid);
        Ok(abi::entry_out(nodeid, &attr, CACHE_SECONDS).into_bytes())
    }

    fn getattr(&mut self, nodeid: u64) -> Answer {
        let path = self.path_of(nodeid)?;
        let found = self.look_all(&path);
        let resolution = resolve(&entries_of(&found));
        let Some(branch) = resolution.served_by() else {
            return Err(-libc::ENOENT);
        };
        let metadata = found
            .iter()
            .find(|(id, _)| *id == branch)
            .map(|(_, metadata)| metadata)
            .ok_or(-libc::ENOENT)?;

        let attr = backing::attr_of(metadata, nodeid);
        Ok(abi::attr_out(&attr, CACHE_SECONDS).into_bytes())
    }

    fn readlink(&mut self, nodeid: u64) -> Answer {
        let path = self.path_of(nodeid)?;
        let found = self.look_all(&path);
        let resolution = resolve(&entries_of(&found));
        let Some(branch) = resolution.served_by() else {
            return Err(-libc::ENOENT);
        };
        let root = self.branch(branch).ok_or(-libc::ENOENT)?;
        let target = std::fs::read_link(root.resolve(&path))
            .map_err(|error| -error.raw_os_error().unwrap_or(libc::EIO))?;
        // Ohne abschliessendes Nullbyte: Die Laenge steht im Kopf der Antwort.
        Ok(std::os::unix::ffi::OsStrExt::as_bytes(target.as_os_str()).to_vec())
    }

    // --- Verzeichnisse ----------------------------------------------------

    fn opendir(&mut self, nodeid: u64) -> Answer {
        let path = self.path_of(nodeid)?;

        // Eine Momentaufnahme, keine laufende Sicht. Der Kernel holt eine
        // Auflistung in mehreren Stuecken und merkt sich dazwischen einen
        // Offset; kaeme jedes Stueck aus einem frisch gelesenen Verzeichnis,
        // koennte ein gleichzeitiges Anlegen einen Eintrag verschieben und
        // damit einen anderen ueberspringen.
        let (listings, inos) = backing::list_all(&self.branches, &path);
        let merged = merge_listing(&listings);

        let entries: Vec<Listed> = merged
            .into_iter()
            .filter_map(|entry| {
                let branch = entry.resolution.served_by()?;
                let kind = entry.resolution.kind()?;
                let ino = inos
                    .get(&(branch, entry.name.clone()))
                    .copied()
                    .unwrap_or(0);
                Some(Listed {
                    name: entry.name,
                    kind,
                    ino,
                })
            })
            .collect();

        let fh = self.take_fh();
        self.dirs.insert(fh, entries);
        Ok(abi::open_out(fh, 0).into_bytes())
    }

    fn readdir(&mut self, data: &[u8]) -> Answer {
        let Some(request) = abi::ReadIn::decode(data) else {
            return Err(-libc::EINVAL);
        };
        let Some(entries) = self.dirs.get(&request.fh) else {
            return Err(-libc::EBADF);
        };

        let limit = request.size as usize;
        let mut out = abi::Writer::with_capacity(limit.min(64 * 1024));
        let mut offset = request.offset;

        // `.` und `..` gehoeren dazu. Manche Programme verlassen sich darauf,
        // und der Kernel ergaenzt sie bei FUSE nicht.
        loop {
            let entry: (u64, u32, &[u8]) = match offset {
                0 => (ROOT, libc::DT_DIR as u32, b"."),
                1 => (ROOT, libc::DT_DIR as u32, b".."),
                _ => match entries.get(offset as usize - 2) {
                    Some(listed) => (listed.ino, dirent_type(listed.kind), listed.name.as_bytes()),
                    None => break,
                },
            };
            if !abi::push_dirent(&mut out, limit, entry.0, offset + 1, entry.1, entry.2) {
                break;
            }
            offset += 1;
        }
        Ok(out.into_bytes())
    }

    fn releasedir(&mut self, data: &[u8]) -> Answer {
        if data.len() >= 8 {
            let mut raw = [0u8; 8];
            raw.copy_from_slice(&data[..8]);
            self.dirs.remove(&u64::from_ne_bytes(raw));
        }
        Ok(Vec::new())
    }

    // --- Dateien ----------------------------------------------------------

    fn open(&mut self, nodeid: u64, data: &[u8]) -> Answer {
        let Some(request) = abi::OpenIn::decode(data) else {
            return Err(-libc::EINVAL);
        };
        let path = self.path_of(nodeid)?;
        let found = self.look_all(&path);
        let resolution = resolve(&entries_of(&found));
        let Some(branch) = resolution.served_by() else {
            return Err(-libc::ENOENT);
        };
        if resolution.kind() == Some(EntryKind::Directory) {
            return Err(-libc::EISDIR);
        }

        let root = self.branch(branch).ok_or(-libc::ENOENT)?.clone();
        let file = backing::open(&root, &path, request.flags as i32)
            .map_err(|error| backing::errno_of(&error))?;

        let fh = self.take_fh();
        self.files.insert(fh, file);
        Ok(abi::open_out(fh, 0).into_bytes())
    }

    fn read(&mut self, data: &[u8]) -> Answer {
        let Some(request) = abi::ReadIn::decode(data) else {
            return Err(-libc::EINVAL);
        };
        let Some(file) = self.files.get(&request.fh) else {
            return Err(-libc::EBADF);
        };

        let mut buffer = vec![0u8; request.size as usize];
        let mut filled = 0usize;
        while filled < buffer.len() {
            match file.read_at(&mut buffer[filled..], request.offset + filled as u64) {
                // Dateiende. Der Kernel erwartet eine kurze Antwort, keinen
                // Fehler.
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(-error.raw_os_error().unwrap_or(libc::EIO)),
            }
        }
        buffer.truncate(filled);
        Ok(buffer)
    }

    fn release(&mut self, data: &[u8]) -> Answer {
        if data.len() >= 8 {
            let mut raw = [0u8; 8];
            raw.copy_from_slice(&data[..8]);
            self.files.remove(&u64::from_ne_bytes(raw));
        }
        Ok(Vec::new())
    }

    // --- Der ganze Pool ---------------------------------------------------

    fn statfs(&mut self) -> Answer {
        // Auf 4-KiB-Bloecke normiert, statt die Blockgroesse des ersten
        // Branches fuer alle zu nehmen: Die Members duerfen verschieden
        // formatiert sein, und eine Summe ueber verschieden grosse Bloecke
        // waere eine Zahl ohne Bedeutung.
        const UNIT: u64 = 4096;
        let mut total = 0u64;
        let mut free = 0u64;
        for branch in &self.branches {
            let Ok((branch_total, branch_free)) = backing::space(branch) else {
                // Eine Platte, die gerade nicht antwortet, macht den Pool
                // nicht kleiner, als er ist — sie fehlt in der Summe. Der
                // Fehler gehoert in die Control plane, nicht in ein `df`.
                continue;
            };
            total = total.saturating_add(branch_total / UNIT);
            free = free.saturating_add(branch_free / UNIT);
        }
        Ok(abi::statfs_out(total, free, free, 0, 0, UNIT as u32, 255, UNIT as u32).into_bytes())
    }

    // --- Kleinkram --------------------------------------------------------

    fn take_fh(&mut self) -> u64 {
        let fh = self.next_fh;
        self.next_fh += 1;
        fh
    }

    fn branch(&self, id: BranchId) -> Option<&BranchRoot> {
        self.branches.iter().find(|branch| branch.id == id)
    }

    fn path_of(&self, nodeid: u64) -> std::result::Result<String, i32> {
        self.inodes
            .path_of(nodeid)
            .map(|path| path.to_string())
            .ok_or(-libc::ENOENT)
    }

    /// Der Pfad eines Kindes, geprueft.
    fn child_path(&self, parent: u64, name: &str) -> std::result::Result<String, i32> {
        let parent = self.path_of(parent)?;
        let path = if parent.is_empty() {
            name.to_string()
        } else {
            format!("{parent}/{name}")
        };
        // Auch wenn der Kernel `.` und `..` selbst aufloest: Die Sicherheit
        // dieses Servers haengt nicht an einer Eigenschaft seines Aufrufers.
        depth_of(&path).map_err(|_| -libc::EINVAL)?;
        Ok(path)
    }

    /// Was jeder Branch an dieser Stelle traegt.
    fn look_all(&self, path: &str) -> Vec<(BranchId, Metadata)> {
        self.branches
            .iter()
            .filter_map(|branch| backing::look(branch, path).map(|meta| (branch.id, meta)))
            .collect()
    }
}

fn entries_of(found: &[(BranchId, Metadata)]) -> Vec<BranchEntry> {
    found
        .iter()
        .map(|(branch, metadata)| BranchEntry {
            branch: *branch,
            kind: backing::kind_of(metadata),
        })
        .collect()
}

fn dirent_type(kind: EntryKind) -> u32 {
    let raw = match kind {
        EntryKind::File => libc::DT_REG,
        EntryKind::Directory => libc::DT_DIR,
        EntryKind::Symlink => libc::DT_LNK,
        EntryKind::Other => libc::DT_UNKNOWN,
    };
    u32::from(raw)
}

/// Der nullterminierte Name am Anfang der Nutzlast.
fn name_of(data: &[u8]) -> std::result::Result<&str, i32> {
    let end = data
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(data.len());
    let raw = &data[..end];
    if raw.is_empty() {
        return Err(-libc::EINVAL);
    }
    // Siehe den Modulkopf: Ein Name, der kein UTF-8 ist, wird zurueckgewiesen
    // und nicht verlustbehaftet umgewandelt.
    std::str::from_utf8(raw).map_err(|_| -libc::EINVAL)
}

/// Damit `PoolError` auch aus diesem Modul heraus benutzbar bleibt.
impl From<PoolError> for i32 {
    fn from(error: PoolError) -> i32 {
        backing::errno_of(&error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_read_up_to_the_nul_byte() {
        assert_eq!(name_of(b"datei.txt\0").unwrap(), "datei.txt");
        assert_eq!(name_of(b"ohne-nullbyte").unwrap(), "ohne-nullbyte");
    }

    #[test]
    fn an_empty_name_is_refused() {
        assert_eq!(name_of(b"\0"), Err(-libc::EINVAL));
        assert_eq!(name_of(b""), Err(-libc::EINVAL));
    }

    #[test]
    fn a_name_that_is_not_utf8_is_refused_and_not_mangled() {
        // Eine verlustbehaftete Umwandlung ergaebe hier "\u{FFFD}", und der
        // Server suchte danach eine Datei, die es nicht gibt.
        assert_eq!(name_of(b"\xff\xfe\0"), Err(-libc::EINVAL));
    }

    #[test]
    fn a_name_may_carry_trailing_garbage_after_the_nul_byte() {
        // Der Kernel haengt hinter den Namen weitere Argumente. Wer bis zum
        // Ende der Nutzlast liest, nimmt sie in den Dateinamen auf.
        assert_eq!(name_of(b"a.txt\0\x01\x02\x03").unwrap(), "a.txt");
    }

    #[test]
    fn every_entry_kind_has_a_dirent_type() {
        assert_eq!(dirent_type(EntryKind::File), u32::from(libc::DT_REG));
        assert_eq!(dirent_type(EntryKind::Directory), u32::from(libc::DT_DIR));
        assert_eq!(dirent_type(EntryKind::Symlink), u32::from(libc::DT_LNK));
        assert_eq!(dirent_type(EntryKind::Other), u32::from(libc::DT_UNKNOWN));
    }
}
