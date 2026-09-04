// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Die ublk-ABI, 1:1 aus `linux/ublk_cmd.h`.
//!
//! Nichts hier ist geraten. Jede Konstante und jedes Feld steht so im
//! UAPI-Header des Kernels; wo eine Zahl aus einem Makro kommt, rechnet sie
//! eine `const fn` nach statt sie als Literal zu wiederholen. Ein falsch
//! abgeschriebenes Offset ergaebe hier keinen Uebersetzungsfehler, sondern
//! einen Kernel, der etwas anderes tut als gemeint.
//!
//! Der Grund fuer die eigene Abschrift statt `bindgen`: Die ublk-ABI ist Teil
//! des stabilen UAPI und aendert sich nicht mehr. Eine Codegenerierung zur
//! Bauzeit machte das Projekt von den installierten Kernel-Headern abhaengig —
//! und die sind auf jeder Distribution eine andere Version als der laufende
//! Kernel. Hier steht, wogegen wir gebaut haben.

#![allow(dead_code)]

use core::mem::size_of;

// --- ioctl-Kodierung, `asm-generic/ioctl.h` ------------------------------

const IOC_NRBITS: u32 = 8;
const IOC_TYPEBITS: u32 = 8;
const IOC_SIZEBITS: u32 = 14;

const IOC_NRSHIFT: u32 = 0;
const IOC_TYPESHIFT: u32 = IOC_NRSHIFT + IOC_NRBITS;
const IOC_SIZESHIFT: u32 = IOC_TYPESHIFT + IOC_TYPEBITS;
const IOC_DIRSHIFT: u32 = IOC_SIZESHIFT + IOC_SIZEBITS;

const IOC_NONE: u32 = 0;
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

const fn ioc(dir: u32, kind: u32, nr: u32, size: usize) -> u32 {
    (dir << IOC_DIRSHIFT)
        | (kind << IOC_TYPESHIFT)
        | (nr << IOC_NRSHIFT)
        | ((size as u32) << IOC_SIZESHIFT)
}

const fn ior(kind: u32, nr: u32, size: usize) -> u32 {
    ioc(IOC_READ, kind, nr, size)
}

const fn iowr(kind: u32, nr: u32, size: usize) -> u32 {
    ioc(IOC_READ | IOC_WRITE, kind, nr, size)
}

/// `'u'` — der Typbuchstabe aller ublk-Kommandos.
const UBLK: u32 = 0x75;

// --- Control-Kommandos ----------------------------------------------------

const CMD_GET_QUEUE_AFFINITY: u32 = 0x01;
const CMD_GET_DEV_INFO: u32 = 0x02;
const CMD_ADD_DEV: u32 = 0x04;
const CMD_DEL_DEV: u32 = 0x05;
const CMD_START_DEV: u32 = 0x06;
const CMD_STOP_DEV: u32 = 0x07;
const CMD_SET_PARAMS: u32 = 0x08;
const CMD_GET_PARAMS: u32 = 0x09;
const CMD_GET_FEATURES: u32 = 0x13;

const CTRL_CMD_SIZE: usize = size_of::<UblksrvCtrlCmd>();

pub const UBLK_U_CMD_GET_QUEUE_AFFINITY: u32 = ior(UBLK, CMD_GET_QUEUE_AFFINITY, CTRL_CMD_SIZE);
pub const UBLK_U_CMD_GET_DEV_INFO: u32 = ior(UBLK, CMD_GET_DEV_INFO, CTRL_CMD_SIZE);
pub const UBLK_U_CMD_ADD_DEV: u32 = iowr(UBLK, CMD_ADD_DEV, CTRL_CMD_SIZE);
pub const UBLK_U_CMD_DEL_DEV: u32 = iowr(UBLK, CMD_DEL_DEV, CTRL_CMD_SIZE);
pub const UBLK_U_CMD_START_DEV: u32 = iowr(UBLK, CMD_START_DEV, CTRL_CMD_SIZE);
pub const UBLK_U_CMD_STOP_DEV: u32 = iowr(UBLK, CMD_STOP_DEV, CTRL_CMD_SIZE);
pub const UBLK_U_CMD_SET_PARAMS: u32 = iowr(UBLK, CMD_SET_PARAMS, CTRL_CMD_SIZE);
pub const UBLK_U_CMD_GET_PARAMS: u32 = ior(UBLK, CMD_GET_PARAMS, CTRL_CMD_SIZE);
pub const UBLK_U_CMD_GET_FEATURES: u32 = ior(UBLK, CMD_GET_FEATURES, CTRL_CMD_SIZE);

// --- I/O-Kommandos --------------------------------------------------------

const IO_FETCH_REQ: u32 = 0x20;
const IO_COMMIT_AND_FETCH_REQ: u32 = 0x21;
const IO_NEED_GET_DATA: u32 = 0x22;

const IO_CMD_SIZE: usize = size_of::<UblksrvIoCmd>();

pub const UBLK_U_IO_FETCH_REQ: u32 = iowr(UBLK, IO_FETCH_REQ, IO_CMD_SIZE);
pub const UBLK_U_IO_COMMIT_AND_FETCH_REQ: u32 = iowr(UBLK, IO_COMMIT_AND_FETCH_REQ, IO_CMD_SIZE);
pub const UBLK_U_IO_NEED_GET_DATA: u32 = iowr(UBLK, IO_NEED_GET_DATA, IO_CMD_SIZE);

// --- Feature-Bits ---------------------------------------------------------

pub const UBLK_F_SUPPORT_ZERO_COPY: u64 = 1 << 0;
pub const UBLK_F_URING_CMD_COMP_IN_TASK: u64 = 1 << 1;
pub const UBLK_F_NEED_GET_DATA: u64 = 1 << 2;
pub const UBLK_F_USER_RECOVERY: u64 = 1 << 3;
pub const UBLK_F_USER_RECOVERY_REISSUE: u64 = 1 << 4;
pub const UBLK_F_UNPRIVILEGED_DEV: u64 = 1 << 5;
pub const UBLK_F_CMD_IOCTL_ENCODE: u64 = 1 << 6;
/// Datentransfer ueber `pread`/`pwrite` auf `/dev/ublkcN` statt ueber
/// gemappte Puffer. Damit entfaellt der `NEED_GET_DATA`-Umweg fuer Writes.
pub const UBLK_F_USER_COPY: u64 = 1 << 7;
pub const UBLK_F_ZONED: u64 = 1 << 8;

// --- Geraetezustand -------------------------------------------------------

pub const UBLK_S_DEV_DEAD: u16 = 0;
pub const UBLK_S_DEV_LIVE: u16 = 1;
pub const UBLK_S_DEV_QUIESCED: u16 = 2;

// --- Operationen ----------------------------------------------------------

pub const UBLK_IO_OP_READ: u8 = 0;
pub const UBLK_IO_OP_WRITE: u8 = 1;
pub const UBLK_IO_OP_FLUSH: u8 = 2;
pub const UBLK_IO_OP_DISCARD: u8 = 3;
pub const UBLK_IO_OP_WRITE_SAME: u8 = 4;
pub const UBLK_IO_OP_WRITE_ZEROES: u8 = 5;

pub const UBLK_IO_F_FUA: u32 = 1 << 13;

pub const UBLK_IO_RES_OK: i32 = 0;
pub const UBLK_IO_RES_NEED_GET_DATA: i32 = 1;

// --- Offsets im `mmap` von `/dev/ublkcN` ---------------------------------

pub const UBLKSRV_CMD_BUF_OFFSET: u64 = 0;
pub const UBLKSRV_IO_BUF_OFFSET: u64 = 0x8000_0000;

const UBLK_IO_BUF_BITS: u32 = 25;
const UBLK_TAG_OFF: u32 = UBLK_IO_BUF_BITS;
const UBLK_TAG_BITS: u32 = 16;
const UBLK_QID_OFF: u32 = UBLK_TAG_OFF + UBLK_TAG_BITS;

/// Offset, unter dem der Puffer eines Requests in `/dev/ublkcN` erreichbar ist.
///
/// Bei `UBLK_F_USER_COPY` ist das der Offset fuer `pread`/`pwrite` — die Daten
/// werden nicht gemappt, sondern an dieser Stelle gelesen und geschrieben.
pub const fn io_buf_offset(q_id: u16, tag: u16) -> u64 {
    UBLKSRV_IO_BUF_OFFSET + (((q_id as u64) << UBLK_QID_OFF) | ((tag as u64) << UBLK_TAG_OFF))
}

pub const UBLK_MAX_QUEUE_DEPTH: u16 = 4096;

// --- Strukturen -----------------------------------------------------------

/// `struct ublksrv_ctrl_cmd`, wandert im `cmd`-Feld einer `URING_CMD`-SQE.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct UblksrvCtrlCmd {
    pub dev_id: u32,
    /// `u16::MAX`, wenn das Kommando nicht an eine Queue geht.
    pub queue_id: u16,
    pub len: u16,
    pub addr: u64,
    pub data: [u64; 1],
    pub dev_path_len: u16,
    pub pad: u16,
    pub reserved: u32,
}

/// `struct ublksrv_ctrl_dev_info`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct UblksrvCtrlDevInfo {
    pub nr_hw_queues: u16,
    pub queue_depth: u16,
    pub state: u16,
    pub pad0: u16,
    pub max_io_buf_bytes: u32,
    pub dev_id: u32,
    pub ublksrv_pid: i32,
    pub pad1: u32,
    pub flags: u64,
    pub ublksrv_flags: u64,
    pub owner_uid: u32,
    pub owner_gid: u32,
    pub reserved1: u64,
    pub reserved2: u64,
}

/// `struct ublksrv_io_desc` — ein Request, vom Treiber in den gemappten
/// Deskriptorbereich geschrieben und ueber den Tag indiziert.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct UblksrvIoDesc {
    /// Operation in Bit 0..8, Flags darueber.
    pub op_flags: u32,
    pub nr_sectors: u32,
    pub start_sector: u64,
    pub addr: u64,
}

impl UblksrvIoDesc {
    pub fn op(&self) -> u8 {
        (self.op_flags & 0xff) as u8
    }

    pub fn flags(&self) -> u32 {
        self.op_flags >> 8
    }
}

/// `struct ublksrv_io_cmd`, wandert im `cmd`-Feld einer `URING_CMD`-SQE an
/// `/dev/ublkcN`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct UblksrvIoCmd {
    pub q_id: u16,
    pub tag: u16,
    /// Nur bei `COMMIT*` von Bedeutung: uebertragene Bytes oder ein negativer
    /// `errno`.
    pub result: i32,
    pub addr: u64,
}

pub const UBLK_ATTR_READ_ONLY: u32 = 1 << 0;
pub const UBLK_ATTR_ROTATIONAL: u32 = 1 << 1;
pub const UBLK_ATTR_VOLATILE_CACHE: u32 = 1 << 2;
pub const UBLK_ATTR_FUA: u32 = 1 << 3;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct UblkParamBasic {
    pub attrs: u32,
    pub logical_bs_shift: u8,
    pub physical_bs_shift: u8,
    pub io_opt_shift: u8,
    pub io_min_shift: u8,
    pub max_sectors: u32,
    pub chunk_sectors: u32,
    pub dev_sectors: u64,
    pub virt_boundary_mask: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct UblkParamDiscard {
    pub discard_alignment: u32,
    pub discard_granularity: u32,
    pub max_discard_sectors: u32,
    pub max_write_zeroes_sectors: u32,
    pub max_discard_segments: u16,
    pub reserved0: u16,
}

/// Nur lesbar; der Treiber fuellt sie, nachdem das Geraet gestartet ist.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct UblkParamDevt {
    pub char_major: u32,
    pub char_minor: u32,
    pub disk_major: u32,
    pub disk_minor: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct UblkParamZoned {
    pub max_open_zones: u32,
    pub max_active_zones: u32,
    pub max_zone_append_sectors: u32,
    pub reserved: [u8; 20],
}

pub const UBLK_PARAM_TYPE_BASIC: u32 = 1 << 0;
pub const UBLK_PARAM_TYPE_DISCARD: u32 = 1 << 1;
pub const UBLK_PARAM_TYPE_DEVT: u32 = 1 << 2;
pub const UBLK_PARAM_TYPE_ZONED: u32 = 1 << 3;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct UblkParams {
    /// Beide Seiten koennen unterschiedliche Versionen dieser Struktur haben.
    /// Der Treiber richtet sich nach diesem Feld, deshalb muss es gesetzt sein.
    pub len: u32,
    pub types: u32,
    pub basic: UblkParamBasic,
    pub discard: UblkParamDiscard,
    pub devt: UblkParamDevt,
    pub zoned: UblkParamZoned,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Die Groessen, aus denen die Kommandonummern gerechnet werden. Stimmen
    /// sie nicht, stimmt keine einzige davon — und der Kernel lehnt alles ab
    /// oder tut, schlimmer, etwas anderes.
    #[test]
    fn the_structures_have_the_sizes_the_header_implies() {
        assert_eq!(size_of::<UblksrvCtrlCmd>(), 32);
        assert_eq!(size_of::<UblksrvIoCmd>(), 16);
        assert_eq!(size_of::<UblksrvIoDesc>(), 24);
        assert_eq!(size_of::<UblksrvCtrlDevInfo>(), 64);
    }

    /// Gegen von Hand nachgerechnete Werte aus `_IOWR('u', nr, struct)`.
    ///
    /// `(dir << 30) | (size << 16) | ('u' << 8) | nr`
    #[test]
    fn the_command_numbers_match_the_ioctl_encoding() {
        assert_eq!(
            UBLK_U_CMD_ADD_DEV,
            (3 << 30) | (32 << 16) | (0x75 << 8) | 0x04
        );
        assert_eq!(
            UBLK_U_CMD_GET_FEATURES,
            (2 << 30) | (32 << 16) | (0x75 << 8) | 0x13
        );
        assert_eq!(
            UBLK_U_IO_FETCH_REQ,
            (3 << 30) | (16 << 16) | (0x75 << 8) | 0x20
        );
        assert_eq!(
            UBLK_U_IO_COMMIT_AND_FETCH_REQ,
            (3 << 30) | (16 << 16) | (0x75 << 8) | 0x21
        );
    }

    #[test]
    fn the_io_buffer_offset_packs_queue_and_tag() {
        assert_eq!(io_buf_offset(0, 0), UBLKSRV_IO_BUF_OFFSET);
        assert_eq!(io_buf_offset(0, 1), UBLKSRV_IO_BUF_OFFSET + (1 << 25));
        assert_eq!(io_buf_offset(1, 0), UBLKSRV_IO_BUF_OFFSET + (1 << 41));
    }

    #[test]
    fn the_operation_is_the_low_byte_of_op_flags() {
        let desc = UblksrvIoDesc {
            op_flags: UBLK_IO_F_FUA | UBLK_IO_OP_WRITE as u32,
            ..Default::default()
        };
        assert_eq!(desc.op(), UBLK_IO_OP_WRITE);
        assert_eq!(desc.flags(), UBLK_IO_F_FUA >> 8);
    }
}
