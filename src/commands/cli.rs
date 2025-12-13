//! CLI argument definitions for cargo-rail
//!
//! This module defines all CLI structures using clap. These are used by main.rs
//! and the dispatch logic in commands/mod.rs.

use super::common::OutputFormat;
use crate::sync::ConflictStrategy;
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

  /// Output in JSON format (shorthand for -f json)
  #[arg(long, global = true)]
  pub json: bool,

  /// The subcommand to execute
  #[command(subcommand)]
  pub command: Commands,
}

const AFFECTED_HELP: &str = "\
Examples:
  cargo rail affected                     # Changes since origin/main
  cargo rail affected --since HEAD~5      # Changes in last 5 commits
  cargo rail affected --from abc --to def # Changes between two SHAs
  cargo rail affected --explain           # Show why each crate is affected
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
  cargo rail unify --show-diff            # Show manifest changes
  cargo rail unify undo                   # Restore from backup
  cargo rail unify undo --list            # List available backups";

const SPLIT_HELP: &str = "\
Examples:
  cargo rail split init my-crate          # Configure split for my-crate
  cargo rail split init my-crate --check  # Preview generated config
  cargo rail split run my-crate --check   # Preview the split
  cargo rail split run my-crate           # Execute the split
  cargo rail split run --all              # Split all configured crates";

const SYNC_HELP: &str = "\
Examples:
  cargo rail sync my-crate                # Bidirectional sync
  cargo rail sync my-crate --to-remote    # Push monorepo -> split repo
  cargo rail sync my-crate --from-remote  # Pull split repo -> monorepo (PR branch)
  cargo rail sync --all                   # Sync all configured crates";

const RELEASE_HELP: &str = "\
Examples:
  cargo rail release init my-crate              # Configure release for my-crate
  cargo rail release check my-crate             # Validate release readiness
  cargo rail release check my-crate --extended  # Run extended checks (dry-run, MSRV)
  cargo rail release run my-crate --check       # Preview release plan
  cargo rail release run my-crate               # Release (patch bump)
  cargo rail release run my-crate --bump minor
  cargo rail release run my-crate --bump prerelease  # 1.0.0 -> 1.0.0-rc.1
  cargo rail release run my-crate --bump release     # 1.0.0-rc.2 -> 1.0.0
  cargo rail release run --all --bump patch     # Release all crates
  cargo rail release run my-crate --skip-publish  # Tag only, no crates.io";

const INIT_HELP: &str = "\
Examples:
  cargo rail init                       # Generate .config/rail.toml
  cargo rail init --check               # Preview generated config
  cargo rail init -o rail.toml          # Custom output path
  cargo rail init --force               # Overwrite existing config";

const CLEAN_HELP: &str = "\
Examples:
  cargo rail clean                      # Clean all artifacts
  cargo rail clean --cache              # Clean metadata cache only
  cargo rail clean --backups            # Prune old backups
  cargo rail clean --reports            # Clean generated reports
  cargo rail clean --check              # Preview what would be cleaned";

const CONFIG_HELP: &str = "\
Examples:
  cargo rail config validate            # Validate rail.toml
  cargo rail config validate -f json    # JSON output for CI
  cargo rail config sync --check        # Preview config updates
  cargo rail config sync                # Add missing fields, sync targets";

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
    /// Output format
    #[arg(long, short = 'f', default_value_t, value_enum)]
    format: OutputFormat,
    /// Show all workspace crates (ignore changes)
    #[arg(long, short = 'a')]
    all: bool,
    /// Write output to file (e.g., $GITHUB_OUTPUT)
    #[arg(long, short = 'o')]
    output: Option<PathBuf>,
    /// Explain why each crate is affected
    #[arg(long)]
    explain: bool,
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
    /// Subcommand (undo)
    #[command(subcommand)]
    command: Option<UnifyCommand>,
    /// Dry-run mode: preview changes without modifying files
    #[arg(long, short = 'c')]
    check: bool,
    /// Output format
    #[arg(long, short = 'f', default_value_t, value_enum)]
    format: OutputFormat,
    /// Create backups of all modified files
    #[arg(long)]
    backup: bool,
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
  #[command(after_long_help = INIT_HELP)]
  Init {
    /// Output path for rail.toml
    #[arg(long, short, default_value = ".config/rail.toml")]
    output: String,
    /// Overwrite existing configuration
    #[arg(long)]
    force: bool,
    /// Dry-run mode: preview generated config without writing
    #[arg(long, short = 'c')]
    check: bool,
  },

  /// Split a crate to a standalone repository with git history
  #[command(after_long_help = SPLIT_HELP)]
  Split {
    /// Split subcommand
    #[command(subcommand)]
    command: SplitCommand,
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
    /// Conflict resolution strategy
    #[arg(long, default_value_t, value_enum)]
    strategy: ConflictStrategy,
    /// Dry-run mode: preview changes without executing
    #[arg(long, short = 'c')]
    check: bool,
    /// Output format
    #[arg(long, short = 'f', default_value_t, value_enum)]
    format: OutputFormat,
  },

  /// Publish releases (version bump, changelog, tag, publish)
  #[command(after_long_help = RELEASE_HELP)]
  Release {
    /// Release subcommand
    #[command(subcommand)]
    command: ReleaseCommand,
  },

  /// Clean generated artifacts (cache, backups, reports)
  #[command(after_long_help = CLEAN_HELP)]
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
    /// Output format
    #[arg(long, short = 'f', default_value_t, value_enum)]
    format: OutputFormat,
  },

  /// Configuration management
  #[command(name = "config")]
  #[command(after_long_help = CONFIG_HELP)]
  Config {
    /// Subcommand
    #[command(subcommand)]
    command: ConfigCommand,
  },
}

/// Subcommands for `cargo rail config`
#[derive(Subcommand)]
pub enum ConfigCommand {
  /// Validate the configuration file
  ///
  /// Checks for parse errors, unknown keys, and semantic issues.
  /// By default, unknown keys warn locally but error in CI environments
  /// (detected via CI, GITHUB_ACTIONS, GITLAB_CI, or CIRCLECI env vars).
  Validate {
    /// Output format
    #[arg(long, short = 'f', default_value_t, value_enum)]
    format: OutputFormat,
    /// Treat warnings as errors (auto-enabled in CI)
    #[arg(long, conflicts_with = "no_strict")]
    strict: bool,
    /// Never treat warnings as errors (overrides CI auto-detection)
    #[arg(long, conflicts_with = "strict")]
    no_strict: bool,
  },
  /// Sync configuration: add missing fields and update targets
  ///
  /// Scans the workspace for target triples and adds any missing config
  /// fields with their default values. Preserves all existing settings,
  /// comments, and formatting.
  ///
  /// Use this after upgrading cargo-rail to get new configuration options.
  Sync {
    /// Preview changes without modifying rail.toml
    #[arg(long, short = 'c')]
    check: bool,
    /// Output format
    #[arg(long, short = 'f', default_value_t, value_enum)]
    format: OutputFormat,
  },
}

/// Subcommands for `cargo rail unify`
#[derive(Subcommand)]
pub enum UnifyCommand {
  /// Restore manifests from a previous backup
  Undo {
    /// List available backups instead of restoring
    #[arg(long)]
    list: bool,
    /// Specific backup ID to restore (defaults to most recent)
    #[arg(long = "backup-id")]
    backup_id: Option<String>,
  },
}

/// Subcommands for `cargo rail split`
#[derive(Subcommand)]
pub enum SplitCommand {
  /// Configure split for crate(s)
  Init {
    /// Crate name(s) to configure
    crate_names: Vec<String>,
    /// Preview generated config without writing
    #[arg(long, short = 'c')]
    check: bool,
  },
  /// Execute split operation
  Run {
    /// Crate name to split (mutually exclusive with --all)
    #[arg(conflicts_with = "all")]
    crate_name: Option<String>,
    /// Split all configured crates
    #[arg(short, long)]
    all: bool,
    /// Override remote repository
    #[arg(long)]
    remote: Option<String>,
    /// Dry-run mode: preview changes
    #[arg(long, short = 'c')]
    check: bool,
    /// Output format
    #[arg(long, short = 'f', default_value_t, value_enum)]
    format: OutputFormat,
  },
}

/// Subcommands for `cargo rail release`
#[derive(Subcommand)]
pub enum ReleaseCommand {
  /// Configure release settings
  Init {
    /// Crate name(s) to configure (optional)
    crate_names: Vec<String>,
    /// Preview generated config without writing
    #[arg(long, short = 'c')]
    check: bool,
  },
  /// Execute release (plan or publish)
  Run {
    /// Crate name(s) to release (mutually exclusive with --all)
    #[arg(conflicts_with = "all")]
    crate_names: Vec<String>,
    /// Release all workspace crates
    #[arg(short, long)]
    all: bool,
    /// Version bump [major, minor, patch, prerelease, release, or "x.y.z"]
    #[arg(long, default_value = "patch")]
    bump: String,
    /// Dry-run mode: preview release plan
    #[arg(long, short = 'c')]
    check: bool,
    /// Skip publishing to crates.io
    #[arg(long)]
    skip_publish: bool,
    /// Skip git tag creation
    #[arg(long)]
    skip_tag: bool,
    /// Skip confirmation prompts and allow non-default branch
    #[arg(short = 'y', long)]
    yes: bool,
    /// Output format
    #[arg(long, short = 'f', default_value_t, value_enum)]
    format: OutputFormat,
  },
  /// Validate release readiness
  Check {
    /// Crate name(s) to check (mutually exclusive with --all)
    #[arg(conflicts_with = "all")]
    crate_names: Vec<String>,
    /// Check all workspace crates (mutually exclusive with crate names)
    #[arg(short, long)]
    all: bool,
    /// Run extended validation (cargo publish --dry-run, MSRV check)
    #[arg(long, short = 'e')]
    extended: bool,
    /// Output format
    #[arg(long, short = 'f', default_value_t, value_enum)]
    format: OutputFormat,
  },
}

fn get_styles() -> clap::builder::Styles {
  clap::builder::Styles::styled()
}

impl Commands {
  /// Check if this command uses JSON-like output format
  ///
  /// Returns true for any format that produces structured output.
  /// Used for early JSON mode detection to suppress progress messages.
  pub fn is_json_format(&self) -> bool {
    match self {
      Commands::Affected { format, .. }
      | Commands::Unify { format, .. }
      | Commands::Sync { format, .. }
      | Commands::Clean { format, .. } => format.is_json_like(),
      Commands::Split { command } => match command {
        SplitCommand::Init { .. } => false,
        SplitCommand::Run { format, .. } => format.is_json_like(),
      },
      Commands::Release { command } => match command {
        ReleaseCommand::Init { .. } => false,
        ReleaseCommand::Run { format, .. } | ReleaseCommand::Check { format, .. } => format.is_json_like(),
      },
      Commands::Config { command } => match command {
        ConfigCommand::Validate { format, .. } | ConfigCommand::Sync { format, .. } => format.is_json_like(),
      },
      _ => false,
    }
  }

  /// Apply global --json flag by overriding format to Json
  pub fn apply_json_override(&mut self) {
    match self {
      Commands::Affected { format, .. }
      | Commands::Unify { format, .. }
      | Commands::Sync { format, .. }
      | Commands::Clean { format, .. } => *format = OutputFormat::Json,
      Commands::Split {
        command: SplitCommand::Run { format, .. },
      } => *format = OutputFormat::Json,
      Commands::Split { .. } => {}
      Commands::Release {
        command: ReleaseCommand::Run { format, .. } | ReleaseCommand::Check { format, .. },
      } => *format = OutputFormat::Json,
      Commands::Release { .. } => {}
      Commands::Config { command } => match command {
        ConfigCommand::Validate { format, .. } | ConfigCommand::Sync { format, .. } => *format = OutputFormat::Json,
      },
      _ => {}
    }
  }
}
