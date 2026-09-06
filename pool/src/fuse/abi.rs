// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Die FUSE-ABI, 1:1 aus `include/uapi/linux/fuse.h`.
//!
//! # Warum von Hand und nicht mit `transmute`
//!
//! Die Datensaetze kommen als Bytes aus `/dev/fuse`. Ein Zeiger darauf ist
//! nicht zwingend so ausgerichtet, wie es die Struktur verlangt, und ein
//! `transmute` waere damit undefiniertes Verhalten — auf x86 faellt es nicht
//! auf, auf anderen Architekturen schon. Gelesen und geschrieben wird deshalb
//! Feld fuer Feld, genau wie in `format/`.
//!
//! Die Groessen stehen als Konstanten daneben und werden in den Tests gegen
//! Zahlen geprueft, die aus dem Header stammen. Beim ublk-Target hatte ich
//! einmal eine Ausrichtungsluecke uebersehen; der Kernel nahm die zu kurze
//! Struktur an, und es „funktionierte". Dieser Test ist die Lehre daraus.

// --- Opcodes --------------------------------------------------------------

pub const FUSE_LOOKUP: u32 = 1;
pub const FUSE_FORGET: u32 = 2;
pub const FUSE_GETATTR: u32 = 3;
pub const FUSE_SETATTR: u32 = 4;
pub const FUSE_READLINK: u32 = 5;
pub const FUSE_SYMLINK: u32 = 6;
pub const FUSE_MKNOD: u32 = 8;
pub const FUSE_MKDIR: u32 = 9;
pub const FUSE_UNLINK: u32 = 10;
pub const FUSE_RMDIR: u32 = 11;
pub const FUSE_RENAME: u32 = 12;
pub const FUSE_LINK: u32 = 13;
pub const FUSE_OPEN: u32 = 14;
pub const FUSE_READ: u32 = 15;
pub const FUSE_WRITE: u32 = 16;
pub const FUSE_STATFS: u32 = 17;
pub const FUSE_RELEASE: u32 = 18;
pub const FUSE_FSYNC: u32 = 20;
pub const FUSE_SETXATTR: u32 = 21;
pub const FUSE_GETXATTR: u32 = 22;
pub const FUSE_LISTXATTR: u32 = 23;
pub const FUSE_REMOVEXATTR: u32 = 24;
pub const FUSE_FLUSH: u32 = 25;
pub const FUSE_INIT: u32 = 26;
pub const FUSE_OPENDIR: u32 = 27;
pub const FUSE_READDIR: u32 = 28;
pub const FUSE_RELEASEDIR: u32 = 29;
pub const FUSE_FSYNCDIR: u32 = 30;
pub const FUSE_ACCESS: u32 = 34;
pub const FUSE_CREATE: u32 = 35;
pub const FUSE_DESTROY: u32 = 38;
pub const FUSE_BATCH_FORGET: u32 = 42;
pub const FUSE_RENAME2: u32 = 45;

// --- Protokollversion -----------------------------------------------------

pub const FUSE_KERNEL_VERSION: u32 = 7;

/// Die Nebenversion, die dieser Server spricht.
///
/// 7.39, weil der Kernel den Passthrough erst ab dieser Nebenversion
/// anbietet: Er liest `max_stack_depth` aus der Antwort auf `INIT` nur, wenn
/// die ausgehandelte Version hoch genug ist, und ohne dieses Feld gibt es
/// keinen Passthrough.
///
/// Hoeher wird nicht angemeldet. Jede Version bringt Erweiterungen mit, und
/// eine angemeldete Erweiterung ist eine Zusage, die eingeloest werden muss —
/// hier wird nur angemeldet, was auch bedient wird.
pub const FUSE_KERNEL_MINOR_VERSION: u32 = 39;

/// `FUSE_ASYNC_READ`. Ohne dieses Bit serialisiert der Kernel Lesevorgaenge
/// je Datei.
pub const FUSE_ASYNC_READ: u32 = 1 << 0;
/// `FUSE_BIG_WRITES`: Writes duerfen groesser als eine Seite sein.
pub const FUSE_BIG_WRITES: u32 = 1 << 5;
/// `FUSE_INIT_EXT`: Ohne dieses Bit sieht der Kernel `flags2` gar nicht an.
///
/// Er faltet die oberen 32 Bit nur dann zu den Flags dazu, wenn die Antwort
/// `FUSE_INIT_EXT` traegt. Fehlt es, bleibt jedes Bit in `flags2` wirkungslos
/// — und zwar still: Der Mount gelingt, der Passthrough bleibt aus.
pub const FUSE_INIT_EXT: u32 = 1 << 30;
/// `FUSE_POSIX_ACL`: Der Server kann POSIX-ACLs.
///
/// Das Bit tut zweierlei. Der Kernel laesst `system.posix_acl_access` und
/// `-_default` erst dann ueberhaupt durch — ohne es weist er sie selbst mit
/// `EOPNOTSUPP` ab, damit nicht zwei Schichten verschiedene Rechte
/// behaupten. Und er wendet die `umask` beim Anlegen **nicht mehr** an,
/// sondern schickt sie mit: Ein Verzeichnis mit Default-ACL vergibt die
/// Rechte, und dann darf die `umask` nichts mehr abziehen.
pub const FUSE_POSIX_ACL: u32 = 1 << 20;
/// `FUSE_DONT_MASK`: Der Kernel wendet die `umask` nicht mehr selbst an.
///
/// Gehoert zwingend zu [`FUSE_POSIX_ACL`] dazu, auch wenn die Namen es nicht
/// verraten. `FUSE_POSIX_ACL` allein laesst zwar ACLs durch, aber `fuse_mkdir`
/// und `fuse_create_open` ziehen die `umask` weiter selbst ab — und dann
/// verliert jede Default-ACL gegen sie. Gemessen: ohne dieses Bit entstand
/// eine Datei in einem Verzeichnis mit Default-ACL als `0600` statt mit den
/// Rechten, die die ACL vergibt.
pub const FUSE_DONT_MASK: u32 = 1 << 6;
/// `FUSE_DO_READDIRPLUS`: der Kernel darf `READDIRPLUS` schicken.
///
/// Wird hier **nicht** angemeldet. `READDIRPLUS` spart ein `LOOKUP` je
/// Eintrag, verlangt aber, dass jeder Eintrag beim Auflisten voll gestatet
/// wird — im Pool heisst das, jeden Namen auf jedem Branch anzufassen. Ob
/// sich das lohnt, gehoert gemessen und nicht geraten.
pub const FUSE_DO_READDIRPLUS: u32 = 1 << 13;

/// Kleinster Lesepuffer, den der Kernel akzeptiert (`FUSE_MIN_READ_BUFFER`).
pub const FUSE_MIN_READ_BUFFER: usize = 8192;

// --- Passthrough ----------------------------------------------------------

/// `FUSE_PASSTHROUGH`, in `fuse.h` als `1ULL << 37`.
///
/// Die 64 Bit der Flags sind auf zwei Felder verteilt: `flags` traegt die
/// unteren 32, `flags2` die oberen. Bit 37 ist damit Bit 5 in `flags2` — wer
/// das verwechselt, meldet ein Bit an, das etwas anderes bedeutet.
pub const FUSE_PASSTHROUGH_FLAG2: u32 = 1 << 5;

/// `FOPEN_PASSTHROUGH`: Diese Antwort auf `OPEN` traegt eine `backing_id`.
///
/// Danach bedient der Kernel Lesen und Schreiben unmittelbar aus der
/// hinterlegten Datei — dieser Server sieht sie nicht mehr.
pub const FOPEN_PASSTHROUGH: u32 = 1 << 7;

/// Wieviele FUSE-Schichten uebereinander liegen duerfen.
///
/// Ohne diesen Wert kein Passthrough: Der Kernel lehnt eine hinterlegte Datei
/// ab, wenn die Tiefe null ist. Eins genuegt — der Pool liegt auf einem
/// gewoehnlichen Dateisystem und nicht auf einem weiteren FUSE.
pub const MAX_STACK_DEPTH: u32 = 1;

/// `struct fuse_backing_map`: `int32 fd`, `uint32 flags`, `uint64 padding`.
pub const BACKING_MAP_SIZE: usize = 16;

const FUSE_DEV_IOC_MAGIC: u32 = 229;

/// `_IOW(type, nr, size)` aus `asm-generic/ioctl.h`.
///
/// Von Hand ausgerechnet und gegen die Zahlen aus dem Header geprueft — wie
/// bei den ublk-Ioctls. Eine falsche Nummer trifft ein anderes `ioctl`
/// desselben Treibers, und das faellt nicht unbedingt sofort auf.
const fn iow(kind: u32, number: u32, size: u32) -> u32 {
    const WRITE: u32 = 1;
    (WRITE << 30) | (size << 16) | (kind << 8) | number
}

/// Hinterlegt einen Dateideskriptor und liefert eine `backing_id`.
pub const FUSE_DEV_IOC_BACKING_OPEN: u32 = iow(FUSE_DEV_IOC_MAGIC, 1, BACKING_MAP_SIZE as u32);

/// Gibt eine `backing_id` wieder frei.
pub const FUSE_DEV_IOC_BACKING_CLOSE: u32 = iow(FUSE_DEV_IOC_MAGIC, 2, 4);

/// Schreibt `struct fuse_backing_map`.
pub fn backing_map(fd: i32) -> Writer {
    let mut out = Writer::with_capacity(BACKING_MAP_SIZE);
    out.i32(fd)
        .u32(0) // flags
        .u64(0); // padding
    debug_assert_eq!(out.len(), BACKING_MAP_SIZE);
    out
}

// --- Groessen -------------------------------------------------------------

pub const IN_HEADER_SIZE: usize = 40;
pub const OUT_HEADER_SIZE: usize = 16;
pub const ATTR_SIZE: usize = 88;
pub const ENTRY_OUT_SIZE: usize = 128;
pub const ATTR_OUT_SIZE: usize = 104;
pub const OPEN_OUT_SIZE: usize = 16;
pub const KSTATFS_SIZE: usize = 80;
pub const DIRENT_HEADER_SIZE: usize = 24;
pub const WRITE_OUT_SIZE: usize = 8;
pub const GETXATTR_OUT_SIZE: usize = 8;
pub const CREATE_OUT_SIZE: usize = ENTRY_OUT_SIZE + OPEN_OUT_SIZE;

/// Wieviele Bytes der Antwort auf `FUSE_INIT` geschrieben werden.
///
/// # Warum eine Konstante und nicht `size_of`
///
/// Der Kernel lehnt eine Antwort ab, die **laenger** ist als seine eigene
/// `struct fuse_init_out` (`fuse_copy_out_args` gibt dann `-EINVAL`). Kuerzer
/// darf sie sein — fuer `INIT` ist `out_argvar` gesetzt, der Rest bleibt null.
///
/// Diese 40 Bytes reichen bis einschliesslich `max_stack_depth` und damit fuer
/// den Passthrough. `sizeof(struct fuse_init_out)` ist seit 7.28 unveraendert
/// 64 — die Erweiterungen seither haben nur das nachlaufende `unused[]`
/// verkleinert. 40 ist also auf jedem Kernel, der in Frage kommt, kuerzer als
/// dessen Struktur.
///
/// Wer hier ein weiteres Feld braucht, hebt die Zahl **und** die
/// Nebenversion an, nicht nur eines von beidem: Der Kernel liest ein Feld nur,
/// wenn die ausgehandelte Version es kennt.
pub const INIT_OUT_LEN: usize = 40;

// --- Lesen ----------------------------------------------------------------

/// Der Kopf jeder Anfrage: `struct fuse_in_header`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InHeader {
    pub len: u32,
    pub opcode: u32,
    pub unique: u64,
    pub nodeid: u64,
    pub uid: u32,
    pub gid: u32,
    pub pid: u32,
}

impl InHeader {
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < IN_HEADER_SIZE {
            return None;
        }
        Some(InHeader {
            len: u32_at(bytes, 0),
            opcode: u32_at(bytes, 4),
            unique: u64_at(bytes, 8),
            nodeid: u64_at(bytes, 16),
            uid: u32_at(bytes, 24),
            gid: u32_at(bytes, 28),
            pid: u32_at(bytes, 32),
            // 36..40: `total_extlen` und `padding`. Erweiterungen meldet
            // dieser Server nicht an, also kommt hier nichts.
        })
    }
}

/// `struct fuse_init_in`, soweit gebraucht.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitIn {
    pub major: u32,
    pub minor: u32,
    pub max_readahead: u32,
    pub flags: u32,
    /// Die oberen 32 Bit der Flags. Erst ab 7.36 ueberhaupt vorhanden —
    /// bei einem aelteren Kernel steht hier null, und das ist die richtige
    /// Antwort: Er bietet nichts an, was dort stuende.
    pub flags2: u32,
}

impl InitIn {
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 16 {
            return None;
        }
        Some(InitIn {
            major: u32_at(bytes, 0),
            minor: u32_at(bytes, 4),
            max_readahead: u32_at(bytes, 8),
            flags: u32_at(bytes, 12),
            flags2: if bytes.len() >= 20 {
                u32_at(bytes, 16)
            } else {
                0
            },
        })
    }
}

/// `struct fuse_read_in`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadIn {
    pub fh: u64,
    pub offset: u64,
    pub size: u32,
    pub flags: u32,
}

impl ReadIn {
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 40 {
            return None;
        }
        Some(ReadIn {
            fh: u64_at(bytes, 0),
            offset: u64_at(bytes, 8),
            size: u32_at(bytes, 16),
            flags: u32_at(bytes, 32),
        })
    }
}

/// `struct fuse_open_in`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenIn {
    pub flags: u32,
}

impl OpenIn {
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 8 {
            return None;
        }
        Some(OpenIn {
            flags: u32_at(bytes, 0),
        })
    }
}

/// `struct fuse_forget_in`.
pub fn forget_nlookup(bytes: &[u8]) -> Option<u64> {
    (bytes.len() >= 8).then(|| u64_at(bytes, 0))
}

/// `struct fuse_batch_forget_in` samt seiner Liste.
///
/// Liefert Paare aus Nodeid und Zahl der zu vergessenden Lookups.
pub fn batch_forget(bytes: &[u8]) -> Vec<(u64, u64)> {
    // 0..4 count, 4..8 dummy, danach `struct fuse_forget_one { nodeid, nlookup }`.
    if bytes.len() < 8 {
        return Vec::new();
    }
    let count = u32_at(bytes, 0) as usize;
    let mut forgets = Vec::with_capacity(count.min(1024));
    for index in 0..count {
        let at = 8 + index * 16;
        if at + 16 > bytes.len() {
            break;
        }
        forgets.push((u64_at(bytes, at), u64_at(bytes, at + 8)));
    }
    forgets
}

/// `struct fuse_create_in`, gefolgt vom Namen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreateIn {
    pub flags: u32,
    pub mode: u32,
    pub umask: u32,
}

impl CreateIn {
    pub const SIZE: usize = 16;

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        // Die `umask` steht immer da, angewandt ist sie aber nur, solange
        // `FUSE_POSIX_ACL` nicht ausgehandelt wurde. Danach ist sie Aufgabe
        // des Servers — wer sie in beiden Faellen anwendet, zieht jede neue
        // Datei ein zweites Mal ab.
        Some(CreateIn {
            flags: u32_at(bytes, 0),
            mode: u32_at(bytes, 4),
            umask: u32_at(bytes, 8),
        })
    }
}

/// `struct fuse_mkdir_in`, gefolgt vom Namen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MkdirIn {
    pub mode: u32,
    pub umask: u32,
}

impl MkdirIn {
    pub const SIZE: usize = 8;

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        (bytes.len() >= Self::SIZE).then(|| MkdirIn {
            mode: u32_at(bytes, 0),
            umask: u32_at(bytes, 4),
        })
    }
}

/// `struct fuse_mknod_in`, gefolgt vom Namen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MknodIn {
    pub mode: u32,
    pub rdev: u32,
    pub umask: u32,
}

impl MknodIn {
    pub const SIZE: usize = 16;

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        (bytes.len() >= Self::SIZE).then(|| MknodIn {
            mode: u32_at(bytes, 0),
            rdev: u32_at(bytes, 4),
            umask: u32_at(bytes, 8),
        })
    }
}

/// `struct fuse_getxattr_in`, gefolgt vom Namen — und ohne Namen bei
/// `LISTXATTR`.
///
/// `size` ist die Groesse des Puffers, den der Aufrufer bereithaelt. **Null
/// heisst: nur fragen, wie gross es waere.** Dann gehoert ein
/// [`getxattr_out`] in die Antwort und nicht der Wert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GetxattrIn {
    pub size: u32,
}

impl GetxattrIn {
    pub const SIZE: usize = 8;

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        (bytes.len() >= Self::SIZE).then(|| GetxattrIn {
            size: u32_at(bytes, 0),
        })
    }
}

/// `struct fuse_setxattr_in`, gefolgt von Name und Wert.
///
/// # Warum acht Bytes und nicht sechzehn
///
/// Seit 7.33 traegt die Struktur zwei weitere Felder, aber der Kernel
/// schickt sie nur, wenn der Server `FUSE_SETXATTR_EXT` angemeldet hat.
/// Dieser tut das nicht — also kommt die alte Form
/// (`FUSE_COMPAT_SETXATTR_IN_SIZE`). Wer hier sechzehn annaehme, laese den
/// Namen ab der falschen Stelle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetxattrIn {
    /// Laenge des Wertes, der hinter dem Namen steht.
    pub size: u32,
    /// `XATTR_CREATE` oder `XATTR_REPLACE`, oder null.
    pub flags: u32,
}

impl SetxattrIn {
    pub const SIZE: usize = 8;

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        (bytes.len() >= Self::SIZE).then(|| SetxattrIn {
            size: u32_at(bytes, 0),
            flags: u32_at(bytes, 4),
        })
    }
}

/// `struct fuse_write_in`, gefolgt von den Nutzdaten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteIn {
    pub fh: u64,
    pub offset: u64,
    pub size: u32,
}

impl WriteIn {
    pub const SIZE: usize = 40;

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        (bytes.len() >= Self::SIZE).then(|| WriteIn {
            fh: u64_at(bytes, 0),
            offset: u64_at(bytes, 8),
            size: u32_at(bytes, 16),
        })
    }
}

/// `struct fuse_rename_in` und `struct fuse_rename2_in`, gefolgt von zwei
/// Namen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenameIn {
    pub newdir: u64,
    pub flags: u32,
    /// Wo hinter dem Kopf die beiden Namen beginnen.
    pub names_at: usize,
}

impl RenameIn {
    pub fn decode(bytes: &[u8], with_flags: bool) -> Option<Self> {
        let size = if with_flags { 16 } else { 8 };
        if bytes.len() < size {
            return None;
        }
        Some(RenameIn {
            newdir: u64_at(bytes, 0),
            flags: if with_flags { u32_at(bytes, 8) } else { 0 },
            names_at: size,
        })
    }
}

/// `struct fuse_link_in`, gefolgt vom neuen Namen.
pub fn link_oldnodeid(bytes: &[u8]) -> Option<u64> {
    (bytes.len() >= 8).then(|| u64_at(bytes, 0))
}

// Welche Felder eines `SETATTR` gesetzt sind.
pub const FATTR_MODE: u32 = 1 << 0;
pub const FATTR_UID: u32 = 1 << 1;
pub const FATTR_GID: u32 = 1 << 2;
pub const FATTR_SIZE: u32 = 1 << 3;
pub const FATTR_ATIME: u32 = 1 << 4;
pub const FATTR_MTIME: u32 = 1 << 5;
pub const FATTR_FH: u32 = 1 << 6;
pub const FATTR_ATIME_NOW: u32 = 1 << 7;
pub const FATTR_MTIME_NOW: u32 = 1 << 8;

/// `struct fuse_setattr_in`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetattrIn {
    pub valid: u32,
    pub fh: u64,
    pub size: u64,
    pub atime: i64,
    pub mtime: i64,
    pub atimensec: u32,
    pub mtimensec: u32,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
}

impl SetattrIn {
    pub const SIZE: usize = 88;

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        Some(SetattrIn {
            valid: u32_at(bytes, 0),
            fh: u64_at(bytes, 8),
            size: u64_at(bytes, 16),
            atime: u64_at(bytes, 32) as i64,
            mtime: u64_at(bytes, 40) as i64,
            atimensec: u32_at(bytes, 56),
            mtimensec: u32_at(bytes, 60),
            mode: u32_at(bytes, 68),
            uid: u32_at(bytes, 76),
            gid: u32_at(bytes, 80),
        })
    }

    pub fn has(&self, field: u32) -> bool {
        self.valid & field != 0
    }
}

/// Die beiden nullterminierten Namen hinter einem `RENAME` oder `SYMLINK`.
pub fn two_names(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let first = bytes.iter().position(|byte| *byte == 0)?;
    let rest = &bytes[first + 1..];
    let second = rest
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(rest.len());
    Some((&bytes[..first], &rest[..second]))
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    let mut raw = [0u8; 4];
    raw.copy_from_slice(&bytes[at..at + 4]);
    u32::from_ne_bytes(raw)
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    let mut raw = [0u8; 8];
    raw.copy_from_slice(&bytes[at..at + 8]);
    u64::from_ne_bytes(raw)
}

// --- Schreiben ------------------------------------------------------------

/// Sammelt eine Antwort byteweise.
#[derive(Debug, Default)]
pub struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    pub fn with_capacity(capacity: usize) -> Self {
        Writer {
            bytes: Vec::with_capacity(capacity),
        }
    }

    pub fn u32(&mut self, value: u32) -> &mut Self {
        self.bytes.extend_from_slice(&value.to_ne_bytes());
        self
    }

    pub fn i32(&mut self, value: i32) -> &mut Self {
        self.bytes.extend_from_slice(&value.to_ne_bytes());
        self
    }

    pub fn u64(&mut self, value: u64) -> &mut Self {
        self.bytes.extend_from_slice(&value.to_ne_bytes());
        self
    }

    pub fn u16(&mut self, value: u16) -> &mut Self {
        self.bytes.extend_from_slice(&value.to_ne_bytes());
        self
    }

    pub fn bytes(&mut self, value: &[u8]) -> &mut Self {
        self.bytes.extend_from_slice(value);
        self
    }

    /// Fuellt mit Nullbytes auf ein Vielfaches von acht auf.
    pub fn align8(&mut self) -> &mut Self {
        while self.bytes.len() % 8 != 0 {
            self.bytes.push(0);
        }
        self
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Die Dateiattribute, wie FUSE sie erwartet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Attr {
    pub ino: u64,
    pub size: u64,
    pub blocks: u64,
    pub atime: i64,
    pub mtime: i64,
    pub ctime: i64,
    pub atimensec: u32,
    pub mtimensec: u32,
    pub ctimensec: u32,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u32,
    pub blksize: u32,
}

impl Attr {
    /// Schreibt `struct fuse_attr`.
    pub fn encode(&self, out: &mut Writer) {
        out.u64(self.ino)
            .u64(self.size)
            .u64(self.blocks)
            .u64(self.atime as u64)
            .u64(self.mtime as u64)
            .u64(self.ctime as u64)
            .u32(self.atimensec)
            .u32(self.mtimensec)
            .u32(self.ctimensec)
            .u32(self.mode)
            .u32(self.nlink)
            .u32(self.uid)
            .u32(self.gid)
            .u32(self.rdev)
            .u32(self.blksize)
            .u32(0); // flags
    }
}

/// Schreibt `struct fuse_entry_out`.
pub fn entry_out(nodeid: u64, attr: &Attr, valid: u64) -> Writer {
    let mut out = Writer::with_capacity(ENTRY_OUT_SIZE);
    out.u64(nodeid)
        // Generation: Nodeids werden hier nie wiederverwendet, also bleibt sie
        // null. Die Kombination (nodeid, generation) muss ueber die Lebenszeit
        // des Dateisystems eindeutig sein — mit einem Zaehler, der nie
        // zurueckspringt, ist sie das ohne zweites Feld.
        .u64(0)
        .u64(valid)
        .u64(valid)
        .u32(0)
        .u32(0);
    attr.encode(&mut out);
    out
}

/// Schreibt `struct fuse_attr_out`.
pub fn attr_out(attr: &Attr, valid: u64) -> Writer {
    let mut out = Writer::with_capacity(ATTR_OUT_SIZE);
    out.u64(valid).u32(0).u32(0);
    attr.encode(&mut out);
    out
}

/// Schreibt `struct fuse_open_out`.
///
/// `backing_id` gilt nur zusammen mit [`FOPEN_PASSTHROUGH`]; ohne das Flag
/// muss sie null sein.
pub fn open_out(fh: u64, open_flags: u32, backing_id: i32) -> Writer {
    debug_assert!(
        backing_id == 0 || open_flags & FOPEN_PASSTHROUGH != 0,
        "eine backing_id ohne FOPEN_PASSTHROUGH bedeutet dem Kernel nichts"
    );
    let mut out = Writer::with_capacity(OPEN_OUT_SIZE);
    out.u64(fh).u32(open_flags).i32(backing_id);
    out
}

/// Die Antwort auf `FUSE_CREATE`: Eintrag **und** offene Datei in einem.
pub fn create_out(
    nodeid: u64,
    attr: &Attr,
    valid: u64,
    fh: u64,
    open_flags: u32,
    backing_id: i32,
) -> Writer {
    let mut out = entry_out(nodeid, attr, valid);
    out.bytes(open_out(fh, open_flags, backing_id).as_slice());
    out
}

/// Schreibt `struct fuse_write_out`.
pub fn write_out(written: u32) -> Writer {
    let mut out = Writer::with_capacity(WRITE_OUT_SIZE);
    out.u32(written).u32(0);
    out
}

/// `struct fuse_getxattr_out`: die Groesse, die der Wert haette.
///
/// Antwort auf ein `GETXATTR` oder `LISTXATTR` mit `size == 0`. Der Aufrufer
/// legt danach einen Puffer dieser Groesse an und fragt noch einmal.
pub fn getxattr_out(size: u32) -> Writer {
    let mut out = Writer::with_capacity(GETXATTR_OUT_SIZE);
    out.u32(size).u32(0);
    debug_assert_eq!(out.len(), GETXATTR_OUT_SIZE);
    out
}

/// Haengt einen `struct fuse_dirent` an.
///
/// Gibt `false` zurueck, wenn der Eintrag nicht mehr in den Rahmen passt —
/// dann ist die Antwort voll und der Kernel fragt mit dem naechsten Offset
/// nach.
pub fn push_dirent(
    out: &mut Writer,
    limit: usize,
    ino: u64,
    next_offset: u64,
    kind: u32,
    name: &[u8],
) -> bool {
    let entry_len = DIRENT_HEADER_SIZE + name.len();
    let padded = entry_len.div_ceil(8) * 8;
    if out.len() + padded > limit {
        return false;
    }
    out.u64(ino)
        // Der Offset, an dem es **danach** weitergeht. Der Kernel merkt sich
        // ihn und schickt ihn beim naechsten `READDIR` zurueck.
        .u64(next_offset)
        .u32(name.len() as u32)
        .u32(kind)
        .bytes(name)
        .align8();
    true
}

/// Schreibt `struct fuse_kstatfs`.
#[allow(clippy::too_many_arguments)]
pub fn statfs_out(
    blocks: u64,
    bfree: u64,
    bavail: u64,
    files: u64,
    ffree: u64,
    bsize: u32,
    namelen: u32,
    frsize: u32,
) -> Writer {
    let mut out = Writer::with_capacity(KSTATFS_SIZE);
    out.u64(blocks)
        .u64(bfree)
        .u64(bavail)
        .u64(files)
        .u64(ffree)
        .u32(bsize)
        .u32(namelen)
        .u32(frsize)
        .u32(0);
    for _ in 0..6 {
        out.u32(0);
    }
    out
}

/// Schreibt die Antwort auf `FUSE_INIT`, gekuerzt auf [`INIT_OUT_LEN`].
pub fn init_out(minor: u32, max_readahead: u32, flags: u32, flags2: u32, max_write: u32) -> Writer {
    let mut out = Writer::with_capacity(INIT_OUT_LEN);
    out.u32(FUSE_KERNEL_VERSION)
        .u32(minor)
        .u32(max_readahead)
        .u32(flags)
        .u16(0) // max_background
        .u16(0) // congestion_threshold
        .u32(max_write)
        .u32(1) // time_gran: Nanosekunden, denn btrfs kann sie
        .u16(0) // max_pages: 0 heisst, der Kernel entscheidet
        .u16(0) // map_alignment
        .u32(flags2)
        .u32(MAX_STACK_DEPTH);
    debug_assert_eq!(out.len(), INIT_OUT_LEN);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_written_structures_have_the_sizes_from_the_header() {
        // Die Zahlen stammen aus `include/uapi/linux/fuse.h` und nicht aus
        // `size_of` — sonst pruefte der Test die Rechnung gegen sich selbst.
        let attr = Attr::default();
        let mut writer = Writer::default();
        attr.encode(&mut writer);
        assert_eq!(writer.len(), ATTR_SIZE, "struct fuse_attr");

        assert_eq!(
            entry_out(1, &attr, 0).len(),
            ENTRY_OUT_SIZE,
            "struct fuse_entry_out"
        );
        assert_eq!(
            attr_out(&attr, 0).len(),
            ATTR_OUT_SIZE,
            "struct fuse_attr_out"
        );
        assert_eq!(
            open_out(0, 0, 0).len(),
            OPEN_OUT_SIZE,
            "struct fuse_open_out"
        );
        assert_eq!(
            statfs_out(0, 0, 0, 0, 0, 0, 0, 0).len(),
            KSTATFS_SIZE,
            "struct fuse_kstatfs"
        );
        assert_eq!(init_out(39, 0, 0, 0, 0).len(), INIT_OUT_LEN);
        assert_eq!(write_out(0).len(), WRITE_OUT_SIZE, "struct fuse_write_out");
        assert_eq!(
            create_out(1, &attr, 0, 0, 0, 0).len(),
            CREATE_OUT_SIZE,
            "fuse_entry_out und fuse_open_out hintereinander"
        );
    }

    #[test]
    fn the_passthrough_numbers_match_the_header() {
        // `FUSE_PASSTHROUGH` ist `1ULL << 37`, und die oberen 32 Bit liegen in
        // `flags2` — also Bit 5. Eine Verwechslung mit Bit 31 waere still: Der
        // Kernel meldete das Bit nicht zurueck, der Passthrough bliebe aus,
        // und alles liefe weiter, nur langsam.
        assert_eq!(FUSE_PASSTHROUGH_FLAG2, 0x20);
        assert_eq!(FOPEN_PASSTHROUGH, 0x80);
        assert_eq!(backing_map(3).len(), BACKING_MAP_SIZE);

        // Von Hand ausgerechnete Ioctl-Nummern, gegen die Zahlen aus dem
        // Header gehalten: `_IOW(229, 1, struct fuse_backing_map)` und
        // `_IOW(229, 2, uint32_t)`.
        assert_eq!(FUSE_DEV_IOC_BACKING_OPEN, 0x4010_E501);
        assert_eq!(FUSE_DEV_IOC_BACKING_CLOSE, 0x4004_E502);
    }

    #[test]
    fn the_read_structures_have_the_sizes_from_the_header() {
        // Ein zu klein angenommener Kopf verschoebe alles dahinter — bei
        // `SETATTR` waere das ein Modus, der aus einer Zeitangabe stammt.
        assert_eq!(CreateIn::SIZE, 16, "struct fuse_create_in");
        assert_eq!(MkdirIn::SIZE, 8, "struct fuse_mkdir_in");
        assert_eq!(MknodIn::SIZE, 16, "struct fuse_mknod_in");
        assert_eq!(WriteIn::SIZE, 40, "struct fuse_write_in");
        assert_eq!(SetattrIn::SIZE, 88, "struct fuse_setattr_in");
    }

    #[test]
    fn a_setattr_is_read_back_field_by_field() {
        let mut bytes = vec![0u8; SetattrIn::SIZE];
        let put32 = |bytes: &mut Vec<u8>, at: usize, value: u32| {
            bytes[at..at + 4].copy_from_slice(&value.to_ne_bytes());
        };
        let put64 = |bytes: &mut Vec<u8>, at: usize, value: u64| {
            bytes[at..at + 8].copy_from_slice(&value.to_ne_bytes());
        };
        put32(&mut bytes, 0, FATTR_MODE | FATTR_SIZE);
        put64(&mut bytes, 8, 9); // fh
        put64(&mut bytes, 16, 4096); // size
        put64(&mut bytes, 32, 111); // atime
        put64(&mut bytes, 40, 222); // mtime
        put32(&mut bytes, 56, 333); // atimensec
        put32(&mut bytes, 60, 444); // mtimensec
        put32(&mut bytes, 68, 0o644); // mode
        put32(&mut bytes, 76, 1000); // uid
        put32(&mut bytes, 80, 1001); // gid

        let request = SetattrIn::decode(&bytes).expect("erkannt");
        assert_eq!(request.fh, 9);
        assert_eq!(request.size, 4096);
        assert_eq!(request.atime, 111);
        assert_eq!(request.mtime, 222);
        assert_eq!(request.atimensec, 333);
        assert_eq!(request.mtimensec, 444);
        assert_eq!(request.mode, 0o644);
        assert_eq!(request.uid, 1000);
        assert_eq!(request.gid, 1001);
        assert!(request.has(FATTR_MODE));
        assert!(request.has(FATTR_SIZE));
        assert!(!request.has(FATTR_UID), "was nicht gesetzt ist, gilt nicht");
    }

    #[test]
    fn two_names_are_split_at_the_nul_byte() {
        assert_eq!(two_names(b"alt\0neu\0"), Some((&b"alt"[..], &b"neu"[..])));
        assert_eq!(two_names(b"alt\0neu"), Some((&b"alt"[..], &b"neu"[..])));
    }

    #[test]
    fn a_pair_without_a_separator_is_refused() {
        assert_eq!(two_names(b"nur-einer"), None);
    }

    #[test]
    fn the_second_name_may_be_empty() {
        // Kommt bei einem `SYMLINK` auf ein leeres Ziel vor. Der Aufrufer
        // muss das ablehnen — der Parser soll es nicht verschweigen.
        assert_eq!(two_names(b"name\0\0"), Some((&b"name"[..], &b""[..])));
    }

    #[test]
    fn a_rename_carries_its_flags_only_in_the_second_form() {
        let mut bytes = vec![0u8; 16];
        bytes[0..8].copy_from_slice(&7u64.to_ne_bytes());
        bytes[8..12].copy_from_slice(&1u32.to_ne_bytes());

        let plain = RenameIn::decode(&bytes, false).expect("erkannt");
        assert_eq!(plain.newdir, 7);
        assert_eq!(plain.flags, 0, "RENAME kennt keine Flags");
        assert_eq!(plain.names_at, 8);

        let extended = RenameIn::decode(&bytes, true).expect("erkannt");
        assert_eq!(extended.flags, 1);
        assert_eq!(
            extended.names_at, 16,
            "die Namen stehen hinter dem laengeren Kopf"
        );
    }

    #[test]
    fn a_header_is_read_back_field_by_field() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&56u32.to_ne_bytes()); // len
        bytes.extend_from_slice(&FUSE_LOOKUP.to_ne_bytes());
        bytes.extend_from_slice(&0xDEAD_BEEFu64.to_ne_bytes()); // unique
        bytes.extend_from_slice(&1u64.to_ne_bytes()); // nodeid
        bytes.extend_from_slice(&1000u32.to_ne_bytes()); // uid
        bytes.extend_from_slice(&1001u32.to_ne_bytes()); // gid
        bytes.extend_from_slice(&4242u32.to_ne_bytes()); // pid
        bytes.extend_from_slice(&0u32.to_ne_bytes()); // total_extlen, padding

        let header = InHeader::decode(&bytes).expect("erkannt");
        assert_eq!(header.len, 56);
        assert_eq!(header.opcode, FUSE_LOOKUP);
        assert_eq!(header.unique, 0xDEAD_BEEF);
        assert_eq!(header.nodeid, 1);
        assert_eq!(header.uid, 1000);
        assert_eq!(header.gid, 1001);
        assert_eq!(header.pid, 4242);
        assert_eq!(bytes.len(), IN_HEADER_SIZE);
    }

    #[test]
    fn a_truncated_header_is_refused_instead_of_guessed() {
        assert_eq!(InHeader::decode(&[0u8; 39]), None);
        assert_eq!(ReadIn::decode(&[0u8; 39]), None);
        assert_eq!(InitIn::decode(&[0u8; 15]), None);
        assert_eq!(OpenIn::decode(&[0u8; 7]), None);
        assert_eq!(forget_nlookup(&[0u8; 7]), None);
    }

    #[test]
    fn a_dirent_is_padded_to_eight_bytes() {
        let mut out = Writer::default();
        assert!(push_dirent(&mut out, 4096, 7, 1, 4, b"abc"));
        // 24 Bytes Kopf plus drei Bytes Name, aufgefuellt auf 32.
        assert_eq!(out.len(), 32);
        assert!(push_dirent(&mut out, 4096, 8, 2, 8, b"12345678"));
        assert_eq!(out.len(), 32 + 32);
    }

    #[test]
    fn a_dirent_that_does_not_fit_is_refused_rather_than_cut() {
        // Ein abgeschnittener Eintrag machte die ganze Antwort unlesbar: Der
        // Kernel liest den naechsten Kopf dort, wo der Name noch weiterlaeuft.
        let mut out = Writer::default();
        assert!(!push_dirent(&mut out, 16, 7, 1, 4, b"abc"));
        assert!(out.is_empty());
    }

    #[test]
    fn a_batch_forget_that_promises_more_than_it_delivers_is_cut_short() {
        // Die Anzahl steht im Kopf und ist damit Eingabe. Wer ihr glaubt,
        // liest ueber den Puffer hinaus.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1000u32.to_ne_bytes());
        bytes.extend_from_slice(&0u32.to_ne_bytes());
        bytes.extend_from_slice(&42u64.to_ne_bytes());
        bytes.extend_from_slice(&3u64.to_ne_bytes());

        assert_eq!(batch_forget(&bytes), vec![(42, 3)]);
    }

    #[test]
    fn an_empty_batch_forget_yields_nothing() {
        assert_eq!(batch_forget(&[]), Vec::new());
        assert_eq!(batch_forget(&[0u8; 8]), Vec::new());
    }
}
