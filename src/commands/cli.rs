//! CLI argument definitions for cargo-rail.
//!
//! This module defines all CLI structures using clap. These are internal
//! and used by main.rs and the dispatch logic in commands/mod.rs.
//!
//! **Note:** This is not part of the stable public API.

use super::common::{
    ChangeOutputFormat, PlanOutputFormat, SplitOutputFormat, SurfaceOutputFormat, TextJsonOutputFormat,
    UnifyOutputFormat,
};
use crate::sync::ConflictStrategy;
use clap::{Args, CommandFactory, Parser, Subcommand, error::ErrorKind};
use clap_complete::Shell;
use std::path::PathBuf;

const MAIN_HELP: &str = "\
The Rust workspace engine.

Cargo-Rail turns Cargo's resolved workspace model and an exact source snapshot into affected CI, verified compiler reuse,
dependency coherence, exact-SHA releases, and crate synchronization.

Quick start:
  cargo rail plan --merge-base --explain          # Inspect affected work and reasoning
  cargo rail plan --merge-base -f github           # Export typed CI scope
  cargo rail cache setup                           # Enable transparent compiler reuse
  cargo rail unify --check --explain              # Inspect dependency changes (exit 1 when pending)

Docs: https://github.com/loadingalias/cargo-rail";

/// Root CLI wrapper for cargo subcommand integration
///
/// This wrapper allows cargo-rail to be invoked as `cargo rail <subcommand>`.
#[derive(Parser)]
#[command(name = "cargo")]
#[command(bin_name = "cargo")]
#[command(styles = get_styles())]
pub enum CargoCli {
    /// The Rust workspace engine
    #[command(about = "The Rust workspace engine", long_about = MAIN_HELP)]
    Rail(RailCli),
}

/// Main CLI structure for cargo-rail
///
/// Contains global options and the subcommand to execute.
#[derive(Parser)]
#[command(name = "rail")]
#[command(version)]
#[command(about = "The Rust workspace engine")]
#[command(long_about = MAIN_HELP)]
#[command(propagate_version = true)]
#[command(styles = get_styles())]
pub struct RailCli {
    /// Suppress progress messages (for CI/automation)
    #[arg(long, short, global = true)]
    pub quiet: bool,

    /// Output as JSON where supported; rejected otherwise (shorthand for -f json)
    #[arg(long, global = true)]
    pub json: bool,

    /// Path to rail.toml config file (bypass search order)
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Workspace root directory (default: current directory)
    #[arg(long, global = true, value_name = "PATH")]
    pub workspace_root: Option<PathBuf>,

    /// Write diagnostic performance counters to this JSON file
    #[arg(long, global = true, value_name = "PATH", hide = true)]
    pub diagnostics_file: Option<PathBuf>,

    /// The subcommand to execute
    #[command(subcommand)]
    pub command: Commands,
}

const PLAN_HELP: &str = "\
Examples:
  cargo rail plan                           # Changes since default branch
  cargo rail plan --merge-base              # Changes since branch point (CI recommended)
  cargo rail plan --confidence-profile strict  # Conservative planner profile
  cargo rail plan --since HEAD~5            # Changes in last 5 commits
  cargo rail plan --from abc --to def       # Changes between two SHAs
  cargo rail plan --explain                 # Show concise proof chain
  cargo rail plan --schema                  # Print the versioned JSON Schema
  cargo rail plan -f json                   # Full machine-readable contract
  cargo rail plan -f github                 # Compact GitHub Actions key=value output
  cargo rail plan -f github-debug           # GitHub Actions output plus plan_json";

const SURFACE_HELP: &str = "\
Examples:
  cargo rail surface --check --explain      # Inspect complete Rust reachability
  cargo rail surface --check -f json        # Emit the versioned machine contract
  cargo rail surface --fix --dry-run --explain  # Preview exact visibility edits
  cargo rail surface --fix --backup         # Apply verified edits with recovery evidence
  cargo rail surface --schema               # Print the versioned JSON Schema";

const UNIFY_HELP: &str = "\
Examples:
  cargo rail unify --check                # Check for pending changes (exit 1)
  cargo rail unify --check --explain      # Show why each decision was made
  cargo rail unify --check -f json -o out.json  # Write JSON output to file
  cargo rail unify                        # Apply changes
  cargo rail unify --backup               # Apply with backup
  cargo rail unify --show-diff            # Show manifest changes
  cargo rail unify undo                   # Restore from backup
  cargo rail unify undo --list            # List available backups";

const SPLIT_HELP: &str = "\
This is an advanced feature for extracting crates to standalone repositories
while preserving git history. Most teams should start with 'plan', 'run',
and 'unify' before using split/sync.

Examples:
  cargo rail split init my-crate          # Configure split for my-crate
  cargo rail split init my-crate --dry-run  # Preview generated config
  cargo rail split run my-crate --check   # Check for a pending split (exit 1)
  cargo rail split run my-crate           # Execute the split
  cargo rail split run my-crate --yes     # Non-interactive apply confirmation
  cargo rail split run --all              # Split all configured crates";

const SYNC_HELP: &str = "\
This is an advanced feature for bidirectional sync between monorepo and split
repositories. Requires 'split' to be configured first.

Examples:
  cargo rail sync my-crate                # Bidirectional sync
  cargo rail sync my-crate --to-remote    # Push monorepo -> split repo
  cargo rail sync my-crate --from-remote  # Pull split repo -> monorepo (PR branch)
  cargo rail sync my-crate --to-remote --yes  # Non-interactive apply confirmation
  cargo rail sync --all                   # Sync all configured crates";

const RELEASE_HELP: &str = "\
Examples:
  cargo rail release init my-crate              # Configure release for my-crate
  cargo rail release init my-crate --dry-run    # Preview generated config
  cargo rail release check my-crate             # Validate release readiness
  cargo rail release check my-crate --extended  # Run extended checks (dry-run, MSRV)
  cargo rail release run my-crate --check       # Check for a pending release (exit 1)
  cargo rail release run my-crate               # Release from reviewed change intent
  cargo rail release run my-crate --include-dependents  # Release selected crate plus dependent closure
  cargo rail release run my-crate --yes         # Non-interactive apply confirmation
  cargo rail release run my-crate --bump auto   # Infer each bump from the configured release source
  cargo rail release run --all --bump auto --pr # Open a release PR with bumps/changelogs only
  cargo rail release finalize --all             # Tag/publish after the release PR merges
  cargo rail release run my-crate --bump minor
  cargo rail release run my-crate --bump prerelease  # 1.0.0 -> 1.0.0-rc.1
  cargo rail release run my-crate --bump release     # 1.0.0-rc.2 -> 1.0.0
  cargo rail release run --all --bump patch     # Release all crates
  cargo rail release run my-crate --skip-publish  # Tag only, no crates.io";

const CHANGE_HELP: &str = "\
Examples:
  cargo rail change add rail-core --bump minor --message \"Added auto bump planning\"
  cargo rail change add rail-core rail-cli --bump patch --message \"Fixed release notes\"
  cargo rail change add rail-core --bump patch --name fix-parser
  cargo rail change status
  cargo rail change status --format json
  cargo rail change check --merge-base --required
  cargo rail change check --since origin/main --format json

Omit --message in an interactive terminal to author in $VISUAL or $EDITOR.
Change files are consumed (deleted in the release commit) when released.
Consumption is all-or-nothing: a release plan that covers only some of a
file's crates is rejected so no pending intent is ever lost.";

const INIT_HELP: &str = "\
Examples:
  cargo rail init                       # Generate .config/rail.toml
  cargo rail init --dry-run             # Preview generated config
  cargo rail init -o rail.toml          # Custom output path
  cargo rail init --force               # Overwrite existing config";

const CLEAN_HELP: &str = "\
Examples:
  cargo rail clean                      # Clean all artifacts
  cargo rail clean --cache              # Clean validated local and workspace cache state
  cargo rail clean --backups            # Prune old backups
  cargo rail clean --reports            # Clean generated reports
  cargo rail clean --check              # Check for pending cleanup (exit 1)";

const CACHE_HELP: &str = "\
Examples:
  cargo rail cache setup --check                  # Preview transparent compiler reuse setup
  cargo rail cache setup                          # Install or repair the Cargo wrapper
  cargo rail cache status                         # Inspect workspace and shared local cache state
  cargo rail cache status --scope local -f json  # Inspect the shared local CAS only
  cargo rail cache clean --scope workspace --check  # Preview workspace cache reclamation
  cargo rail cache clean --scope local            # Remove the validated cross-workspace CAS
  cargo rail cache remove --check                 # Preview exact setup-state removal
  cargo rail cache remove                         # Remove setup state but preserve the CAS";

const CONFIG_HELP: &str = "\
Examples:
  cargo rail config locate              # Show which config file is active
  cargo rail config print               # Show effective config with defaults
  cargo rail config validate            # Validate rail.toml
  cargo rail config validate -f json    # JSON output for CI
  cargo rail config explain             # Explain effective values and sources
  cargo rail config migrate --check     # Check for pending semantic migrations
  cargo rail config migrate             # Apply explicit semantic migrations";

const HASH_HELP: &str = "\
Examples:
  cargo rail hash                          # Portable identity of the current plan
  cargo rail hash --merge-base             # Identity for the merge-base comparison
  cargo rail hash -f json                  # Structured identity metadata
  cargo rail diff-hash plan-a.json plan-b.json
  cargo rail diff-hash plan-a.json plan-b.json -f json";

const GRAPH_HELP: &str = "\
Examples:
  cargo rail graph                             # Planner reasoning graph (json)
  cargo rail graph --merge-base                # Graph against merge-base comparison
  cargo rail graph --dot                       # GraphViz DOT output
  cargo rail graph --since HEAD~3 -o graph.dot # Write graph output to file";

const COMPLETIONS_HELP: &str = "\
Examples:
  cargo rail completions bash           # Output bash completions
  cargo rail completions zsh            # Output zsh completions
  cargo rail completions fish           # Output fish completions
  cargo rail completions powershell     # Output PowerShell completions

Installation:
  # Bash (~/.bashrc)
  eval \"$(cargo rail completions bash)\"

  # Zsh (~/.zshrc)
  eval \"$(cargo rail completions zsh)\"

  # Fish (~/.config/fish/config.fish)
  cargo rail completions fish | source

  # PowerShell
  cargo rail completions powershell | Out-String | Invoke-Expression";

/// Available subcommands
#[derive(Subcommand)]
pub enum Commands {
    /// Inspect native compiler-cache capability
    Doctor {
        /// Diagnostic to run
        #[command(subcommand)]
        command: DoctorCommand,
    },

    /// Inspect or reclaim explicitly scoped cache state
    #[command(after_long_help = CACHE_HELP)]
    Cache {
        /// Cache operation
        #[command(subcommand)]
        command: CacheCommand,
    },

    /// Build a deterministic file-first change plan
    #[command(after_long_help = PLAN_HELP)]
    Plan {
        /// Git ref to compare against (auto-detects default branch)
        #[arg(long)]
        since: Option<String>,
        /// Start ref (for SHA pair mode)
        #[arg(long, conflicts_with = "since", requires = "to")]
        from: Option<String>,
        /// End ref (for SHA pair mode)
        #[arg(long, requires = "from")]
        to: Option<String>,
        /// Use merge-base with default branch (better for feature branches)
        #[arg(long, conflicts_with_all = ["since", "from", "to"])]
        merge_base: bool,
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: PlanOutputFormat,
        /// Write output to file (overwrites existing content)
        #[arg(long, short = 'o', value_name = "PATH")]
        output: Option<PathBuf>,
        /// Show concise human reasoning chain
        #[arg(long)]
        explain: bool,
        /// Planner confidence profile override (strict|balanced|fast)
        #[arg(long, value_name = "PROFILE", value_parser = ["strict", "balanced", "fast"])]
        confidence_profile: Option<String>,
        /// Print the versioned planner JSON Schema and exit
        #[arg(long, conflicts_with_all = ["since", "from", "to", "merge_base", "output", "explain", "confidence_profile"])]
        schema: bool,
    },

    /// Analyze complete Rust declaration reachability and visibility
    #[command(after_long_help = SURFACE_HELP)]
    Surface {
        /// Check for enabled findings without modifying source (exit 1 when found)
        #[arg(long, conflicts_with = "fix")]
        check: bool,
        /// Apply exact visibility reductions
        #[arg(long, conflicts_with = "check")]
        fix: bool,
        /// Render the exact mutation plan without writing
        #[arg(long, requires = "fix", conflicts_with = "backup")]
        dry_run: bool,
        /// Create a bounded backup before applying visibility edits
        #[arg(long, requires = "fix")]
        backup: bool,
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: SurfaceOutputFormat,
        /// Write output to file (overwrites existing content)
        #[arg(long, short = 'o', value_name = "PATH")]
        output: Option<PathBuf>,
        /// Show the reason chain for every finding
        #[arg(long)]
        explain: bool,
        /// Print the versioned surface JSON Schema and exit
        #[arg(long, conflicts_with_all = ["check", "fix", "dry_run", "backup", "output", "explain"])]
        schema: bool,
    },

    /// Analyze and repair workspace dependency coherence
    #[command(after_long_help = UNIFY_HELP)]
    Unify {
        /// Subcommand (doctor or undo)
        #[command(subcommand)]
        command: Option<UnifyCommand>,
        /// Check for pending changes without modifying files (exit 1 when pending)
        #[arg(long, short = 'c')]
        check: bool,
        /// Apply from a previously generated mutation plan file
        #[arg(long, value_name = "PATH", conflicts_with = "check")]
        plan: Option<PathBuf>,
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: UnifyOutputFormat,
        /// Create backups of all modified files
        #[arg(long)]
        backup: bool,
        /// Skip generating the unify report
        #[arg(long)]
        skip_report: bool,
        /// Custom path for the unify report (default: target/cargo-rail/unify-report.md)
        #[arg(long)]
        report_path: Option<PathBuf>,
        /// Write output to file (overwrites existing content)
        #[arg(long, short = 'o', value_name = "PATH", requires = "check")]
        output: Option<PathBuf>,
        /// Show diff of changes to each manifest
        #[arg(long)]
        show_diff: bool,
        /// Explain why each decision was made
        #[arg(long)]
        explain: bool,
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
        /// Preview generated config without writing
        #[arg(long)]
        dry_run: bool,
    },

    /// (Advanced) Split a crate to a standalone repository with git history
    #[command(after_long_help = SPLIT_HELP)]
    Split {
        /// Split subcommand
        #[command(subcommand)]
        command: SplitCommand,
    },

    /// (Advanced) Sync changes between monorepo and split repos
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
        /// Check for pending changes without executing (exit 1 when pending)
        #[arg(long, short = 'c')]
        check: bool,
        /// Apply from a previously generated mutation plan file
        #[arg(long, value_name = "PATH", conflicts_with = "check")]
        plan: Option<PathBuf>,
        /// Resume a manually resolved sync conflict receipt
        #[arg(
      long,
      value_name = "RECEIPT",
      conflicts_with_all = ["crate_name", "all", "remote", "from_remote", "to_remote", "check", "plan"]
    )]
        resume: Option<PathBuf>,
        /// Allow running on dirty worktree (uncommitted changes)
        #[arg(long)]
        allow_dirty: bool,
        /// Skip confirmation prompts (for CI/automation)
        #[arg(short = 'y', long)]
        yes: bool,
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },

    /// Publish releases (version bump, changelog, tag, publish)
    #[command(after_long_help = RELEASE_HELP)]
    Release {
        /// Release subcommand
        #[command(subcommand)]
        command: ReleaseCommand,
    },

    /// Manage pending release intent files
    #[command(after_long_help = CHANGE_HELP)]
    Change {
        /// Change subcommand
        #[command(subcommand)]
        command: ChangeCommand,
    },

    /// Clean generated artifacts (cache, backups, reports)
    #[command(after_long_help = CLEAN_HELP)]
    Clean {
        /// Clean validated local and workspace cache state
        #[arg(long)]
        cache: bool,
        /// Prune old backups
        #[arg(long)]
        backups: bool,
        /// Clean generated reports
        #[arg(long)]
        reports: bool,
        /// Check for pending cleanup without deleting files (exit 1 when pending)
        #[arg(long, short = 'c')]
        check: bool,
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },

    /// Configuration management
    #[command(name = "config")]
    #[command(after_long_help = CONFIG_HELP)]
    Config {
        /// Subcommand
        #[command(subcommand)]
        command: ConfigCommand,
    },

    /// Compute a portable planner identity (not a cache key)
    #[command(after_long_help = HASH_HELP)]
    Hash {
        /// Git ref to compare against (auto-detects default branch)
        #[arg(long)]
        since: Option<String>,
        /// Start ref (for SHA pair mode)
        #[arg(long, conflicts_with = "since", requires = "to")]
        from: Option<String>,
        /// End ref (for SHA pair mode)
        #[arg(long, requires = "from")]
        to: Option<String>,
        /// Use merge-base with default branch (better for feature branches)
        #[arg(long, conflicts_with_all = ["since", "from", "to"])]
        merge_base: bool,
        /// Planner confidence profile override (strict|balanced|fast)
        #[arg(long, value_name = "PROFILE", value_parser = ["strict", "balanced", "fast"])]
        confidence_profile: Option<String>,
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },

    /// Explain why two portable planner identities differ
    #[command(after_long_help = HASH_HELP)]
    DiffHash {
        /// First planner JSON path
        a: PathBuf,
        /// Second planner JSON path
        b: PathBuf,
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },

    /// Planner reasoning graph for explainability
    #[command(after_long_help = GRAPH_HELP)]
    Graph {
        /// Git ref to compare against (auto-detects default branch)
        #[arg(long)]
        since: Option<String>,
        /// Start ref (for SHA pair mode)
        #[arg(long, conflicts_with = "since", requires = "to")]
        from: Option<String>,
        /// End ref (for SHA pair mode)
        #[arg(long, requires = "from")]
        to: Option<String>,
        /// Use merge-base with default branch (better for feature branches)
        #[arg(long, conflicts_with_all = ["since", "from", "to"])]
        merge_base: bool,
        /// Planner confidence profile override (strict|balanced|fast)
        #[arg(long, value_name = "PROFILE", value_parser = ["strict", "balanced", "fast"])]
        confidence_profile: Option<String>,
        /// Output GraphViz DOT instead of JSON
        #[arg(long)]
        dot: bool,
        /// Write output to file (overwrites existing content)
        #[arg(long, short = 'o', value_name = "PATH")]
        output: Option<PathBuf>,
    },

    /// Generate shell completions
    #[command(after_long_help = COMPLETIONS_HELP)]
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum, value_name = "SHELL")]
        shell: Shell,
    },
}

impl Commands {
    /// Return whether this command consumes one authoritative workspace snapshot.
    #[doc(hidden)]
    pub fn requires_workspace_snapshot(&self) -> bool {
        match self {
            Self::Doctor { .. }
            | Self::Unify { .. }
            | Self::Split { .. }
            | Self::Sync { .. }
            | Self::Surface { schema: false, .. }
            | Self::Release { .. }
            | Self::Change { .. } => true,
            Self::Plan { from, to, schema, .. } => !(*schema || from.is_some() && to.is_some()),
            Self::Hash { from, to, .. } | Self::Graph { from, to, .. } => !(from.is_some() && to.is_some()),
            _ => false,
        }
    }

    /// Return whether dispatch needs source captured before metadata loading.
    #[doc(hidden)]
    pub fn requires_worktree_source_capture(&self) -> bool {
        match self {
            Self::Doctor { .. } | Self::Surface { .. } => false,
            Self::Plan { from, to, schema, .. } => !(*schema || from.is_some() && to.is_some()),
            Self::Hash { from, to, .. } | Self::Graph { from, to, .. } => !(from.is_some() && to.is_some()),
            _ => false,
        }
    }
}

/// Subcommands for `cargo rail doctor`.
#[derive(Subcommand)]
pub enum DoctorCommand {
    /// Inspect the exact native-cache compiler identity
    NativeCache {
        /// Report format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
}

/// Cache ownership scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum CacheScope {
    /// Reconstructible cache state inside the selected workspace.
    Workspace,
    /// The validated user-wide CAS shared by local workspaces.
    Local,
    /// Both workspace state and the shared local CAS.
    All,
}

impl CacheScope {
    pub(crate) const fn includes_workspace(self) -> bool {
        matches!(self, Self::Workspace | Self::All)
    }

    pub(crate) const fn includes_local(self) -> bool {
        matches!(self, Self::Local | Self::All)
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Local => "local",
            Self::All => "all",
        }
    }
}

/// Arguments that install or repair transparent verified compiler reuse.
#[derive(Args)]
pub struct CacheSetupArgs {
    /// Local cache base directory (defaults to Cargo home).
    #[arg(long, value_name = "PATH")]
    pub local_dir: Option<PathBuf>,
    /// Positive binary byte size such as 10GiB.
    #[arg(long, value_name = "SIZE", value_parser = parse_cache_size)]
    pub max_size: Option<u64>,
    /// Machine-owned remote cache URL to persist with this installation.
    #[arg(long, value_name = "URL", conflicts_with = "local_only")]
    pub remote: Option<String>,
    /// Maximum remote authority; explicit selection defaults to read-write.
    #[arg(long, value_name = "MODE", value_parser = ["read", "read-write"], requires = "remote")]
    pub remote_mode: Option<String>,
    /// Additional reviewed compiler environment name admitted to L2 identity.
    #[arg(long = "remote-environment", value_name = "NAME", requires = "remote")]
    pub remote_environment: Vec<String>,
    /// Remove persisted remote activation while preserving local reuse.
    #[arg(long, conflicts_with_all = ["remote", "remote_mode", "remote_environment"])]
    pub local_only: bool,
    /// Enable the same-host distributed protocol qualification path.
    #[arg(long, conflicts_with = "distributed_endpoint")]
    pub distributed_local: bool,
    /// Mutually authenticated direct worker socket address.
    #[arg(
        long,
        value_name = "IP:PORT",
        conflicts_with = "distributed_local",
        requires_all = [
            "distributed_server_name",
            "distributed_capability",
            "distributed_authority",
            "distributed_client_certificate",
            "distributed_client_private_key"
        ]
    )]
    pub distributed_endpoint: Option<String>,
    /// TLS DNS name required from the distributed worker certificate.
    #[arg(long, value_name = "NAME", requires = "distributed_endpoint")]
    pub distributed_server_name: Option<String>,
    /// Exact capability identity advertised by the selected worker.
    #[arg(long, value_name = "IDENTITY", requires = "distributed_endpoint")]
    pub distributed_capability: Option<String>,
    /// PEM certificate authority for the distributed worker.
    #[arg(long, value_name = "PATH", requires = "distributed_endpoint")]
    pub distributed_authority: Option<PathBuf>,
    /// PEM client certificate presented to the distributed worker.
    #[arg(long, value_name = "PATH", requires = "distributed_endpoint")]
    pub distributed_client_certificate: Option<PathBuf>,
    /// Private PEM key for the distributed client certificate.
    #[arg(long, value_name = "PATH", requires = "distributed_endpoint")]
    pub distributed_client_private_key: Option<PathBuf>,
    /// Placement policy for an mTLS worker. Qualification samples every eligible miss;
    /// automatic placement requires retained evidence of a critical-path win.
    #[arg(
        long,
        value_name = "MODE",
        value_parser = ["automatic", "qualification"],
        conflicts_with = "distributed_local"
    )]
    pub distributed_policy: Option<String>,
    /// Preview exact Cargo configuration and private-state changes.
    #[arg(long, short = 'c')]
    pub check: bool,
    /// Report format.
    #[arg(long, short = 'f', default_value_t, value_enum)]
    pub format: TextJsonOutputFormat,
}

/// Subcommands for `cargo rail cache`.
#[derive(Subcommand)]
pub enum CacheCommand {
    /// Install or repair transparent verified compiler reuse.
    Setup(Box<CacheSetupArgs>),
    /// Validate and normalize one machine-owned remote cache URL without network access.
    Normalize {
        /// AWS S3, Azure Blob Storage, or Cloudflare R2 URL.
        #[arg(value_name = "URL")]
        url: String,
        /// Maximum authority; explicit selection defaults to read-write.
        #[arg(long, value_name = "MODE", value_parser = ["read", "read-write"])]
        mode: Option<String>,
        /// Additional reviewed compiler environment name admitted to L2 identity.
        #[arg(long = "environment", value_name = "NAME")]
        environment: Vec<String>,
        /// Report format.
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
    /// Report exact bytes, counts, bounds, leases, and ownership scope.
    Status {
        /// Cache scope to inspect.
        #[arg(long, value_enum, default_value = "all")]
        scope: CacheScope,
        /// Report format.
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
    /// Reclaim one explicitly selected cache scope.
    Clean {
        /// Cache scope to reclaim; required to prevent accidental cross-workspace deletion.
        #[arg(long, value_enum)]
        scope: CacheScope,
        /// Preview exact bytes and paths without deleting them.
        #[arg(long, short = 'c')]
        check: bool,
        /// Report format.
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
    /// Remove only transparent compiler-cache state owned by the setup receipt.
    Remove {
        /// Preview exact Cargo configuration and private-state changes.
        #[arg(long, short = 'c')]
        check: bool,
        /// Report format.
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
}

fn parse_cache_size(value: &str) -> Result<u64, String> {
    let split = value
        .find(|character: char| !character.is_ascii_digit())
        .ok_or_else(|| "cache size requires an explicit unit: B, KiB, MiB, GiB, or TiB".to_string())?;
    let (number, unit) = value.split_at(split);
    if number.is_empty() || number.starts_with('0') && number != "0" {
        return Err("cache size must be a canonical positive integer".to_string());
    }
    let number = number
        .parse::<u64>()
        .map_err(|_| "cache size integer is invalid or overflowing".to_string())?;
    let multiplier = match unit {
        "B" => 1_u64,
        "KiB" => 1024,
        "MiB" => 1024 * 1024,
        "GiB" => 1024 * 1024 * 1024,
        "TiB" => 1024 * 1024 * 1024 * 1024,
        _ => return Err("cache size unit must be B, KiB, MiB, GiB, or TiB".to_string()),
    };
    let bytes = number
        .checked_mul(multiplier)
        .filter(|bytes| *bytes > 0)
        .ok_or_else(|| "cache size must be positive and fit in u64".to_string())?;
    if bytes > usize::MAX as u64 {
        return Err("cache size exceeds this platform's supported limit".to_string());
    }
    Ok(bytes)
}

/// Subcommands for `cargo rail config`
#[derive(Subcommand)]
pub enum ConfigCommand {
    /// Print the path to the active config file
    ///
    /// Shows which config file is being used. Searches in order:
    /// rail.toml, .rail.toml, .cargo/rail.toml, .config/rail.toml
    Locate {
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
    /// Print the effective configuration with defaults
    ///
    /// Shows the merged configuration: user settings plus defaults for
    /// any unset fields. Useful for debugging and understanding what
    /// cargo-rail will actually use.
    Print {
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
    /// Validate the configuration file
    ///
    /// Checks for parse errors, unknown keys, and semantic issues.
    /// By default, unknown keys warn locally but error in CI environments
    /// (detected via CI, GITHUB_ACTIONS, GITLAB_CI, or CIRCLECI env vars).
    Validate {
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
        /// Treat warnings as errors (auto-enabled in CI)
        #[arg(long, conflicts_with = "no_strict")]
        strict: bool,
        /// Never treat warnings as errors (overrides CI auto-detection)
        #[arg(long, conflicts_with = "strict")]
        no_strict: bool,
    },
    /// Explain effective values, defaults, sources, and deprecations
    Explain {
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
    /// Apply explicit semantic configuration migrations
    ///
    /// This never adds coded defaults. It only performs reviewed migrations
    /// for deprecated fields while preserving unrelated TOML formatting.
    Migrate {
        /// Check for pending migrations without modifying rail.toml
        #[arg(long)]
        check: bool,
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
}

/// Subcommands for `cargo rail unify`
#[derive(Subcommand)]
pub enum UnifyCommand {
    /// Inspect Cargo resolution semantics without changing files
    Doctor {
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: UnifyOutputFormat,
    },
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
        #[arg(value_name = "CRATE")]
        crate_names: Vec<String>,
        /// Preview generated config without writing
        #[arg(long)]
        dry_run: bool,
    },
    /// Execute split operation
    Run {
        /// Crate name to split (mutually exclusive with --all)
        #[arg(conflicts_with = "all", value_name = "CRATE")]
        crate_name: Option<String>,
        /// Split all configured crates
        #[arg(short, long)]
        all: bool,
        /// Override remote repository
        #[arg(long)]
        remote: Option<String>,
        /// Check for pending split changes (exit 1 when pending)
        #[arg(long, short = 'c')]
        check: bool,
        /// Apply from a previously generated mutation plan file
        #[arg(long, value_name = "PATH", conflicts_with = "check")]
        plan: Option<PathBuf>,
        /// Allow running on dirty worktree (uncommitted changes)
        #[arg(long)]
        allow_dirty: bool,
        /// Skip confirmation prompts (for CI/automation)
        #[arg(short = 'y', long)]
        yes: bool,
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: SplitOutputFormat,
    },
}

/// Subcommands for `cargo rail release`
#[derive(Subcommand)]
pub enum ReleaseCommand {
    /// Configure release settings
    Init {
        /// Crate name(s) to configure (optional)
        #[arg(value_name = "CRATE")]
        crate_names: Vec<String>,
        /// Preview generated config without writing
        #[arg(long)]
        dry_run: bool,
    },
    /// Execute release (plan or publish)
    Run {
        /// Crate name(s) to release (mutually exclusive with --all)
        #[arg(conflicts_with = "all", value_name = "CRATE")]
        crate_names: Vec<String>,
        /// Release all workspace crates
        #[arg(short, long)]
        all: bool,
        /// Version bump [auto, major, minor, patch, prerelease, release, or "x.y.z"]
        #[arg(long, default_value = "auto")]
        bump: String,
        /// Check for a pending release plan (exit 1 when pending)
        #[arg(long, short = 'c')]
        check: bool,
        /// Apply from a previously generated mutation plan file
        #[arg(long, value_name = "PATH", conflicts_with = "check")]
        plan: Option<PathBuf>,
        /// Skip publishing to crates.io
        #[arg(long)]
        skip_publish: bool,
        /// Skip git tag creation
        #[arg(long)]
        skip_tag: bool,
        /// Prepare a release PR branch instead of tagging or publishing
        #[arg(long)]
        pr: bool,
        /// Expand explicit crate selection to include the full dependent closure
        #[arg(long)]
        include_dependents: bool,
        /// Skip confirmation prompts and allow non-default branch
        #[arg(short = 'y', long)]
        yes: bool,
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
    /// Validate release readiness
    Check {
        /// Crate name(s) to check (mutually exclusive with --all)
        #[arg(conflicts_with = "all", value_name = "CRATE")]
        crate_names: Vec<String>,
        /// Check all workspace crates (mutually exclusive with crate names)
        #[arg(short, long)]
        all: bool,
        /// Run extended validation (cargo publish --dry-run, MSRV check)
        #[arg(long, short = 'e')]
        extended: bool,
        /// Expand explicit crate selection to include the full dependent closure
        #[arg(long)]
        include_dependents: bool,
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
    /// Finalize a merged release PR (tag, push, publish)
    Finalize {
        /// Crate name(s) to finalize (required unless --all)
        #[arg(conflicts_with = "all", value_name = "CRATE")]
        crate_names: Vec<String>,
        /// Finalize all workspace crates with release notes for their current versions
        #[arg(short, long)]
        all: bool,
        /// Skip publishing to crates.io
        #[arg(long)]
        skip_publish: bool,
        /// Skip git tag creation
        #[arg(long)]
        skip_tag: bool,
        /// Expand explicit crate selection to include the full dependent closure and version groups
        #[arg(long)]
        include_dependents: bool,
        /// Skip confirmation prompts and allow non-default branch
        #[arg(short = 'y', long)]
        yes: bool,
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
    /// Resume an interrupted release from its durable state file
    Resume {
        /// State path printed by the interrupted release
        #[arg(value_name = "STATE")]
        state: PathBuf,
    },
    /// Show durable release state and the safe recovery command
    Status {
        /// Inspect one state file instead of every known release transaction
        #[arg(value_name = "STATE")]
        state: Option<PathBuf>,
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
    /// Abort an active release that has not reached remote side effects
    Abort {
        /// State path printed by the active release
        #[arg(value_name = "STATE")]
        state: PathBuf,
        /// Confirm restoration of the pre-release local state
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

/// Subcommands for `cargo rail change`
#[derive(Subcommand)]
pub enum ChangeCommand {
    /// Create a pending change file
    Add {
        /// Crate name(s) covered by this change
        #[arg(value_name = "CRATE")]
        crate_names: Vec<String>,
        /// Release intent for the covered crate(s): none, patch, minor, major
        #[arg(long)]
        bump: String,
        /// User-facing changelog entry body (omit in a terminal to open $VISUAL/$EDITOR)
        #[arg(long, short = 'm')]
        message: Option<String>,
        /// Override the generated filename slug
        #[arg(long, value_name = "SLUG")]
        name: Option<String>,
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: ChangeOutputFormat,
    },
    /// Show pending change files
    Status {
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: ChangeOutputFormat,
    },
    /// Check that changed crates have pending change files
    Check {
        /// Compare against this git ref
        #[arg(long, conflicts_with_all = ["merge_base", "all"], value_name = "REF")]
        since: Option<String>,
        /// Compare from the merge-base with the default branch
        #[arg(long, conflicts_with_all = ["since", "all"])]
        merge_base: bool,
        /// Scan the full reachable history
        #[arg(long, conflicts_with_all = ["since", "merge_base"])]
        all: bool,
        /// Require coverage for every changed crate, ignoring release.require_change_files
        #[arg(long)]
        required: bool,
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: ChangeOutputFormat,
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
            Commands::Doctor {
                command: DoctorCommand::NativeCache { format },
            } => format.is_json_like(),
            Commands::Cache { command } => match command {
                CacheCommand::Setup(setup) => setup.format.is_json_like(),
                CacheCommand::Normalize { format, .. }
                | CacheCommand::Status { format, .. }
                | CacheCommand::Clean { format, .. }
                | CacheCommand::Remove { format, .. } => format.is_json_like(),
            },
            Commands::Sync { format, .. } | Commands::Clean { format, .. } => format.is_json_like(),
            Commands::Plan { format, schema, .. } => *schema || format.is_json_like(),
            Commands::Unify {
                command: Some(UnifyCommand::Doctor { format }),
                ..
            } => format.is_json_like(),
            Commands::Unify { format, .. } => format.is_json_like(),
            Commands::Split { command } => match command {
                SplitCommand::Init { .. } => false,
                SplitCommand::Run { format, .. } => format.is_json_like(),
            },
            Commands::Release { command } => match command {
                ReleaseCommand::Init { .. } => false,
                ReleaseCommand::Resume { .. } | ReleaseCommand::Abort { .. } => false,
                ReleaseCommand::Status { format, .. } => format.is_json_like(),
                ReleaseCommand::Run { format, .. }
                | ReleaseCommand::Check { format, .. }
                | ReleaseCommand::Finalize { format, .. } => format.is_json_like(),
            },
            Commands::Change { command } => match command {
                ChangeCommand::Add { format, .. }
                | ChangeCommand::Status { format }
                | ChangeCommand::Check { format, .. } => format.is_json_like(),
            },
            Commands::Config { command } => match command {
                ConfigCommand::Locate { format }
                | ConfigCommand::Print { format }
                | ConfigCommand::Explain { format }
                | ConfigCommand::Validate { format, .. }
                | ConfigCommand::Migrate { format, .. } => format.is_json_like(),
            },
            Commands::Hash { format, .. } | Commands::DiffHash { format, .. } => format.is_json_like(),
            Commands::Graph { dot, .. } => !dot,
            _ => false,
        }
    }

    /// Apply global `--json` by overriding the selected command's format.
    ///
    /// Commands without a structured output contract reject the shorthand instead
    /// of silently emitting text while JSON mode is enabled.
    pub fn apply_json_override(&mut self) -> Result<(), clap::Error> {
        let unsupported = match self {
            Commands::Unify {
                command: Some(UnifyCommand::Undo { .. }),
                ..
            } => Some("unify undo"),
            Commands::Split {
                command: SplitCommand::Init { .. },
            } => Some("split init"),
            Commands::Release {
                command: ReleaseCommand::Init { .. },
            } => Some("release init"),
            Commands::Release {
                command: ReleaseCommand::Resume { .. },
            } => Some("release resume"),
            Commands::Release {
                command: ReleaseCommand::Abort { .. },
            } => Some("release abort"),
            Commands::Graph { dot: true, .. } => Some("graph --dot"),
            Commands::Completions { .. } => Some("completions"),
            _ => None,
        };
        if let Some(command) = unsupported {
            return Err(CargoCli::command().error(
                ErrorKind::ArgumentConflict,
                format!("--json is not supported by 'cargo rail {command}'"),
            ));
        }

        match self {
            Commands::Doctor {
                command: DoctorCommand::NativeCache { format },
            } => *format = TextJsonOutputFormat::Json,
            Commands::Cache { command } => match command {
                CacheCommand::Setup(setup) => setup.format = TextJsonOutputFormat::Json,
                CacheCommand::Normalize { format, .. }
                | CacheCommand::Status { format, .. }
                | CacheCommand::Clean { format, .. }
                | CacheCommand::Remove { format, .. } => {
                    *format = TextJsonOutputFormat::Json;
                }
            },
            Commands::Sync { format, .. } | Commands::Clean { format, .. } => *format = TextJsonOutputFormat::Json,
            Commands::Plan { format, .. } => *format = PlanOutputFormat::Json,
            Commands::Surface { format, .. } => *format = SurfaceOutputFormat::Json,
            Commands::Unify {
                command: Some(UnifyCommand::Doctor { format }),
                ..
            } => *format = UnifyOutputFormat::Json,
            Commands::Unify { format, .. } => *format = UnifyOutputFormat::Json,
            Commands::Split {
                command: SplitCommand::Run { format, .. },
            } => *format = SplitOutputFormat::Json,
            Commands::Split { .. } => {}
            Commands::Release {
                command:
                    ReleaseCommand::Run { format, .. }
                    | ReleaseCommand::Check { format, .. }
                    | ReleaseCommand::Finalize { format, .. }
                    | ReleaseCommand::Status { format, .. },
            } => *format = TextJsonOutputFormat::Json,
            Commands::Release { .. } => {}
            Commands::Change {
                command:
                    ChangeCommand::Add { format, .. }
                    | ChangeCommand::Status { format }
                    | ChangeCommand::Check { format, .. },
            } => *format = ChangeOutputFormat::Json,
            Commands::Config { command } => match command {
                ConfigCommand::Locate { format }
                | ConfigCommand::Print { format }
                | ConfigCommand::Explain { format }
                | ConfigCommand::Validate { format, .. }
                | ConfigCommand::Migrate { format, .. } => *format = TextJsonOutputFormat::Json,
            },
            Commands::Hash { format, .. } | Commands::DiffHash { format, .. } => *format = TextJsonOutputFormat::Json,
            Commands::Graph { .. } => {}
            _ => {}
        }

        Ok(())
    }
}

/// Generate shell completions and print to stdout
pub fn generate_completions(shell: Shell) {
    let mut cmd = CargoCli::command();
    clap_complete::generate(shell, &mut cmd, "cargo-rail", &mut std::io::stdout());
}

#[cfg(test)]
mod tests {
    use super::parse_cache_size;

    #[test]
    fn transparent_cache_size_grammar_is_exact_and_bounded() {
        assert_eq!(parse_cache_size("1B"), Ok(1));
        assert_eq!(parse_cache_size("10GiB"), Ok(10 * 1024 * 1024 * 1024));
        for invalid in [
            "0B",
            "01GiB",
            "10",
            "1GB",
            "1.5GiB",
            "-1GiB",
            "1gib",
            "18446744073709551615TiB",
        ] {
            assert!(parse_cache_size(invalid).is_err(), "accepted invalid size {invalid}");
        }
    }
}
