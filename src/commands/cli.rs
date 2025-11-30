//! CLI argument definitions for cargo-rail
//!
//! This module defines all CLI structures using clap. These are used by main.rs
//! and the dispatch logic in commands/mod.rs.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

const MAIN_HELP: &str = "\
Monorepo orchestration for Rust workspaces.

Quick start:
  cargo rail init              # Generate rail.toml
  cargo rail affected          # See what changed
  cargo rail test              # Test affected crates only
  cargo rail unify --check     # Preview dependency unification

Docs: https://github.com/loadingalias/cargo-rail";

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
#[command(version)]
#[command(about = "Monorepo orchestration for Rust workspaces")]
#[command(long_about = MAIN_HELP)]
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

const AFFECTED_HELP: &str = "\
Examples:
  cargo rail affected                     # Changes since origin/main
  cargo rail affected --since HEAD~5      # Changes in last 5 commits
  cargo rail affected --from abc --to def # Changes between two SHAs
  cargo rail affected -f github-matrix    # Output for GitHub Actions matrix
  cargo rail affected -f names-only       # Just crate names, one per line";

const TEST_HELP: &str = "\
Examples:
  cargo rail test                         # Test affected crates
  cargo rail test --all                   # Test all crates
  cargo rail test -- --nocapture          # Pass args to test runner
  cargo rail test --explain               # Show why each crate is tested";

const UNIFY_HELP: &str = "\
Examples:
  cargo rail unify --check                # Preview changes (CI mode)
  cargo rail unify                        # Apply changes
  cargo rail unify --backup               # Apply with backup
  cargo rail unify --pin-transitives      # Pin fragmented deps (hakari replacement)
  cargo rail unify undo                   # Restore from backup
  cargo rail unify undo --list            # List available backups";

const SPLIT_HELP: &str = "\
Examples:
  cargo rail split init my-crate          # Configure split for my-crate
  cargo rail split my-crate --check       # Preview the split
  cargo rail split my-crate               # Execute the split
  cargo rail split --all                  # Split all configured crates";

const SYNC_HELP: &str = "\
Examples:
  cargo rail sync my-crate                # Bidirectional sync
  cargo rail sync my-crate --to-remote    # Push monorepo -> split repo
  cargo rail sync my-crate --from-remote  # Pull split repo -> monorepo (PR branch)
  cargo rail sync --all                   # Sync all configured crates";

const RELEASE_HELP: &str = "\
Examples:
  cargo rail release my-crate --check     # Preview release plan
  cargo rail release my-crate             # Release (patch bump)
  cargo rail release my-crate --bump minor
  cargo rail release --all --bump patch   # Release all crates
  cargo rail release my-crate --skip-publish  # Tag only, no crates.io";

/// Available subcommands
#[derive(Subcommand)]
pub enum Commands {
  /// Show which crates are affected by changes
  #[command(after_long_help = AFFECTED_HELP)]
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
  #[command(after_long_help = TEST_HELP)]
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
  #[command(after_long_help = UNIFY_HELP)]
  Unify {
    /// Action: 'undo' to restore from backup (use with --list to see backups)
    action: Option<String>,
    /// Dry-run mode: preview changes without modifying files
    #[arg(long, short = 'c')]
    check: bool,
    /// Output format [text, json]
    #[arg(long, short = 'f', default_value = "text")]
    format: String,
    /// Exclude dependencies from unification (comma-separated)
    #[arg(long, value_delimiter = ',')]
    exclude: Vec<String>,
    /// Force include specific dependencies (comma-separated)
    #[arg(long, value_delimiter = ',')]
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
  #[command(after_long_help = SPLIT_HELP)]
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
  #[command(after_long_help = SYNC_HELP)]
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
  #[command(after_long_help = RELEASE_HELP)]
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
