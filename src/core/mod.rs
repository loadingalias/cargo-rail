//! Core engine for cargo-rail operations
//!
//! This module contains the fundamental building blocks for all cargo-rail functionality:
//!
//! - **config**: Rail configuration (rail.toml) parsing and validation
//! - **context**: Unified workspace context for efficient data sharing across operations
//! - **error**: Comprehensive error types with contextual help messages
//! - **executor**: Plan execution engine for deterministic operations
//! - **mapping**: Git commit mapping storage for split/sync operations
//! - **plan**: Operation planning and serialization
//! - **security**: Security validation for remotes, SSH, and protected branches
//! - **split**: Split monorepo crates to separate repositories
//! - **sync**: Bidirectional synchronization between monorepo and split repos
//! - **conflict**: Conflict detection and resolution strategies
//! - **vcs**: Git operations abstraction (SystemGit)

pub mod config;
pub mod conflict;
pub mod context;
pub mod error;
pub mod executor;
pub mod mapping;
pub mod plan;
pub mod security;
pub mod split;
pub mod sync;
pub mod vcs;
