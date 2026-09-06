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
use std::ffi::CString;
use std::fs::{File, Metadata};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use crate::branch::{Branch, BranchId};
use crate::error::{PoolError, Result};
use crate::fuse::abi;
use crate::fuse::backing::{self, BranchRoot, Timestamp};
use crate::fuse::connection::{Connection, MAX_WRITE};
use crate::fuse::inode::{InodeTable, ROOT};
use crate::merge::{merge_listing, resolve, BranchEntry, EntryKind};
use crate::path::{ancestor_at, depth_of};
use crate::place::{place, PlacementRequest};
use crate::policy::SharePolicy;

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

/// Die Datei auf der Platte: Geraet und Inode.
///
/// Nicht der Pfad — zwei Namen koennen auf dieselbe Datei zeigen (Hardlink),
/// und der Kernel zaehlt Inodes, keine Namen.
type FileKey = (u64, u64);

/// Eine beim Kernel hinterlegte Datei.
#[derive(Debug)]
struct Handed {
    id: i32,
    /// Wie viele offene Handles sie halten. Bei null wird sie freigegeben.
    holders: u32,
}

/// Ein Eintrag in der Momentaufnahme eines geoeffneten Verzeichnisses.
#[derive(Debug, Clone)]
struct Listed {
    name: String,
    kind: EntryKind,
    ino: u64,
}

/// Was der Server unterwegs gezaehlt hat.
///
/// Von aussen lesbar, weil sich sonst nicht pruefen laesst, ob der Passthrough
/// wirklich greift: Ein Test, der nur den Inhalt vergleicht, saehe dasselbe,
/// ob der Kernel die Datei selbst bedient oder dieser Prozess es tut.
/// `reads` ist dabei die Zahl, auf die es ankommt — bleibt sie null, waehrend
/// der Inhalt stimmt, hat der Kernel gelesen.
#[derive(Debug, Default)]
pub struct Counters {
    /// Wurde der Passthrough beim `INIT` ausgehandelt?
    pub passthrough_available: AtomicBool,
    /// Wurden POSIX-ACLs beim `INIT` ausgehandelt?
    pub posix_acl_available: AtomicBool,
    /// Dateien, die mit hinterlegtem Deskriptor geoeffnet wurden.
    pub passthrough_opens: AtomicU64,
    /// Dateien, bei denen das nicht ging — der gewoehnliche Weg.
    pub plain_opens: AtomicU64,
    /// `READ`-Anfragen, die dieser Prozess bedient hat.
    pub reads: AtomicU64,
    /// `WRITE`-Anfragen, die dieser Prozess bedient hat.
    pub writes: AtomicU64,
    /// Attribute, die beim Spiegeln eines Verzeichnisses nicht mitkamen.
    ///
    /// Nicht jedes Dateisystem nimmt jedes Attribut an. Das Verzeichnis
    /// deswegen gar nicht anzulegen waere schlimmer — dann koennte die Datei,
    /// um die es ging, nirgends hin. Gezaehlt wird es trotzdem, sonst
    /// verschwindet es lautlos.
    pub xattrs_not_mirrored: AtomicU64,
    /// Beim Kernel hinterlegte Dateien.
    ///
    /// Kleiner als [`Counters::passthrough_opens`], sobald dieselbe Datei
    /// mehrfach offen ist — genau das ist der Sinn.
    pub backing_opens: AtomicU64,
    /// Erfolgreich wieder freigegebene `backing_id`s.
    ///
    /// Muss am Ende zu [`Counters::backing_opens`] passen. Weicht es ab, haelt
    /// der Kernel Verweise auf Dateien, die niemand mehr braucht.
    pub backing_closes: AtomicU64,
}

/// Der Pool als FUSE-Server.
#[derive(Debug)]
pub struct PoolFs {
    branches: Vec<BranchRoot>,
    policy: SharePolicy,
    inodes: InodeTable,
    files: HashMap<u64, File>,
    /// Zu jeder hinterlegten Datei ihre `backing_id` und ihre Halter.
    ///
    /// Der Schluessel ist die Datei auf der Platte, nicht das Handle: Der
    /// Kernel laesst **eine** hinterlegte Datei je Inode zu. Zwei Handles auf
    /// dieselbe Datei muessen deshalb dieselbe `backing_id` bekommen — wer
    /// jedem Handle eine eigene gibt, bekommt beim zweiten gleichzeitigen
    /// Oeffnen `EIO`.
    backing: HashMap<FileKey, Handed>,
    /// Zu jedem Dateihandle die hinterlegte Datei, die es haelt.
    handed: HashMap<u64, FileKey>,
    dirs: HashMap<u64, Vec<Listed>>,
    next_fh: u64,
    /// Der zuletzt benutzte Branch, fuer [`Allocation::RoundRobin`].
    cursor: Option<BranchId>,
    /// Hat der Kernel den Passthrough angeboten — und wollen wir ihn?
    passthrough: bool,
    /// Wurde `FUSE_POSIX_ACL` ausgehandelt?
    ///
    /// Danach wendet der Kernel die `umask` beim Anlegen nicht mehr an — das
    /// ist ab dann Sache dieses Servers.
    posix_acl: bool,
    /// Darf ueberhaupt verhandelt werden?
    ///
    /// Nur fuer Tests aus: Ohne das Bit weist der Kernel ACL-Attribute selbst
    /// zurueck, und der Umask-Pfad hier laeuft nie. Beides gehoert geprueft.
    allow_posix_acl: bool,
    /// Darf ueberhaupt verhandelt werden?
    ///
    /// Nur fuer Tests aus: Greift der Passthrough, sieht dieser Prozess kein
    /// `READ` und kein `WRITE` mehr, und die beiden Behandler waeren auf einem
    /// neuen Kernel ungetestet. Ein Schalter ist ehrlicher, als sich auf einen
    /// alten Kernel im Testlauf zu verlassen.
    allow_passthrough: bool,
    counters: Arc<Counters>,
}

impl PoolFs {
    pub fn new(branches: Vec<BranchRoot>, policy: SharePolicy) -> Self {
        PoolFs {
            branches,
            policy,
            inodes: InodeTable::new(),
            files: HashMap::new(),
            backing: HashMap::new(),
            handed: HashMap::new(),
            dirs: HashMap::new(),
            next_fh: 1,
            cursor: None,
            passthrough: false,
            allow_passthrough: true,
            posix_acl: false,
            allow_posix_acl: true,
            counters: Arc::new(Counters::default()),
        }
    }

    /// Schaltet den Passthrough ab, bevor eingehaengt wird.
    pub fn without_passthrough(mut self) -> Self {
        self.allow_passthrough = false;
        self
    }

    /// Schaltet die POSIX-ACLs ab, bevor eingehaengt wird.
    pub fn without_posix_acl(mut self) -> Self {
        self.allow_posix_acl = false;
        self
    }

    /// Die Zaehler — abzuholen, **bevor** der Server in einen Thread wandert.
    pub fn counters(&self) -> Arc<Counters> {
        Arc::clone(&self.counters)
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

            if let Some(answer) = self.dispatch(connection, &header, data) {
                let (error, payload) = match answer {
                    Ok(payload) => (0, payload),
                    Err(errno) => (errno, Vec::new()),
                };
                connection.reply(header.unique, error, &payload)?;
            }
        }
    }

    /// `None` heisst: Auf diese Anfrage gehoert keine Antwort.
    fn dispatch(
        &mut self,
        connection: &Connection,
        header: &abi::InHeader,
        data: &[u8],
    ) -> Option<Answer> {
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

            abi::FUSE_OPEN => Some(self.open(connection, header.nodeid, data)),
            abi::FUSE_READ => Some(self.read(data)),
            abi::FUSE_RELEASE => Some(self.release(connection, data)),
            abi::FUSE_FLUSH | abi::FUSE_FSYNCDIR => Some(Ok(Vec::new())),
            abi::FUSE_FSYNC => Some(self.fsync(data)),

            // --- Schreiben ---
            abi::FUSE_CREATE => Some(self.create(connection, header, data)),
            abi::FUSE_MKDIR => Some(self.mkdir(header, data)),
            abi::FUSE_MKNOD => Some(self.mknod(header, data)),
            abi::FUSE_SYMLINK => Some(self.symlink(header, data)),
            abi::FUSE_LINK => Some(self.link(header, data)),
            abi::FUSE_WRITE => Some(self.write(data)),
            abi::FUSE_UNLINK => Some(self.unlink(header.nodeid, data)),
            abi::FUSE_RMDIR => Some(self.rmdir(header.nodeid, data)),
            abi::FUSE_RENAME => Some(self.rename(header.nodeid, data, false)),
            abi::FUSE_RENAME2 => Some(self.rename(header.nodeid, data, true)),
            abi::FUSE_SETATTR => Some(self.setattr(header.nodeid, data)),

            // --- Erweiterte Attribute ---
            abi::FUSE_GETXATTR => Some(self.getxattr(header.nodeid, data)),
            abi::FUSE_LISTXATTR => Some(self.listxattr(header.nodeid, data)),
            abi::FUSE_SETXATTR => Some(self.setxattr(header.nodeid, data)),
            abi::FUSE_REMOVEXATTR => Some(self.removexattr(header.nodeid, data)),

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
        let mut flags = request.flags & (abi::FUSE_ASYNC_READ | abi::FUSE_BIG_WRITES);
        // Beide zusammen oder keines: ACLs anzumelden, ohne die `umask` zu
        // uebernehmen, hiesse, dass der Kernel sie weiter abzieht und jede
        // Default-ACL gegen sie verliert.
        if self.allow_posix_acl {
            flags |= request.flags & (abi::FUSE_POSIX_ACL | abi::FUSE_DONT_MASK);
        }
        self.posix_acl = flags & abi::FUSE_POSIX_ACL != 0 && flags & abi::FUSE_DONT_MASK != 0;
        self.counters
            .posix_acl_available
            .store(self.posix_acl, Ordering::Relaxed);
        let flags2 = if self.allow_passthrough {
            request.flags2 & abi::FUSE_PASSTHROUGH_FLAG2
        } else {
            0
        };
        if flags2 != 0 {
            flags |= request.flags & abi::FUSE_INIT_EXT;
        }

        // Der Passthrough braucht **dreierlei**: das Bit vom Kernel, das
        // zurueckgegebene `FUSE_INIT_EXT`, ohne das er `flags2` gar nicht
        // ansieht, und eine ausgehandelte Version, die `max_stack_depth`
        // kennt. Fehlt eines, laeuft alles wie bisher durch diesen Prozess —
        // langsamer, aber richtig.
        self.passthrough = flags2 & abi::FUSE_PASSTHROUGH_FLAG2 != 0
            && flags & abi::FUSE_INIT_EXT != 0
            && minor >= abi::FUSE_KERNEL_MINOR_VERSION;
        self.counters
            .passthrough_available
            .store(self.passthrough, Ordering::Relaxed);

        Ok(abi::init_out(minor, request.max_readahead, flags, flags2, MAX_WRITE).into_bytes())
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
        Ok(abi::open_out(fh, 0, 0).into_bytes())
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

    fn open(&mut self, connection: &Connection, nodeid: u64, data: &[u8]) -> Answer {
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
        let (open_flags, backing_id) = self.hand_over(connection, &file, fh);
        self.files.insert(fh, file);
        Ok(abi::open_out(fh, open_flags, backing_id).into_bytes())
    }

    fn read(&mut self, data: &[u8]) -> Answer {
        self.counters.reads.fetch_add(1, Ordering::Relaxed);
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

    fn release(&mut self, connection: &Connection, data: &[u8]) -> Answer {
        if data.len() >= 8 {
            let mut raw = [0u8; 8];
            raw.copy_from_slice(&data[..8]);
            let fh = u64::from_ne_bytes(raw);
            self.files.remove(&fh);
            // **Zu jedem `backing_open` gehoert ein `backing_close`.** Sonst
            // haelt der Kernel einen Verweis auf die Datei, bis der Pool
            // ausgehaengt wird — bei einem Dienst, der Monate laeuft, ist das
            // ein Leck, das erst beim Aufraeumen auffaellt.
            self.take_back(connection, fh);
        }
        Ok(Vec::new())
    }

    // --- Anlegen ----------------------------------------------------------

    fn create(&mut self, connection: &Connection, header: &abi::InHeader, data: &[u8]) -> Answer {
        let Some(request) = abi::CreateIn::decode(data) else {
            return Err(-libc::EINVAL);
        };
        let name = name_of(data.get(abi::CreateIn::SIZE..).ok_or(-libc::EINVAL)?)?;
        let path = self.child_path(header.nodeid, name)?;
        let flags = request.flags as i32;

        // Da? Dann ist das kein Anlegen, sondern ein Oeffnen — es sei denn,
        // der Aufrufer bestand auf `O_EXCL`.
        if let Some(branch) = self.serving_branch(&path) {
            if flags & libc::O_EXCL != 0 {
                return Err(-libc::EEXIST);
            }
            let root = self.branch(branch).ok_or(-libc::ENOENT)?.clone();
            let file = backing::open(&root, &path, flags).map_err(|e| backing::errno_of(&e))?;
            return self.opened(connection, &path, file);
        }

        let branch = self.place_at(&path, 0)?;
        self.ensure_parents(branch, &path)?;
        let root = self.branch(branch).ok_or(-libc::ENOENT)?.clone();
        let mode = self.creation_mode(&root, header.nodeid, request.mode, request.umask);
        let file =
            backing::create_file(&root, &path, mode, flags).map_err(|e| backing::errno_of(&e))?;
        self.give_to_caller(&root, &path, header)?;
        self.opened(connection, &path, file)
    }

    fn mkdir(&mut self, header: &abi::InHeader, data: &[u8]) -> Answer {
        let Some(request) = abi::MkdirIn::decode(data) else {
            return Err(-libc::EINVAL);
        };
        let name = name_of(data.get(abi::MkdirIn::SIZE..).ok_or(-libc::EINVAL)?)?;
        let path = self.child_path(header.nodeid, name)?;
        if !self.look_all(&path).is_empty() {
            return Err(-libc::EEXIST);
        }

        // Auf **einen** Branch, nicht auf alle. Ein Verzeichnis ueberall
        // anzulegen machte die Split-Regel bedeutungslos: Jeder Vorfahre
        // laege dann auf jeder Platte, und nichts bliebe mehr zusammen.
        let branch = self.place_at(&path, 0)?;
        self.ensure_parents(branch, &path)?;
        let root = self.branch(branch).ok_or(-libc::ENOENT)?.clone();
        let mode = self.creation_mode(&root, header.nodeid, request.mode, request.umask);
        backing::make_dir(&root, &path, mode).map_err(|e| backing::errno_of(&e))?;
        self.give_to_caller(&root, &path, header)?;
        self.entry_reply(&path)
    }

    fn mknod(&mut self, header: &abi::InHeader, data: &[u8]) -> Answer {
        let Some(request) = abi::MknodIn::decode(data) else {
            return Err(-libc::EINVAL);
        };
        let name = name_of(data.get(abi::MknodIn::SIZE..).ok_or(-libc::EINVAL)?)?;

        // Geraeteknoten nicht. Der Pool haengt mit `MS_NODEV`, dort waeren sie
        // wirkungslos — auf der Platte laegen sie aber weiter, und wer diese
        // Platte spaeter direkt einhaengt, faende ein Geraet, das er nie
        // angelegt hat.
        let kind = request.mode & libc::S_IFMT;
        if kind == libc::S_IFBLK || kind == libc::S_IFCHR {
            return Err(-libc::EPERM);
        }

        let path = self.child_path(header.nodeid, name)?;
        if !self.look_all(&path).is_empty() {
            return Err(-libc::EEXIST);
        }
        let branch = self.place_at(&path, 0)?;
        self.ensure_parents(branch, &path)?;
        let root = self.branch(branch).ok_or(-libc::ENOENT)?.clone();
        let mode = self.creation_mode(&root, header.nodeid, request.mode, request.umask);
        backing::make_node(&root, &path, mode).map_err(|e| backing::errno_of(&e))?;
        self.give_to_caller(&root, &path, header)?;
        self.entry_reply(&path)
    }

    fn symlink(&mut self, header: &abi::InHeader, data: &[u8]) -> Answer {
        let Some((name, target)) = abi::two_names(data) else {
            return Err(-libc::EINVAL);
        };
        if target.is_empty() {
            return Err(-libc::EINVAL);
        }
        let name = std::str::from_utf8(name).map_err(|_| -libc::EINVAL)?;
        let path = self.child_path(header.nodeid, name)?;
        if !self.look_all(&path).is_empty() {
            return Err(-libc::EEXIST);
        }

        let branch = self.place_at(&path, 0)?;
        self.ensure_parents(branch, &path)?;
        let root = self.branch(branch).ok_or(-libc::ENOENT)?.clone();
        // Das Ziel wird **nicht** geprueft: Ein Symlink darf ins Leere zeigen,
        // und ein Ziel ausserhalb des Pools loest der Kernel im Kontext des
        // Aufrufers auf, nicht in unserem.
        backing::make_symlink(&root, &path, target).map_err(|e| backing::errno_of(&e))?;
        self.give_to_caller(&root, &path, header)?;
        self.entry_reply(&path)
    }

    fn link(&mut self, new_parent: &abi::InHeader, data: &[u8]) -> Answer {
        let Some(old) = abi::link_oldnodeid(data) else {
            return Err(-libc::EINVAL);
        };
        let name = name_of(data.get(8..).ok_or(-libc::EINVAL)?)?;
        let from = self.path_of(old)?;
        let to = self.child_path(new_parent.nodeid, name)?;

        // Ein Hardlink kann das Dateisystem nicht verlassen, und jeder Branch
        // ist ein eigenes. Er muss also dorthin, wo das Original liegt — die
        // Platzierungsregel hat hier nichts zu entscheiden.
        let Some(branch) = self.serving_branch(&from) else {
            return Err(-libc::ENOENT);
        };
        if !self.look_all(&to).is_empty() {
            return Err(-libc::EEXIST);
        }
        self.ensure_parents(branch, &to)?;
        let root = self.branch(branch).ok_or(-libc::ENOENT)?.clone();
        backing::make_link(&root, &from, &to).map_err(|e| backing::errno_of(&e))?;
        self.entry_reply(&to)
    }

    // --- Schreiben --------------------------------------------------------

    fn write(&mut self, data: &[u8]) -> Answer {
        self.counters.writes.fetch_add(1, Ordering::Relaxed);
        let Some(request) = abi::WriteIn::decode(data) else {
            return Err(-libc::EINVAL);
        };
        let payload = data
            .get(abi::WriteIn::SIZE..)
            .ok_or(-libc::EINVAL)?
            .get(..request.size as usize)
            .ok_or(-libc::EINVAL)?;
        let Some(file) = self.files.get(&request.fh) else {
            return Err(-libc::EBADF);
        };

        let mut written = 0usize;
        while written < payload.len() {
            match file.write_at(&payload[written..], request.offset + written as u64) {
                // Kein Fortschritt und kein Fehler: Die Platte nimmt nichts
                // mehr. Als `ENOSPC` melden statt endlos zu drehen.
                Ok(0) => return Err(-libc::ENOSPC),
                Ok(count) => written += count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(-error.raw_os_error().unwrap_or(libc::EIO)),
            }
        }
        Ok(abi::write_out(written as u32).into_bytes())
    }

    fn fsync(&mut self, data: &[u8]) -> Answer {
        if data.len() < 12 {
            return Err(-libc::EINVAL);
        }
        let mut raw = [0u8; 8];
        raw.copy_from_slice(&data[..8]);
        let fh = u64::from_ne_bytes(raw);
        let mut flags = [0u8; 4];
        flags.copy_from_slice(&data[8..12]);
        let datasync = u32::from_ne_bytes(flags) & 1 != 0;

        let Some(file) = self.files.get(&fh) else {
            return Err(-libc::EBADF);
        };
        // Bei `datasync` reichen die Nutzdaten; sonst muessen auch die
        // Metadaten stehen. Beides an das darunterliegende Dateisystem
        // weiterzugeben ist der ganze Sinn eines `fsync` — hier `Ok` zu
        // melden, ohne etwas zu tun, waere eine Zusage ohne Deckung.
        let result = if datasync {
            file.sync_data()
        } else {
            file.sync_all()
        };
        result.map_err(|error| -error.raw_os_error().unwrap_or(libc::EIO))?;
        Ok(Vec::new())
    }

    // --- Loeschen ---------------------------------------------------------

    fn unlink(&mut self, parent: u64, data: &[u8]) -> Answer {
        let name = name_of(data)?;
        let path = self.child_path(parent, name)?;

        // Von **jedem** Branch, der ihn traegt. Wer nur den bedienenden
        // loescht, laesst die zweite Datei wieder auftauchen — genau das
        // Verhalten, das der Pool an anderer Stelle als Konflikt meldet.
        let carriers = self.carriers(&path);
        if carriers.is_empty() {
            return Err(-libc::ENOENT);
        }
        let mut first_error = None;
        for branch in carriers {
            let Some(root) = self.branch(branch).cloned() else {
                continue;
            };
            if let Err(error) = backing::remove_file(&root, &path) {
                first_error.get_or_insert(backing::errno_of(&error));
            }
        }
        match first_error {
            Some(errno) => Err(errno),
            None => Ok(Vec::new()),
        }
    }

    fn rmdir(&mut self, parent: u64, data: &[u8]) -> Answer {
        let name = name_of(data)?;
        let path = self.child_path(parent, name)?;
        let carriers = self.carriers(&path);
        if carriers.is_empty() {
            return Err(-libc::ENOENT);
        }

        // Erst alle pruefen, dann alle loeschen. Andersherum stuende nach
        // einem `ENOTEMPTY` die Haelfte des Verzeichnisses nicht mehr da.
        for branch in &carriers {
            let Some(root) = self.branch(*branch) else {
                continue;
            };
            if !backing::list(root, &path).is_empty() {
                return Err(-libc::ENOTEMPTY);
            }
        }

        let mut first_error = None;
        for branch in carriers {
            let Some(root) = self.branch(branch).cloned() else {
                continue;
            };
            if let Err(error) = backing::remove_dir(&root, &path) {
                first_error.get_or_insert(backing::errno_of(&error));
            }
        }
        match first_error {
            Some(errno) => Err(errno),
            None => Ok(Vec::new()),
        }
    }

    // --- Umbenennen -------------------------------------------------------

    /// # Warum das nicht atomar ist
    ///
    /// `rename(2)` auf einem Dateisystem ist atomar. Ueber mehrere Platten
    /// hinweg gibt es das nicht: Traegt der Name mehrere Branches, sind es
    /// mehrere Aufrufe, und dazwischen kann der Strom ausfallen.
    ///
    /// Umbenannt wird deshalb **zuerst**, und erst danach wird geraeumt, was
    /// am Ziel im Weg lag. Bricht es beim Umbenennen ab, ist das Ziel
    /// unberuehrt — so wie es `rename` zusagt. Bricht es beim Raeumen ab,
    /// liegt der Name doppelt, und das meldet der Pool als Konflikt statt es
    /// zu verstecken. Von beiden Fehlerbildern ist das zweite das bessere.
    fn rename(&mut self, parent: u64, data: &[u8], with_flags: bool) -> Answer {
        let Some(request) = abi::RenameIn::decode(data, with_flags) else {
            return Err(-libc::EINVAL);
        };
        let Some((old, new)) = abi::two_names(data.get(request.names_at..).ok_or(-libc::EINVAL)?)
        else {
            return Err(-libc::EINVAL);
        };
        let old = std::str::from_utf8(old).map_err(|_| -libc::EINVAL)?;
        let new = std::str::from_utf8(new).map_err(|_| -libc::EINVAL)?;

        let from = self.child_path(parent, old)?;
        let to = self.child_path(request.newdir, new)?;

        const RENAME_NOREPLACE: u32 = 1;
        const RENAME_EXCHANGE: u32 = 2;
        if request.flags & RENAME_EXCHANGE != 0 {
            // Zwei Namen zu tauschen, die auf verschiedenen Platten liegen,
            // hiesse zwei Dateien zu verschieben — und das ist kein Tausch
            // mehr, sondern ein Kopiervorgang mit einem Fenster dazwischen.
            return Err(-libc::EINVAL);
        }

        let carriers = self.carriers(&from);
        if carriers.is_empty() {
            return Err(-libc::ENOENT);
        }
        let occupied = self.carriers(&to);
        if request.flags & RENAME_NOREPLACE != 0 && !occupied.is_empty() {
            return Err(-libc::EEXIST);
        }

        for branch in &carriers {
            self.ensure_parents(*branch, &to)?;
            let Some(root) = self.branch(*branch).cloned() else {
                continue;
            };
            backing::move_within(&root, &from, &to).map_err(|e| backing::errno_of(&e))?;
        }

        // Was am Ziel lag und nicht mitgezogen ist, verdeckte sonst das
        // Ergebnis — auf einem Branch mit kleinerem Index sogar bevorzugt.
        for branch in occupied {
            if carriers.contains(&branch) {
                continue;
            }
            let Some(root) = self.branch(branch).cloned() else {
                continue;
            };
            let is_dir = backing::look(&root, &to).map(|meta| meta.is_dir()) == Some(true);
            let _ = if is_dir {
                backing::remove_dir(&root, &to)
            } else {
                backing::remove_file(&root, &to)
            };
        }

        self.inodes.rename(&from, &to);
        Ok(Vec::new())
    }

    // --- Attribute setzen -------------------------------------------------

    fn setattr(&mut self, nodeid: u64, data: &[u8]) -> Answer {
        let Some(request) = abi::SetattrIn::decode(data) else {
            return Err(-libc::EINVAL);
        };
        let path = self.path_of(nodeid)?;
        let carriers = self.carriers(&path);
        if carriers.is_empty() {
            return Err(-libc::ENOENT);
        }

        for branch in &carriers {
            let Some(root) = self.branch(*branch).cloned() else {
                continue;
            };
            // Auf **allen** Branches, die den Namen tragen. Ein Verzeichnis
            // liegt oft auf mehreren; haetten die verschiedene Rechte, haenge
            // es vom bedienenden Branch ab, welche gelten.
            if request.has(abi::FATTR_MODE) {
                backing::set_mode(&root, &path, request.mode).map_err(|e| backing::errno_of(&e))?;
            }
            if request.has(abi::FATTR_UID) || request.has(abi::FATTR_GID) {
                backing::set_owner(
                    &root,
                    &path,
                    request.has(abi::FATTR_UID).then_some(request.uid),
                    request.has(abi::FATTR_GID).then_some(request.gid),
                )
                .map_err(|e| backing::errno_of(&e))?;
            }
            if request.has(abi::FATTR_ATIME) || request.has(abi::FATTR_MTIME) {
                backing::set_times(
                    &root,
                    &path,
                    stamp(
                        request.has(abi::FATTR_ATIME),
                        request.has(abi::FATTR_ATIME_NOW),
                        request.atime,
                        request.atimensec,
                    ),
                    stamp(
                        request.has(abi::FATTR_MTIME),
                        request.has(abi::FATTR_MTIME_NOW),
                        request.mtime,
                        request.mtimensec,
                    ),
                )
                .map_err(|e| backing::errno_of(&e))?;
            }
            if request.has(abi::FATTR_SIZE) {
                backing::truncate(&root, &path, request.size).map_err(|e| backing::errno_of(&e))?;
            }
        }

        self.getattr(nodeid)
    }

    // --- Erweiterte Attribute ---------------------------------------------
    //
    // # Die Regel
    //
    // **Gelesen wird vom bedienenden Branch, geschrieben auf jeden, der den
    // Namen traegt.** Fuer eine Datei ist das genau einer. Ein Verzeichnis
    // liegt oft auf mehreren, und truegen die verschiedene Attribute, haenge
    // es vom bedienenden Branch ab, welche gelten — dieselbe Ueberlegung wie
    // bei `chmod` in [`PoolFs::setattr`].
    //
    // Gefiltert wird nichts. Wer `trusted.*` oder `security.*` setzen darf,
    // entscheidet der Kernel im VFS anhand der Rechte des Aufrufers, bevor
    // die Anfrage hier ankommt; eine zweite Pruefung an dieser Stelle waere
    // eine, die irgendwann von der ersten abweicht.

    fn getxattr(&mut self, nodeid: u64, data: &[u8]) -> Answer {
        let Some(request) = abi::GetxattrIn::decode(data) else {
            return Err(-libc::EINVAL);
        };
        let name = xattr_name(data.get(abi::GetxattrIn::SIZE..).ok_or(-libc::EINVAL)?)?;
        let path = self.path_of(nodeid)?;
        let branch = self.serving(&path)?;

        let value = backing::get_xattr(&branch, &path, &name).map_err(|e| backing::errno_of(&e))?;
        answer_sized(request.size, value)
    }

    fn listxattr(&mut self, nodeid: u64, data: &[u8]) -> Answer {
        let Some(request) = abi::GetxattrIn::decode(data) else {
            return Err(-libc::EINVAL);
        };
        let path = self.path_of(nodeid)?;
        let branch = self.serving(&path)?;

        let names = backing::list_xattr(&branch, &path).map_err(|e| backing::errno_of(&e))?;
        answer_sized(request.size, names)
    }

    fn setxattr(&mut self, nodeid: u64, data: &[u8]) -> Answer {
        let Some(request) = abi::SetxattrIn::decode(data) else {
            return Err(-libc::EINVAL);
        };
        let rest = data.get(abi::SetxattrIn::SIZE..).ok_or(-libc::EINVAL)?;
        let name = xattr_name(rest)?;
        // Der Wert steht hinter dem Namen samt dessen Nullbyte.
        let value = rest
            .get(name.as_bytes().len() + 1..)
            .ok_or(-libc::EINVAL)?
            .get(..request.size as usize)
            .ok_or(-libc::EINVAL)?;

        let path = self.path_of(nodeid)?;
        self.on_every_carrier(&path, |root, path| {
            backing::set_xattr(root, path, &name, value, request.flags as i32)
        })
    }

    fn removexattr(&mut self, nodeid: u64, data: &[u8]) -> Answer {
        let name = xattr_name(data)?;
        let path = self.path_of(nodeid)?;
        self.on_every_carrier(&path, |root, path| backing::remove_xattr(root, path, &name))
    }

    /// Fuehrt eine Aenderung auf jedem Branch aus, der den Namen traegt.
    ///
    /// Der **erste** Fehler zaehlt, und die Schleife bricht ab. Weiterzumachen
    /// hiesse, einen Teil der Platten zu aendern und dem Aufrufer trotzdem
    /// einen Fehler zu melden — er wuesste dann nicht, was gilt.
    fn on_every_carrier<F>(&mut self, path: &str, mut action: F) -> Answer
    where
        F: FnMut(&BranchRoot, &str) -> Result<()>,
    {
        let carriers = self.carriers(path);
        if carriers.is_empty() {
            return Err(-libc::ENOENT);
        }
        for branch in &carriers {
            let Some(root) = self.branch(*branch).cloned() else {
                continue;
            };
            action(&root, path).map_err(|e| backing::errno_of(&e))?;
        }
        Ok(Vec::new())
    }

    /// Die Rechte, mit denen ein neues Objekt entsteht.
    ///
    /// Ohne `FUSE_POSIX_ACL` hat der Kernel die `umask` schon auf `mode`
    /// angewandt — dann bleibt sie, wie sie ist. Mit dem Bit liegt es hier,
    /// und dann gilt POSIX.1e: Traegt das Elternverzeichnis eine Default-ACL,
    /// vergibt **sie** die Rechte und die `umask` zieht nichts ab; sonst zieht
    /// sie ab.
    ///
    /// Beides falsch herum ist still. Die Datei entsteht so oder so — nur mit
    /// anderen Rechten als bestellt, und das faellt erst auf, wenn jemand
    /// nicht mehr hineinkommt.
    fn creation_mode(&mut self, root: &BranchRoot, parent: u64, mode: u32, umask: u32) -> u32 {
        if !self.posix_acl {
            return mode;
        }
        let Ok(parent) = self.path_of(parent) else {
            return mode & !umask;
        };
        if backing::has_default_acl(root, &parent) {
            mode
        } else {
            mode & !umask
        }
    }

    /// Der Branch, der diesen Pfad bedient.
    fn serving(&mut self, path: &str) -> std::result::Result<BranchRoot, i32> {
        let branch = self.serving_branch(path).ok_or(-libc::ENOENT)?;
        self.branch(branch).cloned().ok_or(-libc::ENOENT)
    }

    // --- Platzierung ------------------------------------------------------

    /// Waehlt den Branch fuer ein neues Objekt.
    ///
    /// Der freie Platz wird **jetzt** gemessen und nicht gemerkt: Eine
    /// zwischengespeicherte Zahl fuehrte dazu, dass der Pool eine Platte fuer
    /// leer haelt, die gerade vollgelaufen ist — und das ENOSPC kaeme dann
    /// mitten im Schreiben statt vorher.
    fn place_at(&mut self, path: &str, needed: u64) -> std::result::Result<BranchId, i32> {
        let branches = self.branches_now();
        let anchors = self.anchors_for(path);
        let mut request = PlacementRequest::new(path)
            .with_anchors(&anchors)
            .with_needed(needed);
        if let Some(cursor) = self.cursor {
            request = request.with_cursor(cursor);
        }

        let placement = place(&branches, &self.policy, &request).map_err(|error| {
            // `NoBranch` heisst hier: keine Platte ist beschreibbar. Fuer
            // einen Aufrufer ist das `ENOSPC` — er kann nichts ablegen.
            backing::errno_of(&error)
        })?;
        self.cursor = Some(placement.cursor);
        Ok(placement.branch)
    }

    /// Der Zustand der Branches, wie ihn die Platzierung braucht.
    ///
    /// Ein Branch, dessen `statvfs` scheitert, faellt heraus: Eine Platte, die
    /// gerade nicht antwortet, soll nichts Neues bekommen.
    fn branches_now(&self) -> Vec<Branch> {
        self.branches
            .iter()
            .filter_map(|root| {
                let space = backing::space(root).ok()?;
                let mut branch = Branch::new(root.id, space.total, space.free);
                if space.read_only {
                    branch = branch.read_only();
                }
                Some(branch)
            })
            .collect()
    }

    /// Die Branches, die den von der Split-Regel gemeinten Vorfahren tragen.
    fn anchors_for(&self, path: &str) -> Vec<BranchId> {
        let Some(depth) = self.policy.split_depth() else {
            return Vec::new();
        };
        let Some(ancestor) = ancestor_at(path, depth) else {
            return Vec::new();
        };
        self.carriers(ancestor)
    }

    /// Legt die fehlenden Vorfahren auf dem Zielbranch an.
    ///
    /// Modus, Eigentuemer und Gruppe kommen dabei von dem Branch, der das
    /// Verzeichnis bisher bedient. Ohne das truege dasselbe Verzeichnis auf
    /// zwei Platten verschiedene Rechte, und welche gelten, haenge davon ab,
    /// welche Platte die Auskunft gerade gibt — und das aendert sich, sobald
    /// die andere geloescht wird.
    fn ensure_parents(&mut self, branch: BranchId, path: &str) -> std::result::Result<(), i32> {
        let Some(root) = self.branch(branch).cloned() else {
            return Err(-libc::ENOENT);
        };
        let depth = depth_of(path).map_err(|_| -libc::EINVAL)?;

        for level in 1..depth {
            let Some(ancestor) = ancestor_at(path, level) else {
                break;
            };
            if backing::look(&root, ancestor).is_some() {
                continue;
            }
            // Der Vorfahre existiert im Pool — sonst haette der Kernel diesen
            // Aufruf gar nicht schicken koennen —, nur eben nicht hier.
            let Some(source) = self.serving_branch(ancestor) else {
                return Err(-libc::ENOENT);
            };
            let Some(from) = self.branch(source).cloned() else {
                return Err(-libc::ENOENT);
            };
            let Some(metadata) = backing::look(&from, ancestor) else {
                return Err(-libc::ENOENT);
            };
            backing::make_dir(&root, ancestor, metadata.mode() & 0o7777)
                .map_err(|e| backing::errno_of(&e))?;
            backing::clone_metadata(&metadata, &root, ancestor)
                .map_err(|e| backing::errno_of(&e))?;
            // Auch die erweiterten Attribute: Eine Default-ACL, die nur auf
            // dem ersten Branch liegt, hiesse, dass eine Datei je nach
            // Platte mit anderen Rechten entsteht.
            let (_, failed) = backing::copy_xattrs(&from, &root, ancestor);
            self.counters
                .xattrs_not_mirrored
                .fetch_add(failed as u64, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Uebertraegt ein frisch angelegtes Objekt an den Aufrufer.
    ///
    /// Der Server laeuft als Root; ohne das gehoerte jede neue Datei Root,
    /// egal wer sie angelegt hat. Auf einem NAS, hinter dem Samba und NFS mit
    /// eigenen Nutzern stehen, waere das nicht bloss unschoen — der Nutzer
    /// koennte seine eigene Datei danach nicht mehr aendern.
    fn give_to_caller(
        &self,
        root: &BranchRoot,
        path: &str,
        header: &abi::InHeader,
    ) -> std::result::Result<(), i32> {
        backing::set_owner(root, path, Some(header.uid), Some(header.gid))
            .map_err(|e| backing::errno_of(&e))
    }

    /// Die Antwort auf eine Anfrage, die ein Objekt angelegt hat.
    fn entry_reply(&mut self, path: &str) -> Answer {
        let found = self.look_all(path);
        let resolution = resolve(&entries_of(&found));
        let Some(branch) = resolution.served_by() else {
            return Err(-libc::ENOENT);
        };
        let metadata = found
            .iter()
            .find(|(id, _)| *id == branch)
            .map(|(_, metadata)| metadata)
            .ok_or(-libc::ENOENT)?;

        let nodeid = self.inodes.lookup(path);
        let attr = backing::attr_of(metadata, nodeid);
        Ok(abi::entry_out(nodeid, &attr, CACHE_SECONDS).into_bytes())
    }

    /// Die Antwort auf ein `CREATE`: Eintrag und offene Datei in einem.
    fn opened(&mut self, connection: &Connection, path: &str, file: File) -> Answer {
        let metadata = file
            .metadata()
            .map_err(|error| -error.raw_os_error().unwrap_or(libc::EIO))?;
        let nodeid = self.inodes.lookup(path);
        let attr = backing::attr_of(&metadata, nodeid);
        let fh = self.take_fh();
        let (open_flags, backing_id) = self.hand_over(connection, &file, fh);
        self.files.insert(fh, file);
        Ok(abi::create_out(nodeid, &attr, CACHE_SECONDS, fh, open_flags, backing_id).into_bytes())
    }

    /// Uebergibt die Datei an den Kernel, wenn er sie annimmt.
    ///
    /// Danach bedient er Lesen und Schreiben unmittelbar aus ihr, und dieser
    /// Prozess sieht sie nicht mehr. Nimmt er sie nicht — zu alter Kernel, zu
    /// viele hinterlegte Dateien —, laeuft alles wie bisher: langsamer, aber
    /// richtig. Deshalb ist der Rueckfall kein Fehlerpfad, sondern der
    /// zweite gewoehnliche Ausgang.
    fn hand_over(&mut self, connection: &Connection, file: &File, fh: u64) -> (u32, i32) {
        use std::os::fd::AsRawFd;

        if !self.passthrough {
            self.counters.plain_opens.fetch_add(1, Ordering::Relaxed);
            return (0, 0);
        }
        // Ohne Geraet und Inode liesse sich nicht sagen, ob diese Datei schon
        // hinterlegt ist — und eine zweite `backing_id` auf denselben Inode
        // lehnt der Kernel ab.
        let Ok(metadata) = file.metadata() else {
            self.counters.plain_opens.fetch_add(1, Ordering::Relaxed);
            return (0, 0);
        };
        let key: FileKey = (metadata.dev(), metadata.ino());

        if let Some(handed) = self.backing.get_mut(&key) {
            handed.holders += 1;
            let id = handed.id;
            self.handed.insert(fh, key);
            self.counters
                .passthrough_opens
                .fetch_add(1, Ordering::Relaxed);
            return (abi::FOPEN_PASSTHROUGH, id);
        }

        match crate::fuse::connection::backing_open(connection, file.as_raw_fd()) {
            Some(id) => {
                self.backing.insert(key, Handed { id, holders: 1 });
                self.handed.insert(fh, key);
                self.counters.backing_opens.fetch_add(1, Ordering::Relaxed);
                self.counters
                    .passthrough_opens
                    .fetch_add(1, Ordering::Relaxed);
                (abi::FOPEN_PASSTHROUGH, id)
            }
            None => {
                self.counters.plain_opens.fetch_add(1, Ordering::Relaxed);
                (0, 0)
            }
        }
    }

    /// Gibt die hinterlegte Datei frei, sobald das letzte Handle sie loslaesst.
    ///
    /// **Zu jedem `backing_open` gehoert ein `backing_close`.** Sonst haelt der
    /// Kernel einen Verweis auf die Datei, bis der Pool ausgehaengt wird — bei
    /// einem Dienst, der Monate laeuft, ist das ein Leck, das erst beim
    /// Aufraeumen auffaellt. Zu frueh schliessen ist aber genauso falsch: Dann
    /// laege die `backing_id` eines noch offenen Handles daneben.
    fn take_back(&mut self, connection: &Connection, fh: u64) {
        let Some(key) = self.handed.remove(&fh) else {
            return;
        };
        let Some(handed) = self.backing.get_mut(&key) else {
            return;
        };
        handed.holders -= 1;
        if handed.holders > 0 {
            return;
        }
        let id = handed.id;
        self.backing.remove(&key);
        if crate::fuse::connection::backing_close(connection, id) {
            self.counters.backing_closes.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Welcher Branch diesen Pfad bedient, falls einer.
    fn serving_branch(&self, path: &str) -> Option<BranchId> {
        resolve(&entries_of(&self.look_all(path))).served_by()
    }

    /// Alle Branches, die diesen Pfad tragen.
    fn carriers(&self, path: &str) -> Vec<BranchId> {
        self.branches
            .iter()
            .filter(|branch| backing::look(branch, path).is_some())
            .map(|branch| branch.id)
            .collect()
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
            let Ok(space) = backing::space(branch) else {
                // Eine Platte, die gerade nicht antwortet, macht den Pool
                // nicht kleiner, als er ist — sie fehlt in der Summe. Der
                // Fehler gehoert in die Control plane, nicht in ein `df`.
                continue;
            };
            total = total.saturating_add(space.total / UNIT);
            free = free.saturating_add(space.free / UNIT);
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

/// Uebersetzt die drei Faelle eines Zeitstempels aus einem `SETATTR`.
///
/// Nicht gesetzt heisst **nicht anfassen** und nicht „auf null setzen": Wer
/// beim `chmod` einer Datei nebenbei ihr Aenderungsdatum verliert, hat den
/// Unterschied uebersehen.
fn stamp(present: bool, now: bool, seconds: i64, nanos: u32) -> Timestamp {
    match (present, now) {
        (false, _) => Timestamp::Keep,
        (true, true) => Timestamp::Now,
        (true, false) => Timestamp::At(seconds, nanos),
    }
}

/// Der nullterminierte Name am Anfang der Nutzlast.
/// Der Name eines erweiterten Attributs aus dem Anfragerumpf.
///
/// Anders als bei [`name_of`] wird hier **nicht** auf UTF-8 bestanden: Ein
/// Attributname ist eine Bytefolge und geht unveraendert an den Systemaufruf
/// weiter. Ein leerer Name ist keiner, und ein Nullbyte mittendrin gehoert
/// abgewiesen, statt den Namen still abzuschneiden.
fn xattr_name(data: &[u8]) -> std::result::Result<CString, i32> {
    let end = data
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(-libc::EINVAL)?;
    if end == 0 {
        return Err(-libc::EINVAL);
    }
    CString::new(&data[..end]).map_err(|_| -libc::EINVAL)
}

/// Beantwortet ein `GETXATTR` oder `LISTXATTR` nach dem zweistufigen
/// Protokoll.
///
/// `size == 0` heisst: Der Aufrufer fragt nur, wie gross der Puffer sein
/// muesste. Ist sein Puffer zu klein, ist das `ERANGE` — und ausdruecklich
/// kein abgeschnittener Wert, denn eine halbe ACL waere eine andere ACL.
fn answer_sized(size: u32, value: Vec<u8>) -> Answer {
    if size == 0 {
        return Ok(abi::getxattr_out(value.len() as u32).into_bytes());
    }
    if value.len() > size as usize {
        return Err(-libc::ERANGE);
    }
    Ok(value)
}

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
