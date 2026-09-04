// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Das ublk-Target: ein Blockgeraet im Userspace, ohne den Kernel zu patchen.
//!
//! **Linux-only.** Hier liegt die Grenze, die der Kickoff urspruenglich um den
//! ganzen I/O-Pfad ziehen wollte. Der Geraetezugriff aus `device.rs` kommt mit
//! `std` aus und laeuft ueberall; ublk kommt ohne io_uring nicht aus, und das
//! gibt es nur hier.
//!
//! Ein ublk-Geraet **pro Data-Member**, nicht eines fuer den ganzen Pool. Das
//! folgt aus der Kerninvariante: Ein Geraet ueber alle Platten waere ein
//! Striping-Layout, und dann waere keine Platte mehr einzeln montierbar. Auf
//! der rohen Platte liegt das Dateisystem entsprechend ab `payload_offset`,
//! also 1 MiB — wer eine Platte direkt mountet, braucht diesen Offset.
//!
//! Der Ablauf ist vom Treiber vorgegeben und nicht verhandelbar:
//!
//! 1. `/dev/ublk-control` oeffnen, Features abfragen
//! 2. `ADD_DEV`, `SET_PARAMS`
//! 3. Je Queue einen Thread: `/dev/ublkcN` oeffnen, Deskriptorbereich mappen,
//!    fuer jeden Tag ein `FETCH_REQ` absenden — **vor** dem Start
//! 4. `START_DEV`. Ab jetzt existiert `/dev/ublkbN`
//! 5. Requests bedienen, jeden mit `COMMIT_AND_FETCH_REQ` beantworten
//! 6. `STOP_DEV`, `DEL_DEV`
//!
//! Schritt 3 vor Schritt 4 ist keine Stilfrage: Der Treiber bindet jede Queue
//! an den **Thread**, der ihr erstes `FETCH_REQ` abgesetzt hat, und bricht ab,
//! wenn beim Start keiner da ist. Deshalb bedient jeden Queue derselbe Thread,
//! der sie geoeffnet hat, und nicht irgendeiner aus einem Pool.

pub mod control;
pub mod queue;
pub mod uapi;

use std::sync::mpsc;
use std::thread::JoinHandle;

pub use control::{UblkControl, UblkSpec, CONTROL_PATH};
pub use queue::{Completion, Request, RequestKind, UblkQueue};

use crate::device::MemberDevice;
use crate::error::{EngineError, Result};

/// Was hinter einem ublk-Geraet steckt.
///
/// Ein Target sieht Offsets relativ zum Anfang des Geraets, nicht zum Anfang
/// der Platte. Die Umrechnung auf `payload_offset` macht die Umsetzung, nicht
/// der Gast — btrfs weiss nichts von Superbloecken.
pub trait Target: Send {
    fn read(&mut self, offset: u64, buffer: &mut [u8]) -> Result<()>;
    fn write(&mut self, offset: u64, data: &[u8]) -> Result<()>;
    fn flush(&mut self) -> Result<()>;
}

/// Ein Target, das die Payload-Region eines Members 1:1 durchreicht.
///
/// Die Stufe vor dem Schreibpfad: keine Paritaet, kein Log. Sie beweist, dass
/// der ublk-Weg traegt, und ist der Bezugspunkt, gegen den sich der
/// Schreibpfad spaeter messen lassen muss.
#[derive(Debug)]
pub struct Passthrough {
    device: MemberDevice,
    payload_offset: u64,
    payload_size: u64,
}

impl Passthrough {
    pub fn new(device: MemberDevice, payload_offset: u64, payload_size: u64) -> Self {
        Passthrough {
            device,
            payload_offset,
            payload_size,
        }
    }

    /// Prueft, dass der Bereich in der Payload-Region liegt.
    ///
    /// Der Gast kennt nur die Geraetegroesse, die wir ihm gemeldet haben. Waere
    /// diese Pruefung nicht da, koennte ein Request hinter das Ende der Region
    /// greifen — im besten Fall in den Backup-Superblock.
    fn resolve(&self, offset: u64, len: usize) -> Result<u64> {
        let end = offset
            .checked_add(len as u64)
            .ok_or(EngineError::OffsetOverflow {
                offset,
                len: len as u64,
            })?;
        if end > self.payload_size {
            return Err(EngineError::BeyondDevice {
                offset,
                len: len as u64,
                size: self.payload_size,
            });
        }
        Ok(self.payload_offset + offset)
    }
}

impl Target for Passthrough {
    fn read(&mut self, offset: u64, buffer: &mut [u8]) -> Result<()> {
        let at = self.resolve(offset, buffer.len())?;
        self.device.read_at(at, buffer)
    }

    fn write(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        let at = self.resolve(offset, data.len())?;
        self.device.write_at(at, data)
    }

    fn flush(&mut self) -> Result<()> {
        self.device.flush()
    }
}

/// Ein laufendes ublk-Geraet.
///
/// Solange dieser Wert lebt, existiert `/dev/ublkbN` und nimmt I/O an.
#[derive(Debug)]
pub struct UblkDevice {
    control: UblkControl,
    dev_id: u32,
    workers: Vec<JoinHandle<Result<()>>>,
}

impl UblkDevice {
    /// Legt das Geraet an, startet die Queues und macht es benutzbar.
    ///
    /// `targets` muss genau `spec.nr_hw_queues` Eintraege haben — jede Queue
    /// bekommt ihr eigenes, weil sie in ihrem eigenen Thread laeuft.
    pub fn start<T: Target + 'static>(spec: &UblkSpec, targets: Vec<T>) -> Result<Self> {
        if targets.len() != usize::from(spec.nr_hw_queues) {
            return Err(EngineError::Ublk {
                what: "zu jeder Queue gehoert genau ein Target",
                errno: libc::EINVAL,
            });
        }

        let mut control = UblkControl::open()?;
        let info = control.add_device(spec)?;
        let dev_id = info.dev_id;

        // Ab hier existiert ein Geraet im Kernel. Geht etwas schief, muss es
        // wieder weg — sonst bleibt eine Karteileiche zurueck, die beim
        // naechsten Lauf die Nummer belegt.
        let started = Self::spin_up(&mut control, dev_id, spec, targets);
        match started {
            Ok(workers) => Ok(UblkDevice {
                control,
                dev_id,
                workers,
            }),
            Err(error) => {
                let _ = control.stop_device(dev_id);
                let _ = control.delete_device(dev_id);
                Err(error)
            }
        }
    }

    fn spin_up<T: Target + 'static>(
        control: &mut UblkControl,
        dev_id: u32,
        spec: &UblkSpec,
        targets: Vec<T>,
    ) -> Result<Vec<JoinHandle<Result<()>>>> {
        control.set_params(dev_id, spec)?;

        let buffer_size = spec.max_io_buf_bytes as usize;
        let depth = spec.queue_depth;
        let mut workers = Vec::with_capacity(targets.len());

        // Jede Queue meldet, sobald ihre FETCHes stehen. Erst wenn alle so weit
        // sind, darf `START_DEV` kommen.
        for (q_id, target) in targets.into_iter().enumerate() {
            let (ready_tx, ready_rx) = mpsc::channel();
            let q_id = q_id as u16;

            let worker = std::thread::Builder::new()
                .name(format!("ferrite-ublk{dev_id}-q{q_id}"))
                .spawn(move || {
                    let queue = match UblkQueue::open(dev_id, q_id, depth) {
                        Ok(queue) => {
                            // Der Fehler beim Senden bedeutet, dass niemand mehr
                            // wartet — dann ist der Start ohnehin abgebrochen.
                            let _ = ready_tx.send(Ok(()));
                            queue
                        }
                        Err(error) => {
                            let _ = ready_tx.send(Err(error.clone()));
                            return Err(error);
                        }
                    };
                    serve(queue, target, buffer_size)
                })
                .map_err(|_| EngineError::Ublk {
                    what: "Thread fuer die Queue anlegen",
                    errno: libc::EAGAIN,
                })?;

            match ready_rx.recv() {
                Ok(Ok(())) => workers.push(worker),
                Ok(Err(error)) => return Err(error),
                Err(_) => {
                    return Err(EngineError::Ublk {
                        what: "Queue-Thread endete vor der Bereitmeldung",
                        errno: libc::EIO,
                    })
                }
            }
        }

        // SAFETY-frei: `getpid` hat keine Vorbedingungen. Der Treiber merkt
        // sich diesen Prozess und bricht ab, wenn er verschwindet.
        let pid = std::process::id() as i32;
        control.start_device(dev_id, pid)?;
        Ok(workers)
    }

    pub fn dev_id(&self) -> u32 {
        self.dev_id
    }

    /// Pfad des Blockgeraets, das der Gast benutzt.
    pub fn block_path(&self) -> String {
        format!("/dev/ublkb{}", self.dev_id)
    }

    /// Haelt das Geraet an und raeumt es weg.
    ///
    /// Nimmt `self`, weil es danach keins mehr gibt. Ein `stop`, das man
    /// zweimal aufrufen kann, waere ein zweites `DEL_DEV` auf eine Nummer, die
    /// inzwischen jemand anderem gehoert.
    pub fn stop(mut self) -> Result<()> {
        // `STOP_DEV` laesst die ausstehenden FETCHes fehlschlagen, und daran
        // erkennen die Queue-Threads, dass Schluss ist.
        self.control.stop_device(self.dev_id)?;
        for worker in self.workers.drain(..) {
            match worker.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => return Err(error),
                Err(_) => {
                    return Err(EngineError::Ublk {
                        what: "ein Queue-Thread ist abgestuerzt",
                        errno: libc::EIO,
                    })
                }
            }
        }
        self.control.delete_device(self.dev_id)
    }
}

/// Die Schleife, die eine Queue bedient.
fn serve<T: Target>(mut queue: UblkQueue, mut target: T, buffer_size: usize) -> Result<()> {
    let mut buffer = vec![0u8; buffer_size];

    while let Some(request) = queue.next_request()? {
        let completion = handle(&mut target, &queue, &request, &mut buffer);
        queue.complete(request.tag, completion)?;
    }
    Ok(())
}

/// Bedient einen Request.
///
/// Gibt niemals einen Fehler nach aussen: Ein Request, der nicht beantwortet
/// wird, laesst den Gast haengen. Was schiefgeht, wird dem Gast als `errno`
/// gemeldet — er kann damit umgehen, mit einem Timeout nicht.
fn handle<T: Target>(
    target: &mut T,
    queue: &UblkQueue,
    request: &Request,
    buffer: &mut [u8],
) -> Completion {
    if request.len > buffer.len() {
        // Der Treiber haelt sich an `max_io_buf_bytes`. Kommt trotzdem etwas
        // Groesseres, stimmt eine Annahme nicht — dann lieber ablehnen als in
        // einen zu kleinen Puffer schreiben.
        return Completion::Failed(libc::EINVAL);
    }
    let slice = &mut buffer[..request.len];

    let outcome = match request.kind {
        RequestKind::Read => target
            .read(request.offset, slice)
            .and_then(|()| queue.write_payload(request.tag, slice)),
        RequestKind::Write => queue
            .read_payload(request.tag, slice)
            .and_then(|()| target.write(request.offset, slice))
            .and_then(|()| if request.fua { target.flush() } else { Ok(()) }),
        RequestKind::Flush => target.flush(),
        // Discard und Write-Zeroes gibt es hier nicht. `EOPNOTSUPP` sagt dem
        // Gast, dass er es lassen soll; ein gemeldeter Erfolg waere eine Luege
        // ueber Daten, die weiterhin dastehen.
        RequestKind::Discard | RequestKind::WriteZeroes => {
            return Completion::Failed(libc::EOPNOTSUPP)
        }
        RequestKind::Unsupported(_) => return Completion::Failed(libc::EOPNOTSUPP),
    };

    match outcome {
        Ok(()) => Completion::Done(request.len),
        Err(error) => Completion::Failed(errno_of(&error)),
    }
}

/// Uebersetzt einen internen Fehler in etwas, das der Gast versteht.
fn errno_of(error: &EngineError) -> i32 {
    match error {
        EngineError::Io { raw_os_error, .. } => raw_os_error.unwrap_or(libc::EIO),
        EngineError::Ublk { errno, .. } => *errno,
        EngineError::BeyondDevice { .. } | EngineError::OffsetOverflow { .. } => libc::ENOSPC,
        EngineError::NotWritable => libc::EROFS,
        _ => libc::EIO,
    }
}
