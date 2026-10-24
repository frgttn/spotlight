//! The file index behind Search Files. Platform-free: walkers and the NTFS
//! journal (in `platform-win`) feed it [`Meta`] records, and it answers
//! ranked queries.
//!
//! - [`Index`]: a flat table of entries (parent, name, extension, flags,
//!   modification time; 16 bytes each), changed in place as files change.
//! - Names are stored once each, in an arena laid out for `neo_frizbee` and
//!   in an `fst` sorted by case-folded name; names added after the last build
//!   wait in a small map until [`Index::compact`].
//! - [`Query`] parses what the user typed; [`Index::search`] ranks the
//!   matches (see `search.rs`).
//!
//! The plan is in `notes/04-file-search-plan.md`.

mod index;
mod kind;
mod names;
mod query;
mod scan;
mod search;

pub use index::{Builder, EntryId, Index, Meta, NO_PARENT};
pub use kind::Kind;
pub use query::Query;
pub use scan::{Exclusions, Listed, ScanOptions, add_tree, is_excluded, scan};
pub use search::{Hit, SearchOptions};

#[cfg(test)]
mod tests;
