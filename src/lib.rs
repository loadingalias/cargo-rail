//! cargo-rail library
//!
//! This library provides workspace orchestration tools for Rust monorepos.

// Only expose what's needed for testing
pub mod change_detection {
  pub mod classify {
    pub use crate::main_change_detection::classify::*;
  }
}

pub mod error;

// Internal module path (used by main.rs)
#[path = "change_detection/mod.rs"]
mod main_change_detection;

// Re-export commonly used types
pub use error::{RailError, RailResult};
