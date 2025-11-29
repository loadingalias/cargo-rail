//! CLI argument definitions for cargo-rail
//!
//! This module defines all CLI structures using clap. These are used by main.rs
//! and the dispatch logic in commands/mod.rs.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Root CLI wrapper for cargo subcommand integration
///
/// This wrapper allows cargo-rail to be invoked as `cargo rail <subcommand>`.
#[derive(Parser)]
#[command(name = "cargo")]
#[command(bin_name = "cargo")]
#[command(styles = get_styles())]
pub enum CargoCli {
  /// The rail subcommand
  Rail(RailCli),
}

/// Main CLI structure for cargo-rail
///
/// Contains global options and the subcommand to execute.
#[derive(Parser)]
#[command(name = "rail")]
#[command(version, about, long_about = None)]
#[command(propagate_version = true)]
#[command(styles = get_styles())]
pub struct RailCli {
  /// Suppress progress messages (for CI/automation)
  #[arg(long, short, global = true)]
  pub quiet: bool,

  /// The subcommand to execute
  #[command(subcommand)]
  pub command: Commands,
}

/// Available subcommands
#[derive(Subcommand)]
pub enum Commands {
  /// Show which crates are affected by changes
  Affected {
    /// Git ref to compare against (auto-detects origin/main or origin/master)
    #[arg(long)]
    since: Option<String>,
    /// Start ref (for SHA pair mode)
    #[arg(long, conflicts_with = "since", requires = "to")]
    from: Option<String>,
    /// End ref (for SHA pair mode)
    #[arg(long, requires = "from")]
    to: Option<String>,
    /// Output format [text, json, names-only, github, github-matrix, jsonl]
    #[arg(long, short = 'f', default_value = "text")]
    format: String,
    /// Show all workspace crates (ignore changes)
    #[arg(long, short = 'a')]
    all: bool,
    /// Write output to file (e.g., $GITHUB_OUTPUT)
    #[arg(long, short = 'o')]
    output: Option<PathBuf>,
  },

  /// Run tests for affected crates only
  Test {
    /// Git ref to compare against (auto-detects origin/main or origin/master)
    #[arg(long)]
    since: Option<String>,
    /// Skip change detection and run all tests
    #[arg(long, short = 'a')]
    all: bool,
    /// Disable automatic use of cargo-nextest
    #[arg(long)]
    skip_nextest: bool,
    /// Explain why tests are being run
    #[arg(long)]
    explain: bool,
    /// Pass additional arguments to the test runner
    #[arg(last = true)]
    test_args: Vec<String>,
  },

  /// Unify workspace dependencies (replaces workspace-hack crates)
  Unify {
    /// Action: 'undo' to restore a backup
    action: Option<String>,
    /// Dry-run mode: preview changes without modifying files
    #[arg(long, short = 'c')]
    check: bool,
    /// Output format [text, json]
    #[arg(long, short = 'f', default_value = "text")]
    format: String,
    /// Exclude dependencies from unification
    #[arg(long)]
    exclude: Vec<String>,
    /// Force include specific dependencies
    #[arg(long)]
    include: Vec<String>,
    /// Create backups of all modified files
    #[arg(long)]
    backup: bool,
    /// Pin transitive-only deps with fragmented features to workspace
    #[arg(long)]
    pin_transitives: bool,
    /// Include renamed dependencies (package = "...") in unification
    #[arg(long)]
    include_renamed: bool,
    /// List available backups (for undo action)
    #[arg(long)]
    list: bool,
    /// Specific backup ID to restore (for undo action)
    #[arg(long = "backup-id")]
    backup_id: Option<String>,
    /// Skip generating the unify report
    #[arg(long)]
    skip_report: bool,
    /// Custom path for the unify report (default: target/cargo-rail/unify-report.md)
    #[arg(long)]
    report_path: Option<PathBuf>,
    /// Show diff of changes to each manifest
    #[arg(long)]
    show_diff: bool,
  },

  /// Initialize configuration (rail.toml)
  Init {
    /// Output path for rail.toml
    #[arg(long, short, default_value = ".config/rail.toml")]
    output: String,
    /// Overwrite existing configuration
    #[arg(long)]
    force: bool,
    /// Skip interactive prompts
    #[arg(long)]
    non_interactive: bool,
    /// Dry-run mode: preview generated config without writing
    #[arg(long, short = 'c')]
    check: bool,
  },

  /// Split a crate to a standalone repository with git history
  Split {
    /// Action: 'init' to configure splits, or crate name to split
    action: Option<String>,
    /// Additional crate name(s) for init
    #[arg(conflicts_with = "all")]
    crate_names: Vec<String>,
    /// Split all configured crates (mutually exclusive with crate names)
    #[arg(short, long, conflicts_with = "action")]
    all: bool,
    /// Override remote repository
    #[arg(long)]
    remote: Option<String>,
    /// Dry-run mode: preview changes without executing
    #[arg(long, short = 'c')]
    check: bool,
    /// Output format [text, json]
    #[arg(long, short = 'f', default_value = "text")]
    format: String,
  },

  /// Sync changes between monorepo and split repos
  Sync {
    /// Crate name to sync (mutually exclusive with --all)
    #[arg(conflicts_with = "all")]
    crate_name: Option<String>,
    /// Sync all configured crates (mutually exclusive with crate name)
    #[arg(short, long)]
    all: bool,
    /// Override remote repository
    #[arg(long)]
    remote: Option<String>,
    /// Sync from remote to monorepo only
    #[arg(long)]
    from_remote: bool,
    /// Sync from monorepo to remote only
    #[arg(long)]
    to_remote: bool,
    /// Conflict resolution [ours, theirs, manual, union]
    #[arg(long, default_value = "manual")]
    strategy: String,
    /// Allow direct commits to protected branches
    #[arg(long)]
    no_protected_branches: bool,
    /// Dry-run mode: preview changes without executing
    #[arg(long, short = 'c')]
    check: bool,
    /// Output format [text, json]
    #[arg(long, short = 'f', default_value = "text")]
    format: String,
  },

  /// Publish releases (version bump, changelog, tag, publish)
  Release {
    /// Action: 'init' to configure release settings
    action: Option<String>,
    /// Crate name(s) to release (mutually exclusive with --all)
    #[arg(conflicts_with = "all")]
    crate_names: Vec<String>,
    /// Release all workspace crates (mutually exclusive with crate names)
    #[arg(short, long, conflicts_with = "action")]
    all: bool,
    /// Version bump [major, minor, patch, or "x.y.z"]
    #[arg(long, default_value = "patch")]
    bump: String,
    /// Dry-run mode: preview release plan without executing
    #[arg(long, short = 'c')]
    check: bool,
    /// Skip publishing to crates.io
    #[arg(long)]
    skip_publish: bool,
    /// Skip git tag creation
    #[arg(long)]
    skip_tag: bool,
    /// Output format [text, json]
    #[arg(long, short = 'f', default_value = "text")]
    format: String,
  },

  /// Validate release readiness
  Check {
    /// Crate name(s) to check (mutually exclusive with --all)
    #[arg(conflicts_with = "all")]
    crate_names: Vec<String>,
    /// Check all workspace crates (mutually exclusive with crate names)
    #[arg(short, long)]
    all: bool,
    /// Output format [text, json]
    #[arg(long, short = 'f', default_value = "text")]
    format: String,
  },

  /// Clean generated artifacts (cache, backups, reports)
  Clean {
    /// Clean metadata cache only
    #[arg(long)]
    cache: bool,
    /// Prune old backups
    #[arg(long)]
    backups: bool,
    /// Clean generated reports
    #[arg(long)]
    reports: bool,
    /// Dry-run mode: preview what would be cleaned
    #[arg(long, short = 'c')]
    check: bool,
  },

  /// Validate configuration file
  #[command(name = "config")]
  Config {
    /// Action: 'validate' to check configuration
    action: String,
    /// Output format [text, json]
    #[arg(long, short = 'f', default_value = "text")]
    format: String,
  },
}

fn get_styles() -> clap::builder::Styles {
  clap::builder::Styles::styled()
}

impl Commands {
  /// Check if this command uses JSON output format
  ///
  /// Returns true for any format that produces structured output.
  /// Used for early JSON mode detection to suppress progress messages.
  pub fn is_json_format(&self) -> bool {
    match self {
      Commands::Affected { format, .. } => {
        let f = format.to_lowercase();
        matches!(f.as_str(), "json" | "jsonl" | "json-lines" | "github" | "github-matrix")
      }
      Commands::Unify { format, .. }
      | Commands::Split { format, .. }
      | Commands::Sync { format, .. }
      | Commands::Release { format, .. }
      | Commands::Check { format, .. }
      | Commands::Config { format, .. } => format.to_lowercase() == "json",
      _ => false,
    }
  }
}
