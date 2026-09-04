// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Eine ublk-Queue: Requests abholen, beantworten, wieder abholen.
//!
//! Der Treiber legt jeden Request als [`UblksrvIoDesc`] in einen gemeinsamen
//! Speicherbereich, indiziert ueber den Tag. Wir holen ihn mit `FETCH_REQ` ab
//! und beantworten ihn mit `COMMIT_AND_FETCH_REQ` — das Kommando committet und
//! holt in einem Zug das naechste. Ein Tag ohne ausstehendes `FETCH` ist ein
//! Tag, auf dem der Treiber nichts mehr zustellen kann.
//!
//! # Warum `UBLK_F_USER_COPY`
//!
//! Ohne dieses Feature bekommt ein Write erst einen Request ohne Daten, dann
//! muesste `UBLK_IO_NEED_GET_DATA` einen zweiten Umlauf machen, um an sie zu
//! kommen. Mit `USER_COPY` liegen die Daten unter einem Offset auf
//! `/dev/ublkcN` und werden mit gewoehnlichem positioniertem Lesen und
//! Schreiben geholt — derselbe Weg wie in `device.rs`. Ein Zustandsautomat
//! statt zweier, und der zweite waere hier von niemandem ausfuehrbar.

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

/// Abstand zwischen den Deskriptorbereichen zweier Queues im `mmap`.
///
/// Der Treiber rechnet mit dem Maximum, nicht mit der tatsaechlichen Tiefe —
/// wer hier die eigene `queue_depth` einsetzt, mappt ab Queue 1 an der
/// falschen Stelle.
const QUEUE_DESC_STRIDE: u64 = UBLK_MAX_QUEUE_DEPTH as u64 * 24;

/// Was der Treiber von uns will.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Read,
    Write,
    Flush,
    Discard,
    WriteZeroes,
    /// Etwas, das dieses Target nicht bedient. Wird mit `EOPNOTSUPP`
    /// beantwortet statt stillschweigend als erledigt gemeldet.
    Unsupported(u8),
}

/// Ein einzelner Request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    pub tag: u16,
    pub kind: RequestKind,
    /// Offset in Bytes, umgerechnet aus dem 512-Byte-Sektor des Treibers.
    pub offset: u64,
    pub len: usize,
    /// Der Gast verlangt, dass dieser Write die Platte erreicht, bevor er
    /// bestaetigt wird.
    pub fua: bool,
}

/// Wie ein Request beantwortet wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    /// Erledigt, so viele Bytes uebertragen.
    Done(usize),
    /// Fehlgeschlagen, mit diesem positiven `errno`.
    Failed(i32),
}

/// Eine geoeffnete Queue eines ublk-Geraets.
pub struct UblkQueue {
    /// `/dev/ublkcN` — traegt sowohl die Kommandos als auch, unter eigenen
    /// Offsets, die Nutzdaten jedes Tags.
    channel: File,
    ring: CmdRing,
    q_id: u16,
    depth: u16,
    descriptors: Descriptors,
}

impl UblkQueue {
    /// Oeffnet die Queue und meldet fuer jeden Tag ein `FETCH_REQ` an.
    ///
    /// Muss **vor** `START_DEV` passieren: Der Treiber prueft beim Start, dass
    /// jede Queue bedient wird, und bricht sonst ab.
    pub fn open(dev_id: u32, q_id: u16, depth: u16) -> Result<Self> {
        if depth == 0 || depth > UBLK_MAX_QUEUE_DEPTH {
            return Err(EngineError::Ublk {
                what: "Queue-Tiefe ausserhalb des Erlaubten",
                errno: libc::EINVAL,
            });
        }

        let channel = OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!("/dev/ublkc{dev_id}"))
            .map_err(io_error("ublkc oeffnen"))?;

        let ring = CmdRing::builder()
            .build(u32::from(depth).next_power_of_two())
            .map_err(io_error("io_uring fuer die Queue anlegen"))?;

        let descriptors = Descriptors::map(&channel, q_id, depth)?;

        let mut queue = UblkQueue {
            channel,
            ring,
            q_id,
            depth,
            descriptors,
        };
        for tag in 0..depth {
            queue.push(UBLK_U_IO_FETCH_REQ, tag, 0)?;
        }
        queue
            .ring
            .submit()
            .map_err(io_error("FETCH_REQ absenden"))?;
        Ok(queue)
    }

    pub fn q_id(&self) -> u16 {
        self.q_id
    }

    pub fn depth(&self) -> u16 {
        self.depth
    }

    /// Wartet auf den naechsten Request.
    ///
    /// `None` heisst, dass der Treiber die Queue abgeraeumt hat — das Geraet
    /// wird gestoppt.
    pub fn next_request(&mut self) -> Result<Option<Request>> {
        loop {
            if let Some(completion) = self.ring.completion().next() {
                let tag = completion.user_data() as u16;
                let result = completion.result();

                // Der Treiber signalisiert das Ende, indem er die
                // ausstehenden FETCHes mit einem Fehler abschliesst.
                if result < 0 {
                    return Ok(None);
                }
                if result == UBLK_IO_RES_NEED_GET_DATA {
                    // Kann mit `UBLK_F_USER_COPY` nicht auftreten. Wenn doch,
                    // liegt eine falsche Annahme vor — melden statt raten.
                    return Err(EngineError::Ublk {
                        what: "NEED_GET_DATA trotz USER_COPY",
                        errno: libc::EPROTO,
                    });
                }
                return Ok(Some(self.descriptors.request(tag)?));
            }

            self.ring
                .submit_and_wait(1)
                .map_err(io_error("auf einen Request warten"))?;
        }
    }

    /// Liest die Nutzdaten eines Write-Requests.
    ///
    /// Sie liegen unter einem eigenen Offset auf `/dev/ublkcN` — das ist, was
    /// `UBLK_F_USER_COPY` bedeutet.
    pub fn read_payload(&self, tag: u16, buffer: &mut [u8]) -> Result<()> {
        crate::device::read_exact_at(&self.channel, io_buf_offset(self.q_id, tag), buffer)
            .map_err(io_error("Nutzdaten des Requests lesen"))
    }

    /// Stellt die Nutzdaten eines Read-Requests bereit.
    pub fn write_payload(&self, tag: u16, data: &[u8]) -> Result<()> {
        crate::device::write_all_at(&self.channel, io_buf_offset(self.q_id, tag), data)
            .map_err(io_error("Nutzdaten des Requests schreiben"))
    }

    /// Beantwortet einen Request und meldet denselben Tag wieder an.
    ///
    /// Beides in einem Kommando: Ein Commit ohne anschliessendes Fetch liesse
    /// den Tag tot zurueck, und die Queue wuerde mit jedem Request schmaler.
    pub fn complete(&mut self, tag: u16, completion: Completion) -> Result<()> {
        let result = match completion {
            Completion::Done(bytes) => i32::try_from(bytes).unwrap_or(i32::MAX),
            Completion::Failed(errno) => -errno.abs(),
        };
        self.push(UBLK_U_IO_COMMIT_AND_FETCH_REQ, tag, result)?;
        self.ring
            .submit()
            .map_err(io_error("Antwort absenden"))
            .map(|_| ())
    }

    fn push(&mut self, op: u32, tag: u16, result: i32) -> Result<()> {
        let cmd = UblksrvIoCmd {
            q_id: self.q_id,
            tag,
            result,
            // Bei `USER_COPY` wird `addr` nicht benutzt.
            addr: 0,
        };
        let entry = opcode::UringCmd80::new(types::Fd(self.channel.as_raw_fd()), op)
            .cmd(cmd.encode_into_sqe())
            .build()
            .user_data(u64::from(tag));

        // SAFETY: `entry` verweist nur auf `self.channel`, das die Queue
        // ueberlebt. Der Kommandoblock liegt in der SQE selbst, es gibt keinen
        // Puffer, der laenger leben muesste.
        unsafe {
            self.ring
                .submission()
                .push(&entry)
                .map_err(|_| EngineError::Ublk {
                    what: "io_uring-Warteschlange voll",
                    errno: libc::EAGAIN,
                })
        }
    }
}

impl UblksrvIoCmd {
    fn encode_into_sqe(&self) -> [u8; 80] {
        let mut out = [0u8; 80];
        out[0..2].copy_from_slice(&self.q_id.to_le_bytes());
        out[2..4].copy_from_slice(&self.tag.to_le_bytes());
        out[4..8].copy_from_slice(&self.result.to_le_bytes());
        out[8..16].copy_from_slice(&self.addr.to_le_bytes());
        out
    }
}

// --- Der gemappte Deskriptorbereich ---------------------------------------

/// Der Bereich, in den der Treiber die Requests schreibt.
///
/// Nur lesbar gemappt: Geschrieben wird hier ausschliesslich vom Kernel.
#[derive(Debug)]
struct Descriptors {
    base: *const u8,
    len: usize,
    depth: u16,
}

// SAFETY: Der Bereich wird nur gelesen, und der Zeiger bleibt bis zum `Drop`
// gueltig. Ohne diese Zusage koennte die Queue nicht zwischen Threads wandern —
// was der Schreibpfad spaeter braucht, wenn jede Queue ihren eigenen bekommt.
unsafe impl Send for Descriptors {}

impl Descriptors {
    fn map(channel: &File, q_id: u16, depth: u16) -> Result<Self> {
        let entry_size = core::mem::size_of::<UblksrvIoDesc>();
        let page_size = page_size();
        let len = round_up(depth as usize * entry_size, page_size);
        let offset = UBLKSRV_CMD_BUF_OFFSET + u64::from(q_id) * QUEUE_DESC_STRIDE;

        // SAFETY: `len` ist auf die Seitengroesse aufgerundet, `offset` ist der
        // vom Treiber vorgegebene Wert fuer diese Queue, und `channel` bleibt
        // waehrend des Aufrufs offen. Ein Fehler kommt als `MAP_FAILED`
        // zurueck und wird unten geprueft.
        let base = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_SHARED | libc::MAP_POPULATE,
                channel.as_raw_fd(),
                offset as libc::off_t,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(EngineError::Ublk {
                what: "Deskriptorbereich mappen",
                errno: std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
            });
        }

        Ok(Descriptors {
            base: base as *const u8,
            len,
            depth,
        })
    }

    fn request(&self, tag: u16) -> Result<Request> {
        if tag >= self.depth {
            return Err(EngineError::Ublk {
                what: "Tag ausserhalb der Queue-Tiefe",
                errno: libc::EINVAL,
            });
        }
        let entry_size = core::mem::size_of::<UblksrvIoDesc>();
        let offset = tag as usize * entry_size;

        // SAFETY: `offset + entry_size` liegt innerhalb von `len`, weil `tag`
        // gegen `depth` geprueft ist und `len` auf mindestens
        // `depth * entry_size` aufgerundet wurde. Der Kernel haelt den Bereich,
        // solange das Mapping steht.
        let bytes = unsafe { core::slice::from_raw_parts(self.base.add(offset), entry_size) };

        let op_flags = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let nr_sectors = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let mut sector = [0u8; 8];
        sector.copy_from_slice(&bytes[8..16]);
        let start_sector = u64::from_le_bytes(sector);

        let op = (op_flags & 0xff) as u8;
        Ok(Request {
            tag,
            kind: match op {
                UBLK_IO_OP_READ => RequestKind::Read,
                UBLK_IO_OP_WRITE => RequestKind::Write,
                UBLK_IO_OP_FLUSH => RequestKind::Flush,
                UBLK_IO_OP_DISCARD => RequestKind::Discard,
                UBLK_IO_OP_WRITE_ZEROES => RequestKind::WriteZeroes,
                other => RequestKind::Unsupported(other),
            },
            // Der Treiber rechnet immer in 512-Byte-Sektoren, unabhaengig von
            // der logischen Blockgroesse des Geraets.
            offset: start_sector << 9,
            len: (nr_sectors as usize) << 9,
            fua: op_flags & UBLK_IO_F_FUA != 0,
        })
    }
}

impl Drop for Descriptors {
    fn drop(&mut self) {
        // SAFETY: `base` und `len` stammen aus dem `mmap` im Konstruktor und
        // wurden seither nicht veraendert.
        unsafe {
            libc::munmap(self.base as *mut libc::c_void, self.len);
        }
    }
}

fn page_size() -> usize {
    // SAFETY: `sysconf` hat keine Vorbedingungen und schreibt nirgendwohin.
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if size > 0 {
        size as usize
    } else {
        4096
    }
}

fn round_up(value: usize, to: usize) -> usize {
    value.div_ceil(to) * to
}

/// `IoUring` bringt kein `Debug` mit, deshalb hier von Hand — die Kennung der
/// Queue gehoert in jede Fehlermeldung, die sie erwaehnt.
impl core::fmt::Debug for UblkQueue {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UblkQueue")
            .field("q_id", &self.q_id)
            .field("depth", &self.depth)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_up_leaves_exact_multiples_alone() {
        assert_eq!(round_up(4096, 4096), 4096);
        assert_eq!(round_up(1, 4096), 4096);
        assert_eq!(round_up(4097, 4096), 8192);
        assert_eq!(round_up(0, 4096), 0);
    }

    /// Der Abstand zwischen den Queues rechnet mit dem Maximum, nicht mit der
    /// tatsaechlichen Tiefe. Waere es anders, laege Queue 1 an der falschen
    /// Stelle — und wir laesen die Requests einer anderen Queue.
    #[test]
    fn the_queue_stride_uses_the_maximum_depth() {
        assert_eq!(QUEUE_DESC_STRIDE, 4096 * 24);
    }

    #[test]
    fn the_io_command_lands_at_the_offsets_the_driver_expects() {
        let cmd = UblksrvIoCmd {
            q_id: 1,
            tag: 7,
            result: -5,
            addr: 0,
        };
        let encoded = cmd.encode_into_sqe();
        assert_eq!(u16::from_le_bytes([encoded[0], encoded[1]]), 1);
        assert_eq!(u16::from_le_bytes([encoded[2], encoded[3]]), 7);
        assert_eq!(
            i32::from_le_bytes([encoded[4], encoded[5], encoded[6], encoded[7]]),
            -5
        );
        assert!(encoded[16..].iter().all(|&byte| byte == 0));
    }
}
