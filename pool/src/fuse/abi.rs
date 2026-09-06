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
/// 7.31 und nicht die neueste: Alles darueber sind Erweiterungen, die hier
/// noch niemand benutzt, und jede angemeldete Erweiterung ist eine Zusage,
/// die eingeloest werden muss.
pub const FUSE_KERNEL_MINOR_VERSION: u32 = 31;

/// `FUSE_ASYNC_READ`. Ohne dieses Bit serialisiert der Kernel Lesevorgaenge
/// je Datei.
pub const FUSE_ASYNC_READ: u32 = 1 << 0;
/// `FUSE_BIG_WRITES`: Writes duerfen groesser als eine Seite sein.
pub const FUSE_BIG_WRITES: u32 = 1 << 5;
/// `FUSE_DO_READDIRPLUS`: der Kernel darf `READDIRPLUS` schicken.
///
/// Wird hier **nicht** angemeldet. `READDIRPLUS` spart ein `LOOKUP` je
/// Eintrag, verlangt aber, dass jeder Eintrag beim Auflisten voll gestatet
/// wird — im Pool heisst das, jeden Namen auf jedem Branch anzufassen. Ob
/// sich das lohnt, gehoert gemessen und nicht geraten.
pub const FUSE_DO_READDIRPLUS: u32 = 1 << 13;

/// Kleinster Lesepuffer, den der Kernel akzeptiert (`FUSE_MIN_READ_BUFFER`).
pub const FUSE_MIN_READ_BUFFER: usize = 8192;

// --- Groessen -------------------------------------------------------------

pub const IN_HEADER_SIZE: usize = 40;
pub const OUT_HEADER_SIZE: usize = 16;
pub const ATTR_SIZE: usize = 88;
pub const ENTRY_OUT_SIZE: usize = 128;
pub const ATTR_OUT_SIZE: usize = 104;
pub const OPEN_OUT_SIZE: usize = 16;
pub const KSTATFS_SIZE: usize = 80;
pub const DIRENT_HEADER_SIZE: usize = 24;

/// Wieviele Bytes der Antwort auf `FUSE_INIT` geschrieben werden.
///
/// # Warum eine Konstante und nicht `size_of`
///
/// Der Kernel lehnt eine Antwort ab, die **laenger** ist als seine eigene
/// `struct fuse_init_out` (`fuse_copy_out_args` gibt dann `-EINVAL`). Kuerzer
/// darf sie sein — fuer `INIT` ist `out_argvar` gesetzt, der Rest bleibt null.
/// Diese 32 Bytes reichen bis `map_alignment` und damit fuer 7.31; sie sind
/// auf jedem Kernel, der ueberhaupt in Frage kommt, kuerzer als dessen
/// Struktur. Wer hier spaeter `flags2` braucht, hebt die Zahl **und** die
/// Nebenversion an, nicht nur eines von beidem.
pub const INIT_OUT_LEN: usize = 32;

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
pub fn open_out(fh: u64, open_flags: u32) -> Writer {
    let mut out = Writer::with_capacity(OPEN_OUT_SIZE);
    out.u64(fh).u32(open_flags).i32(0);
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
pub fn init_out(minor: u32, max_readahead: u32, flags: u32, max_write: u32) -> Writer {
    let mut out = Writer::with_capacity(INIT_OUT_LEN);
    out.u32(FUSE_KERNEL_VERSION)
        .u32(minor)
        .u32(max_readahead)
        .u32(flags)
        .u16(0) // max_background
        .u16(0) // congestion_threshold
        .u32(max_write)
        .u32(1) // time_gran: Nanosekunden, denn btrfs kann sie
        .u16(0) // max_pages: 0 heisst „der Kernel entscheidet"
        .u16(0); // map_alignment
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
        assert_eq!(open_out(0, 0).len(), OPEN_OUT_SIZE, "struct fuse_open_out");
        assert_eq!(
            statfs_out(0, 0, 0, 0, 0, 0, 0, 0).len(),
            KSTATFS_SIZE,
            "struct fuse_kstatfs"
        );
        assert_eq!(init_out(31, 0, 0, 0).len(), INIT_OUT_LEN);
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
