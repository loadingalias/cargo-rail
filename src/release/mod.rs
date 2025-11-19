//! Release automation for cargo-rail
//!
//! # Design Philosophy
//!
//! - **Safe by default**: Dry-run mode is default, requires explicit --execute
//! - **Graph-aware**: Uses dependency graph for correct publish ordering
//! - **Minimal deps**: Zero deps except winnow for parsing, system git/cargo for operations
//! - **Per-crate changelogs**: CHANGELOG.md lives with each crate in monorepos
//! - **Supply chain safe**: Custom lightweight changelog generator (no git-cliff)
//!
//! # Module Organization
//!
//! - `changelog`: Lightweight changelog generation using winnow for commit parsing
//! - `version`: Version bumping and Cargo.toml manipulation
//! - `planner`: Release planning and dry-run analysis
//! - `publisher`: Actual publishing to crates.io and GitHub
//! - `validator`: Pre-release validation checks

pub mod changelog;
pub mod planner;
pub mod publisher;
pub mod validator;
pub mod version;

pub use planner::ReleasePlanner;
pub use publisher::ReleasePublisher;
pub use validator::ReleaseValidator;
pub use version::VersionBumper;
