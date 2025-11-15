//! Cargo workspace integration and manipulation
//!
//! This module provides utilities for working with Cargo workspaces:
//!
//! - **metadata**: Load and query Cargo.toml metadata using cargo_metadata
//! - **transform**: Transform Cargo.toml files (flatten workspace inheritance, convert path deps)
//! - **files**: Discover and copy auxiliary files (.cargo, rust-toolchain, etc.)
//! - **helpers**: Cargo-specific utility functions

pub mod files;
pub mod helpers;
pub mod metadata;
pub mod transform;
