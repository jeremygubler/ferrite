//! Planung des Schreib- und Rebuild-Pfads.
//!
//! Dieses Crate rechnet aus, **was** zu tun ist — welche Parity-Blöcke ein
//! Stapel Writes dreckig macht, welche Bloecke einem wiederaufgebauten Member
//! noch fehlen, ob ein Member als Datenquelle taugt. Es oeffnet dafuer kein
//! Geraet und ruft kein `read` und kein `write`.
//!
//! Das ublk-Target, das diese Plaene ausfuehrt, ist Linux-only und kommt
//! spaeter hinter `#[cfg(target_os = "linux")]` dazu. Die Trennung ist
//! Absicht und dieselbe wie bei `format` und `parity`: Was rechnet, laesst
//! sich ueberall und deterministisch pruefen; was I/O macht, braucht einen
//! Kernel und eine Platte.
//!
//! ```
//! use ferrite_engine::{dirty_blocks, BlockGeometry, WriteTarget};
//!
//! // Ein Array mit 4-KiB-Bloecken. Zwei Writes, die im selben Block landen,
//! // und einer daneben.
//! let geometry = BlockGeometry::new(12, 64);
//! let writes = [
//!     WriteTarget { slot_index: 0, offset: 0, len: 512 },
//!     WriteTarget { slot_index: 2, offset: 1024, len: 512 },
//!     WriteTarget { slot_index: 1, offset: 8192, len: 4096 },
//! ];
//!
//! // Block 0 einmal, Block 2 einmal — der Slot spielt keine Rolle, Paritaet
//! // wird ueber gleiche Offsets gebildet.
//! assert_eq!(dirty_blocks(&geometry, &writes)?, vec![0..1, 2..3]);
//! # Ok::<(), ferrite_engine::EngineError>(())
//! ```

pub mod error;
pub mod geometry;
pub mod rebuild;
pub mod write_path;

pub use error::{EngineError, Result};
pub use geometry::{dirty_blocks, total_blocks, BlockGeometry, WriteTarget};
pub use rebuild::{data_is_valid_at, RebuildPlan};
pub use write_path::{
    required_parity_update, BatchOrigin, BatchStage, BlockSituation, ParityUpdate, SourceState,
    WriteBatch,
};
