//! Release automation commands for cargo-rail
//!
//! This module provides comprehensive release management:
//! - `plan`: Analyze workspace and suggest version bumps
//! - `prepare`: Update versions and generate changelogs
//! - `publish`: Publish crates to crates.io in topological order
//! - `finalize`: Create git tags and sync to split repos
//!
//! ## Safety
//! - All commands dry-run by default (require `--apply`)
//! - Semver checks prevent accidental breaking changes
//! - Topological ordering ensures dependencies publish first
//! - Post-publish verification confirms crates.io availability

pub mod changelog;
pub mod check;
pub mod finalize;
pub mod github;
pub mod graph;
pub mod plan;
pub mod prepare;
pub mod publish;
pub mod semver;
pub mod semver_check;
pub mod tags;
pub mod verify;

use crate::core::error::RailResult;
use clap::Subcommand;

/// Release automation subcommands
#[derive(Debug, Subcommand)]
pub enum ReleaseCommand {
  /// Analyze changes and suggest version bumps
  ///
  /// Scans conventional commits, runs semver checks,
  /// and outputs a release plan showing which crates
  /// need to be released and with what version bumps.
  Plan {
    /// Target specific crate (default: all changed)
    #[arg(long)]
    crate_name: Option<String>,

    /// Output as JSON for CI integration
    #[arg(long)]
    json: bool,

    /// Also check crates with no changes (for consistency)
    #[arg(long)]
    all: bool,
  },

  /// Update versions and generate changelogs
  ///
  /// Updates Cargo.toml versions according to the release plan,
  /// updates workspace dependencies, and generates CHANGELOG.md
  /// files from conventional commits.
  Prepare {
    /// Target specific crate (default: all from plan)
    #[arg(long)]
    crate_name: Option<String>,

    /// Actually modify files (dry-run by default)
    #[arg(long)]
    apply: bool,

    /// Skip changelog generation
    #[arg(long)]
    no_changelog: bool,
  },

  /// Publish crates to crates.io
  ///
  /// Publishes crates in topological order (dependencies first),
  /// with configurable delays to allow crates.io propagation.
  /// Includes pre-publish validation and post-publish verification.
  Publish {
    /// Target specific crate (default: all from plan)
    #[arg(long)]
    crate_name: Option<String>,

    /// Actually publish (dry-run by default)
    #[arg(long)]
    apply: bool,

    /// Skip confirmation prompts
    #[arg(long)]
    yes: bool,

    /// Delay between publishing dependent crates (seconds)
    #[arg(long, default_value = "30")]
    delay: u64,

    /// Dry-run (run cargo package but don't publish)
    #[arg(long)]
    dry_run: bool,
  },

  /// Create git tags and sync to split repos
  ///
  /// Creates annotated git tags for each released crate,
  /// optionally creates GitHub releases, and syncs the
  /// releases to split repositories.
  Finalize {
    /// Target specific crate (default: all from plan)
    #[arg(long)]
    crate_name: Option<String>,

    /// Actually create tags and push (dry-run by default)
    #[arg(long)]
    apply: bool,

    /// Skip creating GitHub releases
    #[arg(long)]
    no_github: bool,

    /// Skip syncing to split repos
    #[arg(long)]
    no_sync: bool,
  },
}

impl ReleaseCommand {
  /// Execute the release subcommand
  pub fn execute(&self) -> RailResult<()> {
    match self {
      ReleaseCommand::Plan { crate_name, json, all } => plan::run_release_plan(crate_name.as_deref(), *json, *all),

      ReleaseCommand::Prepare {
        crate_name,
        apply,
        no_changelog,
      } => prepare::run_release_prepare(crate_name.as_deref(), *apply, *no_changelog),

      ReleaseCommand::Publish {
        crate_name,
        apply,
        yes,
        delay,
        dry_run,
      } => publish::run_release_publish(crate_name.as_deref(), *apply, *yes, *delay, *dry_run),

      ReleaseCommand::Finalize {
        crate_name,
        apply,
        no_github,
        no_sync,
      } => finalize::run_release_finalize(crate_name.as_deref(), *apply, *no_github, *no_sync),
    }
  }
}
