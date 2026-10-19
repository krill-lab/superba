//! Data relocation with a fixed locator, inter-process exclusion and verified copies.
//!
//! The switch service stops all writers before using [`PreparedMove`]. A directory
//! is copied into private staging on the destination volume, verified, and renamed
//! before the small locator is atomically replaced. The source survives until the
//! router has restarted successfully. [`MoveJournal`] lets the caller finish or
//! roll back an interrupted operation on the next start.

mod atomic;
mod location;
mod relocation;
mod usage;
pub mod validation;

pub use atomic::{private_dir, write_json};
pub use location::{DataLock, LOCATION_FILE, LOCK_FILE, MOVE_FILE, MoveJournal, Pointer, resolve};
pub use relocation::{PreparedMove, check_space, cleanup_source, metadata_file, rewrite_paths};
pub use usage::{CATEGORIES, usage};
