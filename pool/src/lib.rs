// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Der Pool-Namespace, Meilenstein 5.
//!
//! # Was ein Pool ist
//!
//! Jeder Data-Member traegt sein eigenes btrfs. Ein Nutzer will davon nichts
//! wissen — er will `Filme/` sehen, nicht `Platte 3/Filme/`. Der Pool legt
//! genau diese eine Sicht ueber die Members: ein Verzeichnisbaum, gebildet aus
//! der Vereinigung aller Branches.
//!
//! # Die Regel, die den Pool von einem Union-Dateisystem unterscheidet
//!
//! **Eine Datei liegt vollstaendig auf genau einem Member.** Kein Stueckeln,
//! kein Striping, keine Fortsetzung auf der naechsten Platte. Das ist keine
//! Vereinfachung, sondern die Kerninvariante des Projekts: Fallen mehr Members
//! aus, als die Paritaet abdeckt, sollen die uebrigen einzeln montierbar und
//! **vollstaendig lesbar** bleiben. Eine Datei, deren zweite Haelfte auf der
//! verlorenen Platte lag, waere das nicht.
//!
//! Daraus folgt die zweite Regel: **Der Pool speichert nichts.** Er ist eine
//! Sicht und kein Zustand. Es gibt keinen Index, keine Datenbank, keine
//! Zuordnungstabelle — nichts, dessen Verlust eine Platte unlesbar machte. Wer
//! eine Platte ausbaut und woanders einhaengt, sieht dort genau die Dateien,
//! die er im Pool unter denselben Namen gesehen hat.
//!
//! # Kein Overlay
//!
//! Alle Branches sind gleichrangig. Es gibt keine obere und keine untere
//! Schicht, keine Whiteouts, kein Copy-up. Geloescht wird auf dem Branch, der
//! die Datei traegt; geschrieben wird dort, wo sie schon liegt. Ein Overlay
//! braucht seinen Zustand — siehe die zweite Regel.
//!
//! # Was hier steht und was nicht
//!
//! Dieses Crate **entscheidet**: auf welchen Branch ein neues Objekt gehoert,
//! wie ein Name aufzuloesen ist, der auf mehreren Branches vorkommt, was eine
//! Verzeichnisauflistung ueber alle Branches ergibt. Es liest kein
//! Verzeichnis und legt keine Datei an — die Auflistungen kommen als Parameter
//! herein.
//!
//! Dieselbe Trennung wie bei `engine/`: Was rechnet, laesst sich ueberall und
//! deterministisch pruefen; was I/O macht, braucht einen Kernel und einen
//! Mount. Die FUSE-Schale kommt danach und wird die Entscheidungen von hier
//! ausfuehren, nicht noch einmal treffen.
//!
//! ```
//! use ferrite_pool::{place, Allocation, Branch, BranchId, PlacementRequest, SharePolicy};
//!
//! // Drei Platten, die mittlere hat am meisten frei.
//! let branches = [
//!     Branch::new(BranchId(0), 100 << 30, 10 << 30),
//!     Branch::new(BranchId(1), 100 << 30, 60 << 30),
//!     Branch::new(BranchId(2), 100 << 30, 20 << 30),
//! ];
//!
//! let policy = SharePolicy {
//!     allocation: Allocation::MostFree,
//!     ..SharePolicy::default()
//! };
//!
//! let request = PlacementRequest::new("Filme/Neu/film.mkv");
//! let placement = place(&branches, &policy, &request)?;
//! assert_eq!(placement.branch, BranchId(1));
//! # Ok::<(), ferrite_pool::PoolError>(())
//! ```

pub mod branch;
pub mod error;
pub mod merge;
pub mod path;
pub mod place;
pub mod policy;

pub use branch::{total_free, total_size, Branch, BranchId};
pub use error::{PoolError, Result};
pub use merge::{
    merge_listing, resolve, BranchEntry, Conflict, EntryKind, MergedEntry, Resolution,
};
pub use path::{ancestor_at, components, depth_of};
pub use place::{place, Placement, PlacementRequest};
pub use policy::{Allocation, SharePolicy, SplitDepth, SplitOverflow};

/// Die FUSE-Schale. Braucht `/dev/fuse` und damit Linux.
#[cfg(target_os = "linux")]
pub mod fuse;
