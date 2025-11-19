//! cargo-rail library
//!
//! This library provides workspace orchestration tools for Rust monorepos.

pub mod cargo;
pub mod change_detection;
pub mod commands;
pub mod config;
pub mod error;
pub mod git;
pub mod graph;
pub mod split;
pub mod sync;
pub mod test;
pub mod utils;
pub mod workspace;

// Re-export commonly used types
pub use error::{RailError, RailResult};
