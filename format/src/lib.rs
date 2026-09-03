//! On-Disk-Format von Ferrite.
//!
//! Dieses Crate ist die ausfuehrbare Fassung von `docs/FORMAT.md`. Es macht
//! kein I/O, oeffnet keine Geraete und kennt keine Konfiguration — es
//! uebersetzt zwischen Bytes und Typen und setzt die Gueltigkeitsregeln durch.
//!
//! Der Grund fuer diese Trennung ist praktisch: Ein Crate ohne I/O laesst sich
//! vollstaendig gegen zufaellige und boesartige Eingaben pruefen, ohne je eine
//! Platte anzufassen. Alles, was Geraete oeffnet, gehoert in `engine`.
//!
//! ```
//! use ferrite_format::{Role, Superblock, Uuid};
//!
//! let array = Uuid::from_random_bytes([1u8; 16]);
//! let member = Uuid::from_random_bytes([2u8; 16]);
//! let mut superblock = Superblock::new(array, member, Role::Data, 4, 64 * 1024 * 1024);
//! superblock.label = "tank".to_string();
//!
//! let block = superblock.encode().unwrap();
//! assert_eq!(Superblock::decode(&block).unwrap(), superblock);
//! ```

pub mod assemble;
pub mod crc32c;
pub mod error;
pub mod log;
pub mod superblock;
pub mod uuid;

pub use assemble::{assemble, ArrayLayout};
pub use crc32c::crc32c as checksum;
pub use error::{FormatError, Result};
pub use log::ring::{LogRing, LogWriter, Replay, ReplayRecord, ReplayStop};
pub use log::{ChainBreak, ChainValidator, ChainVerdict, LogRecordHeader, RecordType};
pub use superblock::{AccessMode, MemberState, Role, Superblock};
pub use uuid::Uuid;
