//! Narrow safe Windows filesystem operations for cargo-rail.
//!
//! The public API exists only on Windows. Other hosts can still build and document
//! this package without acquiring Windows capabilities.

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{
  FileObservation, LocalNtfsVolume, observe_file, open_for_observation, prove_local_ntfs, rename_write_through,
};
