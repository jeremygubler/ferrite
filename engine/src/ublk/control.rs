// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Die Control-Ebene von ublk: Geraete anlegen, starten, stoppen, loeschen.
//!
//! Alle Kommandos gehen als `IORING_OP_URING_CMD` an `/dev/ublk-control`. Der
//! Ring dafuer braucht `SQE128`: `ublksrv_ctrl_cmd` ist 32 Bytes gross und
//! passt nicht in die 16 Bytes, die eine gewoehnliche SQE fuer `cmd` hat.
//!
//! Die Strukturen werden von Hand in Bytes geschrieben und von Hand gelesen,
//! nicht ueber `transmute` auf `#[repr(C)]`. Das ist ein paar Zeilen mehr und
//! dafuer ohne `unsafe`, ohne Fragen zu Padding-Bytes und mit den Offsets
//! sichtbar an genau einer Stelle — dieselbe Regel wie bei den On-Disk-Strukturen
//! in `format/`.

use core::fmt;
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;

use io_uring::{cqueue, opcode, squeue, types, IoUring};

/// Ein Ring mit 128-Byte-SQEs.
///
/// `ublksrv_ctrl_cmd` ist 32 Bytes gross und passt nicht in die 16 Bytes, die
/// eine gewoehnliche SQE fuer `cmd` vorsieht. In `io-uring` 0.6 wird die
/// Groesse ueber den Typparameter gewaehlt, nicht ueber ein Flag.
type CmdRing = IoUring<squeue::Entry128, cqueue::Entry>;

use super::uapi::*;
use crate::error::{io_error, EngineError, Result};

/// Pfad des Control-Geraets. Es entsteht, sobald `ublk_drv` geladen ist.
pub const CONTROL_PATH: &str = "/dev/ublk-control";

/// Wieviele Kommandos gleichzeitig unterwegs sein duerfen.
///
/// Die Control-Ebene arbeitet ein Kommando nach dem anderen ab — es gibt hier
/// nichts zu parallelisieren, und ein kleiner Ring macht den Fehlerfall
/// ueberschaubar.
const CONTROL_RING_DEPTH: u32 = 4;

/// Was ein ublk-Geraet werden soll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UblkSpec {
    /// `None` laesst den Treiber eine freie Nummer waehlen.
    pub dev_id: Option<u32>,
    pub nr_hw_queues: u16,
    pub queue_depth: u16,
    /// Groesse des groessten Requests, den der Treiber stellen darf.
    pub max_io_buf_bytes: u32,
    /// Groesse des Geraets in Bytes.
    pub size: u64,
    pub logical_block_size: u32,
    pub read_only: bool,
}

impl Default for UblkSpec {
    fn default() -> Self {
        UblkSpec {
            dev_id: None,
            // Eine Queue. Mehrere braeuchten je einen Thread und eine
            // Sperrstrategie fuer die Paritaet — das gehoert in den
            // Schreibpfad und nicht in die Geraeteanlage.
            nr_hw_queues: 1,
            queue_depth: 64,
            max_io_buf_bytes: 512 * 1024,
            size: 0,
            logical_block_size: 4096,
            read_only: false,
        }
    }
}

/// Offener Draht zur Control-Ebene.
///
/// `IoUring` bringt kein `Debug` mit, deshalb steht es weiter unten von Hand —
/// ohne es waere in einer Fehlermeldung nicht zu sehen, gegen welchen Kernel
/// gerade gearbeitet wird.
pub struct UblkControl {
    file: File,
    ring: CmdRing,
    features: u64,
}

impl fmt::Debug for UblkControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UblkControl")
            .field("features", &format_args!("{:#x}", self.features))
            .field("user_copy", &self.supports_user_copy())
            .finish_non_exhaustive()
    }
}

impl UblkControl {
    /// Oeffnet `/dev/ublk-control` und fragt ab, was der Kernel kann.
    ///
    /// Fehlt das Geraet, ist `ublk_drv` nicht geladen — der Fehler sagt das,
    /// statt es zu umgehen.
    pub fn open() -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(CONTROL_PATH)
            .map_err(io_error("ublk-control oeffnen"))?;

        let ring = CmdRing::builder()
            .build(CONTROL_RING_DEPTH)
            .map_err(io_error("io_uring fuer ublk-control anlegen"))?;

        let mut control = UblkControl {
            file,
            ring,
            features: 0,
        };
        control.features = control.read_features()?;
        Ok(control)
    }

    /// Die Feature-Bits, die der laufende Kernel meldet.
    pub fn features(&self) -> u64 {
        self.features
    }

    /// Kann der Kernel `UBLK_F_USER_COPY`?
    ///
    /// Ohne dieses Feature muessten Writes ueber `UBLK_IO_NEED_GET_DATA` einen
    /// zweiten Umlauf machen. Das ist ein anderer Zustandsautomat, und einen
    /// zweiten zu bauen, den hier niemand ausfuehren kann, waere ungetesteter
    /// Code im Schreibpfad.
    pub fn supports_user_copy(&self) -> bool {
        self.features & UBLK_F_USER_COPY != 0
    }

    fn read_features(&mut self) -> Result<u64> {
        let mut buffer = [0u8; UBLK_FEATURES_LEN];
        let cmd = UblksrvCtrlCmd {
            dev_id: u32::MAX,
            queue_id: u16::MAX,
            len: buffer.len() as u16,
            addr: buffer.as_mut_ptr() as u64,
            ..Default::default()
        };
        self.submit(UBLK_U_CMD_GET_FEATURES, &cmd, "Features abfragen")?;
        Ok(u64::from_le_bytes(buffer))
    }

    /// Legt ein Geraet an. Es existiert danach, laeuft aber noch nicht.
    pub fn add_device(&mut self, spec: &UblkSpec) -> Result<UblksrvCtrlDevInfo> {
        if spec.nr_hw_queues == 0 || spec.queue_depth == 0 {
            return Err(EngineError::Ublk {
                what: "Geraet anlegen",
                errno: libc::EINVAL,
            });
        }
        if spec.queue_depth > UBLK_MAX_QUEUE_DEPTH {
            return Err(EngineError::Ublk {
                what: "queue_depth ueber dem Maximum",
                errno: libc::EINVAL,
            });
        }
        if !self.supports_user_copy() {
            return Err(EngineError::Ublk {
                what: "Kernel kann kein UBLK_F_USER_COPY",
                errno: libc::ENOTSUP,
            });
        }

        let info = UblksrvCtrlDevInfo {
            nr_hw_queues: spec.nr_hw_queues,
            queue_depth: spec.queue_depth,
            max_io_buf_bytes: spec.max_io_buf_bytes,
            dev_id: spec.dev_id.unwrap_or(u32::MAX),
            // `UBLK_F_CMD_IOCTL_ENCODE` sagt dem Treiber, dass wir die
            // `_IOWR`-kodierten Kommandonummern benutzen — und die benutzen
            // wir, weil die alten Nummern als veraltet gelten.
            flags: UBLK_F_CMD_IOCTL_ENCODE | UBLK_F_USER_COPY,
            ..Default::default()
        };

        let mut buffer = info.encode();
        let cmd = UblksrvCtrlCmd {
            dev_id: info.dev_id,
            queue_id: u16::MAX,
            len: buffer.len() as u16,
            addr: buffer.as_mut_ptr() as u64,
            ..Default::default()
        };
        self.submit(UBLK_U_CMD_ADD_DEV, &cmd, "Geraet anlegen")?;
        Ok(UblksrvCtrlDevInfo::decode(&buffer))
    }

    pub fn device_info(&mut self, dev_id: u32) -> Result<UblksrvCtrlDevInfo> {
        let mut buffer = [0u8; UblksrvCtrlDevInfo::SIZE];
        let cmd = UblksrvCtrlCmd {
            dev_id,
            queue_id: u16::MAX,
            len: buffer.len() as u16,
            addr: buffer.as_mut_ptr() as u64,
            ..Default::default()
        };
        self.submit(UBLK_U_CMD_GET_DEV_INFO, &cmd, "Geraeteinfo lesen")?;
        Ok(UblksrvCtrlDevInfo::decode(&buffer))
    }

    /// Setzt die Geraeteparameter. Muss vor `start_device` passieren.
    pub fn set_params(&mut self, dev_id: u32, spec: &UblkSpec) -> Result<()> {
        let shift = spec.logical_block_size.trailing_zeros() as u8;
        if spec.logical_block_size != 1 << shift || shift < 9 {
            return Err(EngineError::Ublk {
                what: "logische Blockgroesse ist keine Zweierpotenz ab 512",
                errno: libc::EINVAL,
            });
        }
        if spec.size % u64::from(spec.logical_block_size) != 0 {
            return Err(EngineError::Ublk {
                what: "Geraetegroesse ist kein Vielfaches der Blockgroesse",
                errno: libc::EINVAL,
            });
        }

        let mut attrs = 0;
        if spec.read_only {
            attrs |= UBLK_ATTR_READ_ONLY;
        }
        let params = UblkParams {
            len: UblkParams::SIZE as u32,
            types: UBLK_PARAM_TYPE_BASIC,
            basic: UblkParamBasic {
                attrs,
                logical_bs_shift: shift,
                physical_bs_shift: shift,
                io_opt_shift: shift,
                io_min_shift: shift,
                max_sectors: spec.max_io_buf_bytes >> 9,
                // Der Treiber rechnet in 512-Byte-Sektoren, unabhaengig von der
                // logischen Blockgroesse des Geraets.
                dev_sectors: spec.size >> 9,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut buffer = params.encode();
        let cmd = UblksrvCtrlCmd {
            dev_id,
            queue_id: u16::MAX,
            len: buffer.len() as u16,
            addr: buffer.as_mut_ptr() as u64,
            ..Default::default()
        };
        self.submit(UBLK_U_CMD_SET_PARAMS, &cmd, "Parameter setzen")
    }

    /// Startet das Geraet. Danach existiert `/dev/ublkbN` und nimmt I/O an.
    ///
    /// `pid` ist der Prozess, der die Queues bedient. Der Treiber merkt sich
    /// ihn und bricht ab, wenn er verschwindet — deshalb muss zu diesem
    /// Zeitpunkt bereits fuer jede Queue ein `FETCH_REQ` je Tag ausstehen.
    pub fn start_device(&mut self, dev_id: u32, pid: i32) -> Result<()> {
        let cmd = UblksrvCtrlCmd {
            dev_id,
            queue_id: u16::MAX,
            data: [pid as u64],
            ..Default::default()
        };
        self.submit(UBLK_U_CMD_START_DEV, &cmd, "Geraet starten")
    }

    pub fn stop_device(&mut self, dev_id: u32) -> Result<()> {
        let cmd = UblksrvCtrlCmd {
            dev_id,
            queue_id: u16::MAX,
            ..Default::default()
        };
        self.submit(UBLK_U_CMD_STOP_DEV, &cmd, "Geraet stoppen")
    }

    pub fn delete_device(&mut self, dev_id: u32) -> Result<()> {
        let cmd = UblksrvCtrlCmd {
            dev_id,
            queue_id: u16::MAX,
            ..Default::default()
        };
        self.submit(UBLK_U_CMD_DEL_DEV, &cmd, "Geraet loeschen")
    }

    /// Schickt ein Kommando und wartet auf sein Ergebnis.
    fn submit(&mut self, op: u32, cmd: &UblksrvCtrlCmd, what: &'static str) -> Result<()> {
        let entry = opcode::UringCmd80::new(types::Fd(self.file.as_raw_fd()), op)
            .cmd(cmd.encode_into_sqe())
            .build();

        // SAFETY: `entry` zeigt auf `self.file`, das diesen Aufruf ueberlebt,
        // und auf `cmd.addr` — einen Puffer, den der Aufrufer bis zum
        // Abschluss haelt, weil `submit_and_wait` darunter blockiert.
        unsafe {
            self.ring
                .submission()
                .push(&entry)
                .map_err(|_| EngineError::Ublk {
                    what: "io_uring-Warteschlange voll",
                    errno: libc::EAGAIN,
                })?;
        }
        self.ring
            .submit_and_wait(1)
            .map_err(io_error("io_uring absenden"))?;

        let completion = self.ring.completion().next().ok_or(EngineError::Ublk {
            what: "keine Antwort vom Treiber",
            errno: libc::EIO,
        })?;

        let result = completion.result();
        if result < 0 {
            return Err(EngineError::Ublk {
                what,
                errno: -result,
            });
        }
        Ok(())
    }
}

/// Laenge des Puffers fuer `UBLK_U_CMD_GET_FEATURES`.
const UBLK_FEATURES_LEN: usize = 8;

// --- Kodierung ------------------------------------------------------------
//
// Von Hand statt ueber `transmute`. Die Offsets stehen damit sichtbar an einer
// Stelle, es gibt keine Frage nach Padding-Bytes, und der Code bleibt ohne
// `unsafe` — dieselbe Regel wie bei den On-Disk-Strukturen in `format/`.

impl UblksrvCtrlCmd {
    pub const SIZE: usize = 32;

    fn encode_into_sqe(&self) -> [u8; 80] {
        let mut out = [0u8; 80];
        out[0..4].copy_from_slice(&self.dev_id.to_le_bytes());
        out[4..6].copy_from_slice(&self.queue_id.to_le_bytes());
        out[6..8].copy_from_slice(&self.len.to_le_bytes());
        out[8..16].copy_from_slice(&self.addr.to_le_bytes());
        out[16..24].copy_from_slice(&self.data[0].to_le_bytes());
        out[24..26].copy_from_slice(&self.dev_path_len.to_le_bytes());
        out[26..28].copy_from_slice(&self.pad.to_le_bytes());
        out[28..32].copy_from_slice(&self.reserved.to_le_bytes());
        out
    }
}

impl UblksrvCtrlDevInfo {
    pub const SIZE: usize = 64;

    fn encode(&self) -> [u8; Self::SIZE] {
        let mut out = [0u8; Self::SIZE];
        out[0..2].copy_from_slice(&self.nr_hw_queues.to_le_bytes());
        out[2..4].copy_from_slice(&self.queue_depth.to_le_bytes());
        out[4..6].copy_from_slice(&self.state.to_le_bytes());
        out[8..12].copy_from_slice(&self.max_io_buf_bytes.to_le_bytes());
        out[12..16].copy_from_slice(&self.dev_id.to_le_bytes());
        out[16..20].copy_from_slice(&self.ublksrv_pid.to_le_bytes());
        out[24..32].copy_from_slice(&self.flags.to_le_bytes());
        out[32..40].copy_from_slice(&self.ublksrv_flags.to_le_bytes());
        out[40..44].copy_from_slice(&self.owner_uid.to_le_bytes());
        out[44..48].copy_from_slice(&self.owner_gid.to_le_bytes());
        out
    }

    fn decode(bytes: &[u8; Self::SIZE]) -> Self {
        UblksrvCtrlDevInfo {
            nr_hw_queues: u16_at(bytes, 0),
            queue_depth: u16_at(bytes, 2),
            state: u16_at(bytes, 4),
            pad0: 0,
            max_io_buf_bytes: u32_at(bytes, 8),
            dev_id: u32_at(bytes, 12),
            ublksrv_pid: u32_at(bytes, 16) as i32,
            pad1: 0,
            flags: u64_at(bytes, 24),
            ublksrv_flags: u64_at(bytes, 32),
            owner_uid: u32_at(bytes, 40),
            owner_gid: u32_at(bytes, 44),
            reserved1: 0,
            reserved2: 0,
        }
    }
}

impl UblkParams {
    /// 112 und nicht 108: Die Struktur endet nach `zoned` bei 108, hat wegen
    /// der `u64` in `basic` aber eine Ausrichtung von 8 und wird deshalb auf
    /// 112 aufgefuellt. Der Wert geht als `len` an den Treiber — er richtet
    /// sich danach, wieviel er lesen darf.
    pub const SIZE: usize = 112;

    fn encode(&self) -> [u8; Self::SIZE] {
        let mut out = [0u8; Self::SIZE];
        out[0..4].copy_from_slice(&self.len.to_le_bytes());
        out[4..8].copy_from_slice(&self.types.to_le_bytes());

        // basic, ab Offset 8
        out[8..12].copy_from_slice(&self.basic.attrs.to_le_bytes());
        out[12] = self.basic.logical_bs_shift;
        out[13] = self.basic.physical_bs_shift;
        out[14] = self.basic.io_opt_shift;
        out[15] = self.basic.io_min_shift;
        out[16..20].copy_from_slice(&self.basic.max_sectors.to_le_bytes());
        out[20..24].copy_from_slice(&self.basic.chunk_sectors.to_le_bytes());
        out[24..32].copy_from_slice(&self.basic.dev_sectors.to_le_bytes());
        out[32..40].copy_from_slice(&self.basic.virt_boundary_mask.to_le_bytes());

        // discard, ab Offset 40
        out[40..44].copy_from_slice(&self.discard.discard_alignment.to_le_bytes());
        out[44..48].copy_from_slice(&self.discard.discard_granularity.to_le_bytes());
        out[48..52].copy_from_slice(&self.discard.max_discard_sectors.to_le_bytes());
        out[52..56].copy_from_slice(&self.discard.max_write_zeroes_sectors.to_le_bytes());
        out[56..58].copy_from_slice(&self.discard.max_discard_segments.to_le_bytes());

        // devt ab 60 ist nur lesbar, zoned ab 76 bleibt null.
        out
    }
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    let mut buffer = [0u8; 4];
    buffer.copy_from_slice(&bytes[offset..offset + 4]);
    u32::from_le_bytes(buffer)
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    let mut buffer = [0u8; 8];
    buffer.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_device_info_survives_a_roundtrip() {
        let info = UblksrvCtrlDevInfo {
            nr_hw_queues: 1,
            queue_depth: 64,
            state: UBLK_S_DEV_LIVE,
            max_io_buf_bytes: 512 * 1024,
            dev_id: 7,
            ublksrv_pid: 4242,
            flags: UBLK_F_CMD_IOCTL_ENCODE | UBLK_F_USER_COPY,
            owner_uid: 1000,
            owner_gid: 1000,
            ..Default::default()
        };
        let decoded = UblksrvCtrlDevInfo::decode(&info.encode());
        assert_eq!(decoded.nr_hw_queues, 1);
        assert_eq!(decoded.queue_depth, 64);
        assert_eq!(decoded.state, UBLK_S_DEV_LIVE);
        assert_eq!(decoded.dev_id, 7);
        assert_eq!(decoded.ublksrv_pid, 4242);
        assert_eq!(decoded.flags, UBLK_F_CMD_IOCTL_ENCODE | UBLK_F_USER_COPY);
        assert_eq!(decoded.owner_uid, 1000);
    }

    /// Die von Hand geschriebenen Offsets muessen zu dem passen, was der
    /// Compiler fuer die `#[repr(C)]`-Struktur ausrechnet. Weichen sie ab,
    /// liest der Treiber Felder an der falschen Stelle.
    #[test]
    fn the_hand_written_sizes_match_the_repr_c_layout() {
        use core::mem::size_of;
        assert_eq!(UblksrvCtrlCmd::SIZE, size_of::<UblksrvCtrlCmd>());
        assert_eq!(UblksrvCtrlDevInfo::SIZE, size_of::<UblksrvCtrlDevInfo>());
        assert_eq!(UblkParams::SIZE, size_of::<UblkParams>());
    }

    #[test]
    fn the_control_command_fits_into_the_large_sqe() {
        let cmd = UblksrvCtrlCmd {
            dev_id: 3,
            queue_id: u16::MAX,
            len: 64,
            addr: 0xDEAD_BEEF,
            data: [99],
            ..Default::default()
        };
        let encoded = cmd.encode_into_sqe();
        assert_eq!(u32_at(&encoded, 0), 3);
        assert_eq!(u16_at(&encoded, 4), u16::MAX);
        assert_eq!(u16_at(&encoded, 6), 64);
        assert_eq!(u64_at(&encoded, 8), 0xDEAD_BEEF);
        assert_eq!(u64_at(&encoded, 16), 99);
        // Alles hinter der Struktur bleibt null.
        assert!(encoded[UblksrvCtrlCmd::SIZE..].iter().all(|&b| b == 0));
    }
}
