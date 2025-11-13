//! Health checks and validation infrastructure
//!
//! This module provides a unified interface for running health checks and validations.
//! All checks implement the `Check` trait, making it easy to add new checks without
//! modifying core logic.
//!
//! # Built-in Checks
//!
//! - **workspace-validity**: Validates workspace structure and rail.toml configuration
//! - **ssh-keys**: Checks SSH key existence, permissions, and connectivity
//! - **git-notes**: Validates git-notes mappings integrity
//! - **remote-access**: Tests accessibility of configured remote repositories
//!
//! # Example
//!
//! ```rust,ignore
//! use cargo_rail::checks::{CheckContext, create_default_runner};
//!
//! let ctx = CheckContext {
//!   workspace_root: PathBuf::from("."),
//!   crate_name: None,
//!   thorough: true,
//! };
//!
//! let runner = create_default_runner();
//! let results = runner.run_all(&ctx)?;
//!
//! for result in results {
//!   if !result.passed {
//!     println!("❌ {}: {}", result.check_name, result.message);
//!   }
//! }
//! ```

mod git_notes;
mod remotes;
mod runner;
mod security_config;
mod ssh;
mod trait_def;
mod workspace;

// Re-export public API
pub use runner::create_default_runner;
pub use trait_def::{CheckContext, Severity};

// Individual checks are not exported - they're registered in create_default_runner()
// This keeps the API simple and prevents misuse
