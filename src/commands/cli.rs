//! CLI argument definitions for cargo-rail.
//!
//! This module defines all CLI structures using clap. These are internal
//! and used by main.rs and the dispatch logic in commands/mod.rs.
//!
//! **Note:** This is not part of the stable public API.

use super::common::{
    ChangeOutputFormat, SplitOutputFormat, SurfaceOutputFormat, TextJsonOutputFormat, UnifyOutputFormat,
};
use crate::output::OutputProtocol;
use crate::sync::ConflictStrategy;
use clap::{Args, CommandFactory, Parser, Subcommand, error::ErrorKind};
use clap_complete::Shell;
use std::path::PathBuf;

const MAIN_HELP: &str = "\
Plan workspace work, check dependencies and visibility, and manage releases and compiler reuse.

Start here:
  cargo rail init             # Create repository policy
  cargo rail plan             # Decide required work
  cargo rail unify --check    # Check dependency changes

Docs: https://github.com/loadingalias/cargo-rail";

/// Global options and the selected Cargo-Rail command.
#[derive(Debug, Parser)]
#[command(name = "cargo-rail")]
#[command(bin_name = "cargo-rail")]
#[command(version)]
#[command(about = "Plan workspace work, check dependencies and visibility, and manage releases and compiler reuse")]
#[command(long_about = MAIN_HELP)]
#[command(propagate_version = true)]
#[command(styles = get_styles())]
pub struct RailCli {
    /// Suppress progress messages
    #[arg(
        long,
        short,
        global = true,
        conflicts_with = "verbose",
        help_heading = "Global Options"
    )]
    pub quiet: bool,

    /// Show additional operational detail
    #[arg(
        long,
        short = 'v',
        global = true,
        conflicts_with = "quiet",
        help_heading = "Global Options"
    )]
    pub verbose: bool,

    /// Output as JSON where supported; rejected otherwise
    #[arg(long, global = true, help_heading = "Global Options")]
    pub json: bool,

    /// Path to rail.toml config file (bypass search order)
    #[arg(long, global = true, value_name = "PATH", help_heading = "Global Options")]
    pub config: Option<PathBuf>,

    /// Workspace root directory (default: current directory)
    #[arg(long, global = true, value_name = "PATH", help_heading = "Global Options")]
    pub workspace_root: Option<PathBuf>,

    /// Write diagnostic performance counters to this JSON file
    #[arg(long, global = true, value_name = "PATH", hide = true)]
    pub diagnostics_file: Option<PathBuf>,

    /// The subcommand to execute
    #[command(subcommand)]
    pub command: Commands,
}

const PLAN_HELP: &str = "\
Without comparison flags, use changes since the default-branch merge base.

  cargo rail plan --from abc --to def        # Compare two commits
  cargo rail plan --explain-work cargo.test  # Explain one decision, even when skipped
  cargo rail plan --json > plan.json         # Save the exact plan
  cargo rail plan --verify plan.json         # Verify checkout binding without execution

Use --verify - to read the saved plan from standard input.";

const SURFACE_HELP: &str = "\
Set `[surface] enabled = true` to include this gate in planner-selected CI.
Use `consumer_scope = \"workspace\"` only when each closed compiler crate has no
consumers outside the captured workspace.

  cargo rail surface --check --explain        # Check without modifying source
  cargo rail surface --fix --dry-run          # Preview visibility edits
  cargo rail surface --fix --backup           # Apply with recovery evidence
  cargo rail surface --resume MANIFEST --json # Resume partial compiler acquisition";

const UNIFY_HELP: &str = "\
Without a subcommand, preview dependency changes without modifying manifests.
Use --check to exit 1 when changes are pending.

  cargo rail unify --show-diff     # Inspect manifest changes
  cargo rail unify apply --backup # Apply with a backup
  cargo rail unify undo --list    # Find a backup to restore";

const SPLIT_HELP: &str = "\
Extract crates to standalone repositories while preserving Git history.

  cargo rail split init my-crate --dry-run # Preview configuration
  cargo rail split run my-crate --check   # Check for pending changes (exit 1)
  cargo rail split run my-crate           # Apply the split";

const SYNC_HELP: &str = "\
Requires split configuration. Check mode reports pending changes without
modifying either repository. Apply is bidirectional unless a direction is selected.

  cargo rail sync my-crate --check       # Inspect pending work (exit 1)
  cargo rail sync my-crate --to-remote   # Push monorepo changes to the split repo
  cargo rail sync my-crate --from-remote # Bring remote changes to a review branch

Use --resume RECEIPT after resolving a recorded conflict.";

const RELEASE_HELP: &str = "\
Publication requires explicit --publish authority. Interrupted transactions
retain a state path for `release resume`; `release status` shows recovery actions.

  cargo rail release check --all --publication # Validate publication authority
  cargo rail release run my-crate --bump minor # Execute a release transaction
  cargo rail release run --all --publish --wait --yes # Publish after exact-SHA checks";

const CHANGE_HELP: &str = "\
Record a user-facing entry and its release intent:
  cargo rail change add my-crate --bump patch --message \"Fixed release notes\"
  cargo rail change check --merge-base

Omit --message in a terminal to use $VISUAL or $EDITOR.
Release deletes consumed files in its commit. A plan covering only some of a
file's crates is rejected; pending intent is never partially consumed.";

const INIT_HELP: &str = "\
By default, write .config/rail.toml. Target detection requires --detect-targets;
use repeated --target flags to select targets explicitly.

  cargo rail init --dry-run             # Preview policy
  cargo rail init --target wasm32-wasip1 # Declare a target";

const CLEAN_HELP: &str = "\
Select an artifact class explicitly. --all covers eligible workspace artifacts;
--release-journal selects one terminal transaction by ID or state path.

  cargo rail clean --all --check         # Preview workspace cleanup (exit 1 if pending)
  cargo rail clean --release-journal ID  # Delete one terminal journal";

const CACHE_HELP: &str = "\
Cache setup enrolls this workspace for ordinary Cargo commands.
Remote storage and distributed workers require explicit machine-owned authority;
repository policy cannot enable them.

  cargo rail cache setup --check               # Preview enrollment
  cargo rail cache setup                       # Install or repair the wrapper
  cargo rail cache clean --scope local --check # Preview selected-profile cleanup

Detach preserves the profile and CAS. Uninstall removes the global wrapper while
preserving all profiles and their CAS data.";

const CONFIG_HELP: &str = "\
Bare `cargo rail config` explains configured overrides. Unknown fields and retired
configuration spellings are rejected.

  cargo rail config explain --all # Inspect all effective values and sources
  cargo rail config print         # Emit reusable effective configuration
  cargo rail config validate      # Check configuration";

const COMPLETIONS_HELP: &str = "\
Add the matching command to your shell startup file:

  # Bash (~/.bashrc)
  eval \"$(cargo rail completions bash)\"

  # Zsh (~/.zshrc)
  eval \"$(cargo rail completions zsh)\"

  # Fish (~/.config/fish/config.fish)
  cargo rail completions fish | source

  # PowerShell ($PROFILE)
  cargo rail completions powershell | Out-String | Invoke-Expression";

/// Available subcommands
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Inspect native compiler-cache capability
    Doctor {
        /// Diagnostic to run
        #[command(subcommand)]
        command: DoctorCommand,
    },

    /// Install, inspect, or reclaim explicitly scoped compiler-cache state
    #[command(after_long_help = CACHE_HELP)]
    Cache {
        /// Cache operation
        #[command(subcommand)]
        command: CacheCommand,
    },

    /// Build an evidence-backed named-work plan
    #[command(after_long_help = PLAN_HELP)]
    Plan {
        /// Compare against this Git ref (default: default-branch merge base)
        #[arg(long)]
        since: Option<String>,
        /// Start ref (for SHA pair mode)
        #[arg(long, conflicts_with = "since", requires = "to")]
        from: Option<String>,
        /// End ref (for SHA pair mode)
        #[arg(long, requires = "from")]
        to: Option<String>,
        /// Machine output selected by the global --json flag
        #[arg(skip)]
        json: bool,
        /// Explain required work decisions
        #[arg(long)]
        explain: bool,
        /// Explain one exact work decision, including when it was skipped
        #[arg(long, value_name = "WORK_ID")]
        explain_work: Option<String>,
        /// Require every registered work item with full valid scope
        #[arg(long)]
        all: bool,
        /// Load compatible observed-input evidence from a file
        #[arg(long, value_name = "PATH")]
        evidence: Option<PathBuf>,
        /// Verify that the current checkout matches one saved plan; use `-` for standard input
        #[arg(
            long,
            value_name = "PATH",
            conflicts_with_all = ["since", "from", "to", "explain", "explain_work", "all", "evidence", "schema"]
        )]
        verify: Option<PathBuf>,
        /// Print the versioned planner JSON Schema and exit
        #[arg(long, conflicts_with_all = ["since", "from", "to", "explain", "explain_work", "all", "evidence", "verify"])]
        schema: bool,
    },

    /// Analyze declaration reachability and repair visibility
    #[command(after_long_help = SURFACE_HELP)]
    Surface {
        /// Prepare and authenticate the exact-toolchain Surface producer without analysis
        #[arg(
            long,
            conflicts_with_all = ["check", "fix", "dry_run", "backup", "explain", "only", "schema"]
        )]
        prepare: bool,
        /// Fail on denied findings without modifying source
        #[arg(long, conflicts_with = "fix")]
        check: bool,
        /// Apply exact visibility reductions
        #[arg(long, conflicts_with = "check")]
        fix: bool,
        /// Resume from a prior partial acquisition manifest
        #[arg(
            long,
            value_name = "MANIFEST",
            conflicts_with_all = ["prepare", "fix", "dry_run", "backup", "schema"]
        )]
        resume: Option<PathBuf>,
        /// Preview visibility edits without modifying source
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
        /// Restrict findings and fixes to these lint classes
        #[arg(
            long,
            value_name = "LINT",
            value_delimiter = ',',
            value_parser = [
                "dead-public",
                "unnecessary-public",
                "unnecessary-restricted-visibility",
                "unnecessary-crate-visibility"
            ]
        )]
        only: Vec<String>,
        /// Print the versioned surface JSON Schema and exit
        #[arg(
            long,
            conflicts_with_all = ["prepare", "check", "fix", "resume", "dry_run", "backup", "output", "explain", "only"]
        )]
        schema: bool,
    },

    /// Analyze and repair workspace dependency coherence
    #[command(after_long_help = UNIFY_HELP)]
    Unify {
        /// Explicit mutation, diagnostics, or recovery operation
        #[command(subcommand)]
        command: Option<UnifyCommand>,
        /// Check for pending manifest changes without modifying manifests (exit 1 when pending)
        #[arg(long, short = 'c')]
        check: bool,
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: UnifyOutputFormat,
        /// Generate the dependency report
        #[arg(long)]
        report: bool,
        /// Path for the dependency report
        #[arg(long, value_name = "PATH", requires = "report")]
        report_path: Option<PathBuf>,
        /// Write output to file (overwrites existing content)
        #[arg(long, short = 'o', value_name = "PATH", requires = "check")]
        output: Option<PathBuf>,
        /// Show diff of changes to each manifest
        #[arg(long)]
        show_diff: bool,
        /// Explain each dependency decision
        #[arg(long)]
        explain: bool,
    },

    /// Create sparse repository policy in rail.toml
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
        /// Add an exact supported Cargo target triple (repeatable)
        #[arg(long = "target", value_name = "TRIPLE", conflicts_with = "detect_targets")]
        targets: Vec<String>,
        /// Detect target triples from repository files
        #[arg(long, conflicts_with = "targets")]
        detect_targets: bool,
    },

    /// Split a crate to a standalone repository with Git history
    #[command(after_long_help = SPLIT_HELP)]
    Split {
        /// Split subcommand
        #[command(subcommand)]
        command: SplitCommand,
    },

    /// Synchronize a monorepo with configured split repositories
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
        /// Allow uncommitted worktree changes
        #[arg(long)]
        allow_dirty: bool,
        /// Skip confirmation prompts
        #[arg(short = 'y', long)]
        yes: bool,
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },

    /// Plan and execute exact-SHA releases
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

    /// Clean generated artifacts owned by the current workspace
    #[command(after_long_help = CLEAN_HELP)]
    Clean {
        /// Clean every eligible workspace-owned artifact class.
        #[arg(long, conflicts_with_all = ["cache", "prune_backups", "all_backups", "reports", "release_journal"])]
        all: bool,
        /// Clean cache state owned by this workspace
        #[arg(long)]
        cache: bool,
        /// Prune backups beyond the configured retention bound.
        #[arg(long, conflicts_with = "all_backups")]
        prune_backups: bool,
        /// Delete every workspace backup.
        #[arg(long, conflicts_with = "prune_backups")]
        all_backups: bool,
        /// Clean generated reports
        #[arg(long)]
        reports: bool,
        /// Delete exactly one terminal release journal by transaction ID or state path.
        #[arg(long, value_name = "ID_OR_PATH")]
        release_journal: Option<String>,
        /// Check for pending cleanup without deleting files (exit 1 when pending)
        #[arg(long, short = 'c')]
        check: bool,
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },

    /// Inspect effective repository policy
    #[command(name = "config")]
    #[command(after_long_help = CONFIG_HELP)]
    Config {
        /// Subcommand
        #[command(subcommand)]
        command: Option<ConfigCommand>,
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
            Self::Plan { .. } => false,
            _ => false,
        }
    }

    /// Return whether dispatch needs sparse planning state captured before metadata loading.
    #[doc(hidden)]
    pub fn requires_planning_source_capture(&self) -> bool {
        match self {
            Self::Plan { from, to, schema, .. } => !(*schema || from.is_some() && to.is_some()),
            _ => false,
        }
    }

    /// Return whether dispatch needs source captured before metadata loading.
    #[doc(hidden)]
    pub fn requires_worktree_source_capture(&self) -> bool {
        match self {
            Self::Doctor { .. } | Self::Surface { .. } => false,
            _ => false,
        }
    }
}

/// Subcommands for `cargo rail doctor`.
#[derive(Debug, Subcommand)]
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
    /// The validated local CAS owned by the selected workspace profile.
    Local,
    /// Both workspace state and the selected profile's local CAS.
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
#[derive(Debug, Args)]
pub struct CacheSetupArgs {
    /// Existing opaque profile ID to bind this workspace explicitly.
    #[arg(long, value_name = "PROFILE_ID")]
    pub profile: Option<String>,
    /// Local cache base directory (defaults to Cargo home).
    #[arg(long, value_name = "PATH")]
    pub local_dir: Option<PathBuf>,
    /// Maximum local cache size; use a positive integer with B, KiB, MiB, GiB, or TiB.
    #[arg(long, value_name = "SIZE", value_parser = parse_cache_size)]
    pub max_size: Option<u64>,
    /// Machine-owned remote cache URL to persist with this workspace profile.
    #[arg(long, value_name = "URL", conflicts_with = "local_only")]
    pub remote: Option<String>,
    /// Maximum remote authority; explicit selection defaults to read-write.
    #[arg(long, value_name = "MODE", value_parser = ["read", "read-write"], requires = "remote")]
    pub remote_mode: Option<String>,
    /// Additional reviewed compiler environment name admitted to L2 identity.
    #[arg(long = "remote-environment", value_name = "NAME", requires = "remote")]
    pub remote_environment: Vec<String>,
    /// Root identity mode: physical binds exact paths; remap permits eligible cross-root reuse.
    #[arg(long, value_name = "MODE", value_parser = ["physical", "remap"])]
    pub root_portability: Option<String>,
    /// Remove persisted remote activation while preserving local reuse.
    #[arg(long, conflicts_with_all = ["remote", "remote_mode", "remote_environment", "root_portability"])]
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
#[derive(Debug, Subcommand)]
pub enum CacheCommand {
    /// Install or repair transparent verified compiler reuse.
    Setup(Box<CacheSetupArgs>),
    /// Start or finish one explicit cache measurement interval.
    Report {
        /// Create a new private recording; export CARGO_RAIL_CACHE_REPORT to this absolute path.
        #[arg(long, conflicts_with = "finish", required_unless_present = "finish")]
        start: Option<PathBuf>,
        /// Finish the interval and report aggregated compiler outcomes.
        #[arg(long, conflicts_with = "start", required_unless_present = "start")]
        finish: Option<PathBuf>,
        /// Report format.
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
    /// Validate and normalize a remote cache URL without network access.
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
    /// Authenticate the selected remote object store and validate its protocol marker.
    Probe {
        /// Report format.
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
    /// Report cache installation and owned-storage health.
    Status {
        /// Cache scope to inspect.
        #[arg(long, value_enum, default_value = "all")]
        scope: CacheScope,
        /// Report format.
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
    /// Inspect every private workspace profile without selecting or mutating one.
    Profiles {
        /// Report format.
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
    /// Detach the current workspace while preserving its profile and CAS data.
    Detach {
        /// Preview the exact binding removal without modifying profile state.
        #[arg(long, short = 'c')]
        check: bool,
        /// Report format.
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
    /// Permanently remove one detached workspace profile and its owned local state.
    DropProfile {
        /// Opaque profile identifier reported by `cargo rail cache profiles`.
        #[arg(long)]
        profile: String,
        /// Preview the exact profile removal without modifying cache state.
        #[arg(long, short = 'c')]
        check: bool,
        /// Report format.
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
    /// Quarantine a selected markerless CAS and create a fresh owned authority.
    Recover {
        /// Preview the exact quarantine move without modifying cache state.
        #[arg(long, short = 'c')]
        check: bool,
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
    /// Uninstall the one global wrapper while preserving every workspace profile and CAS.
    Uninstall {
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
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print the path to the active config file
    ///
    /// Without --config, searches in order:
    /// rail.toml, .rail.toml, .cargo/rail.toml, .config/rail.toml
    Locate {
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
    /// Print canonical effective configuration with defaults
    ///
    /// Text output is reusable `rail.toml` input.
    Print {
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
    /// Validate the configuration file
    ///
    /// Parse errors and unknown keys always fail. Semantic warnings become errors
    /// in CI unless --no-strict is selected.
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
    /// Explain effective values, defaults, and sources
    Explain {
        /// Exact configuration field path(s) to explain in full
        #[arg(value_name = "FIELD", conflicts_with = "all")]
        fields: Vec<String>,
        /// Explain every known effective field
        #[arg(long, conflicts_with = "fields")]
        all: bool,
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
}

/// Subcommands for `cargo rail unify`
#[derive(Debug, Subcommand)]
pub enum UnifyCommand {
    /// Apply the exact dependency-coherence decision
    Apply {
        /// Apply from a previously generated mutation plan file
        #[arg(long, value_name = "PATH")]
        plan: Option<PathBuf>,
        /// Create backups of all modified files
        #[arg(long)]
        backup: bool,
        /// Generate the dependency report
        #[arg(long)]
        report: bool,
        /// Path for the dependency report
        #[arg(long, value_name = "PATH", requires = "report")]
        report_path: Option<PathBuf>,
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: UnifyOutputFormat,
    },
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
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
}

/// Subcommands for `cargo rail split`
#[derive(Debug, Subcommand)]
pub enum SplitCommand {
    /// Configure crate splits
    Init {
        /// Crate name(s) to configure
        #[arg(value_name = "CRATE")]
        crate_names: Vec<String>,
        /// Preview generated config without writing
        #[arg(long)]
        dry_run: bool,
    },
    /// Extract the selected crates and their Git history
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
        /// Allow uncommitted worktree changes
        #[arg(long)]
        allow_dirty: bool,
        /// Skip confirmation prompts
        #[arg(short = 'y', long)]
        yes: bool,
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: SplitOutputFormat,
    },
}

/// Subcommands for `cargo rail release`
#[derive(Debug, Subcommand)]
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
    /// Execute a durable release transaction
    Run {
        /// Crate name(s) to release (mutually exclusive with --all)
        #[arg(conflicts_with = "all", value_name = "CRATE")]
        crate_names: Vec<String>,
        /// Release all workspace crates
        #[arg(short, long)]
        all: bool,
        /// Version bump: auto, major, minor, patch, prerelease, release, or x.y.z
        #[arg(long, default_value = "auto")]
        bump: String,
        /// Apply from a previously generated mutation plan file
        #[arg(long, value_name = "PATH")]
        plan: Option<PathBuf>,
        /// Authorize irreversible publication to crates.io
        #[arg(long, conflicts_with = "pr")]
        publish: bool,
        /// Skip git tag creation
        #[arg(long)]
        skip_tag: bool,
        /// Prepare a release PR branch instead of tagging or publishing
        #[arg(long)]
        pr: bool,
        /// Wait for exact-SHA remote checks instead of stopping with a resume command
        #[arg(long, conflicts_with = "pr")]
        wait: bool,
        /// Expand explicit crate selection to include the full dependent closure
        #[arg(long)]
        include_dependents: bool,
        /// Skip the interactive confirmation prompt.
        #[arg(short = 'y', long)]
        yes: bool,
        /// Authorize release execution from a non-default branch.
        #[arg(long)]
        allow_non_default_branch: bool,
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
    /// Validate the local release plan or publication readiness
    Check {
        /// Crate name(s) to check (mutually exclusive with --all)
        #[arg(conflicts_with = "all", value_name = "CRATE")]
        crate_names: Vec<String>,
        /// Check all workspace crates (mutually exclusive with crate names)
        #[arg(short, long)]
        all: bool,
        /// Version bump: auto, major, minor, patch, prerelease, release, or x.y.z
        #[arg(long, default_value = "auto")]
        bump: String,
        /// Validate publication authority for the same release plan
        #[arg(long)]
        publication: bool,
        /// Run extended publication validation (publish dry-run, MSRV, optional semver checks)
        #[arg(long, short = 'e', requires = "publication")]
        extended: bool,
        /// Exclude git tag creation from the release plan
        #[arg(long)]
        skip_tag: bool,
        /// Expand explicit crate selection to include the full dependent closure
        #[arg(long)]
        include_dependents: bool,
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: TextJsonOutputFormat,
    },
    /// Finalize the exact release transaction introduced by a merged release PR
    Finalize {
        /// Crate name(s) to finalize (required unless --all)
        #[arg(conflicts_with = "all", value_name = "CRATE")]
        crate_names: Vec<String>,
        /// Finalize all workspace crates with release notes for their current versions
        #[arg(short, long)]
        all: bool,
        /// Authorize irreversible publication to crates.io
        #[arg(long)]
        publish: bool,
        /// Skip git tag creation
        #[arg(long)]
        skip_tag: bool,
        /// Expand explicit crate selection to include the full dependent closure and version groups
        #[arg(long)]
        include_dependents: bool,
        /// Skip the interactive confirmation prompt.
        #[arg(short = 'y', long)]
        yes: bool,
        /// Authorize release execution from a non-default branch.
        #[arg(long)]
        allow_non_default_branch: bool,
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
        /// Include terminal and reconstructed transaction history
        #[arg(long)]
        history: bool,
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
#[derive(Debug, Subcommand)]
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
        /// Output format
        #[arg(long, short = 'f', default_value_t, value_enum)]
        format: ChangeOutputFormat,
    },
}

fn get_styles() -> clap::builder::Styles {
    clap::builder::Styles::styled()
}

fn text_json_protocol(json: bool) -> OutputProtocol {
    if json {
        OutputProtocol::Json
    } else {
        OutputProtocol::Text
    }
}

impl Commands {
    /// Select the invocation's complete output transport before fallible work.
    #[doc(hidden)]
    pub fn output_protocol(&self) -> OutputProtocol {
        match self {
            Commands::Doctor {
                command: DoctorCommand::NativeCache { format },
            } => text_json_protocol(format.is_json()),
            Commands::Cache { command } => match command {
                CacheCommand::Setup(setup) => text_json_protocol(setup.format.is_json()),
                CacheCommand::Normalize { format, .. }
                | CacheCommand::Probe { format }
                | CacheCommand::Status { format, .. }
                | CacheCommand::Report { format, .. }
                | CacheCommand::Recover { format, .. }
                | CacheCommand::Clean { format, .. }
                | CacheCommand::Profiles { format }
                | CacheCommand::Detach { format, .. }
                | CacheCommand::DropProfile { format, .. }
                | CacheCommand::Uninstall { format, .. } => text_json_protocol(format.is_json()),
            },
            Commands::Sync { format, .. } | Commands::Clean { format, .. } => text_json_protocol(format.is_json()),
            Commands::Plan { schema: true, .. } => OutputProtocol::Raw,
            Commands::Plan { json, .. } => text_json_protocol(*json),
            Commands::Surface { schema: true, .. } => OutputProtocol::Raw,
            Commands::Surface { format, .. } => match format {
                SurfaceOutputFormat::Text => OutputProtocol::Text,
                SurfaceOutputFormat::Json => OutputProtocol::Json,
                SurfaceOutputFormat::GitHub => OutputProtocol::Raw,
            },
            Commands::Unify {
                command: Some(UnifyCommand::Doctor { format } | UnifyCommand::Apply { format, .. }),
                ..
            } => text_json_protocol(format.is_json()),
            Commands::Unify {
                command: Some(UnifyCommand::Undo { format, .. }),
                ..
            } => text_json_protocol(format.is_json()),
            Commands::Unify { format, .. } => text_json_protocol(format.is_json()),
            Commands::Split { command } => match command {
                SplitCommand::Init { .. } => OutputProtocol::Text,
                SplitCommand::Run { format, .. } => match format {
                    SplitOutputFormat::Text => OutputProtocol::Text,
                    SplitOutputFormat::Json => OutputProtocol::Json,
                    SplitOutputFormat::NamesOnly | SplitOutputFormat::JsonLines => OutputProtocol::Raw,
                },
            },
            Commands::Release { command } => match command {
                ReleaseCommand::Init { .. } | ReleaseCommand::Resume { .. } | ReleaseCommand::Abort { .. } => {
                    OutputProtocol::Text
                }
                ReleaseCommand::Status { format, .. } => text_json_protocol(format.is_json()),
                ReleaseCommand::Run { format, .. }
                | ReleaseCommand::Check { format, .. }
                | ReleaseCommand::Finalize { format, .. } => text_json_protocol(format.is_json()),
            },
            Commands::Change { command } => match command {
                ChangeCommand::Add { format, .. }
                | ChangeCommand::Status { format }
                | ChangeCommand::Check { format, .. } => match format {
                    ChangeOutputFormat::Text => OutputProtocol::Text,
                    ChangeOutputFormat::Json => OutputProtocol::Json,
                    ChangeOutputFormat::NamesOnly => OutputProtocol::Raw,
                },
            },
            Commands::Config {
                command:
                    Some(
                        ConfigCommand::Locate { format }
                        | ConfigCommand::Print { format }
                        | ConfigCommand::Explain { format, .. }
                        | ConfigCommand::Validate { format, .. },
                    ),
            } => text_json_protocol(format.is_json()),
            Commands::Completions { .. } => OutputProtocol::Raw,
            _ => OutputProtocol::Text,
        }
    }

    /// Check whether this command emits exactly one complete JSON value.
    pub fn is_json_format(&self) -> bool {
        self.output_protocol() == OutputProtocol::Json
    }

    /// Apply global `--json` by overriding the selected command's format.
    ///
    /// Commands without a structured output contract reject the shorthand instead
    /// of silently emitting text while JSON mode is enabled.
    pub fn apply_json_override(&mut self) -> Result<(), clap::Error> {
        let unsupported = match self {
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
            Commands::Completions { .. } => Some("completions"),
            _ => None,
        };
        if let Some(command) = unsupported {
            return Err(RailCli::command().error(
                ErrorKind::ArgumentConflict,
                format!("--json is not supported by 'cargo rail {command}'"),
            ));
        }

        let incompatible_stream = match self {
            Commands::Split {
                command: SplitCommand::Run { format, .. },
            } => match format {
                SplitOutputFormat::NamesOnly => Some("names-only"),
                SplitOutputFormat::JsonLines => Some("jsonl"),
                SplitOutputFormat::Text | SplitOutputFormat::Json => None,
            },
            Commands::Surface { format, .. } => match format {
                SurfaceOutputFormat::GitHub => Some("github"),
                SurfaceOutputFormat::Text | SurfaceOutputFormat::Json => None,
            },
            Commands::Change { command } => match command {
                ChangeCommand::Add { format, .. }
                | ChangeCommand::Status { format }
                | ChangeCommand::Check { format, .. } => match format {
                    ChangeOutputFormat::NamesOnly => Some("names-only"),
                    ChangeOutputFormat::Text | ChangeOutputFormat::Json => None,
                },
            },
            _ => None,
        };
        if let Some(format) = incompatible_stream {
            return Err(RailCli::command().error(
                ErrorKind::ArgumentConflict,
                format!("--json conflicts with the distinct '--format {format}' stream protocol"),
            ));
        }

        match self {
            Commands::Doctor {
                command: DoctorCommand::NativeCache { format },
            } => *format = TextJsonOutputFormat::Json,
            Commands::Cache { command } => match command {
                CacheCommand::Setup(setup) => setup.format = TextJsonOutputFormat::Json,
                CacheCommand::Normalize { format, .. }
                | CacheCommand::Probe { format }
                | CacheCommand::Status { format, .. }
                | CacheCommand::Report { format, .. }
                | CacheCommand::Recover { format, .. }
                | CacheCommand::Clean { format, .. }
                | CacheCommand::Profiles { format }
                | CacheCommand::Detach { format, .. }
                | CacheCommand::DropProfile { format, .. }
                | CacheCommand::Uninstall { format, .. } => {
                    *format = TextJsonOutputFormat::Json;
                }
            },
            Commands::Sync { format, .. } | Commands::Clean { format, .. } => *format = TextJsonOutputFormat::Json,
            Commands::Plan { json, .. } => *json = true,
            Commands::Surface { format, .. } => *format = SurfaceOutputFormat::Json,
            Commands::Unify {
                command: Some(UnifyCommand::Doctor { format } | UnifyCommand::Apply { format, .. }),
                ..
            } => *format = UnifyOutputFormat::Json,
            Commands::Unify {
                command: Some(UnifyCommand::Undo { format, .. }),
                ..
            } => *format = TextJsonOutputFormat::Json,
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
                Some(
                    ConfigCommand::Locate { format }
                    | ConfigCommand::Print { format }
                    | ConfigCommand::Explain { format, .. }
                    | ConfigCommand::Validate { format, .. },
                ) => *format = TextJsonOutputFormat::Json,
                None => {
                    *command = Some(ConfigCommand::Explain {
                        fields: Vec::new(),
                        all: false,
                        format: TextJsonOutputFormat::Json,
                    })
                }
            },
            _ => {}
        }

        Ok(())
    }
}

/// Generate shell completions and print to stdout
pub fn generate_completions(shell: Shell) {
    let mut cmd = RailCli::command();
    clap_complete::generate(shell, &mut cmd, "cargo-rail", &mut std::io::stdout());
}

#[cfg(test)]
mod tests {
    use super::{RailCli, parse_cache_size};
    use crate::output::OutputProtocol;
    use clap::Parser as _;

    #[test]
    fn surface_formats_select_distinct_output_protocols() {
        for (format, expected) in [
            ("text", OutputProtocol::Text),
            ("json", OutputProtocol::Json),
            ("github", OutputProtocol::Raw),
        ] {
            let cli = RailCli::try_parse_from(["cargo-rail", "surface", "--format", format])
                .expect("surface format must parse");
            assert_eq!(
                cli.command.output_protocol(),
                expected,
                "surface --format {format} selected the wrong transport"
            );
            assert_eq!(
                cli.command.is_json_format(),
                expected == OutputProtocol::Json,
                "surface --format {format} selected the wrong JSON compatibility mode"
            );
        }
    }

    #[test]
    fn command_owned_streams_select_the_raw_output_protocol() {
        let cases: &[&[&str]] = &[
            &["cargo-rail", "plan", "--schema"],
            &["cargo-rail", "surface", "--schema"],
            &["cargo-rail", "surface", "--format", "github"],
            &["cargo-rail", "split", "run", "crate-a", "--format", "names-only"],
            &["cargo-rail", "split", "run", "crate-a", "--format", "jsonl"],
            &["cargo-rail", "change", "status", "--format", "names-only"],
            &["cargo-rail", "completions", "bash"],
        ];

        for arguments in cases {
            let cli = RailCli::try_parse_from(arguments.iter().copied()).expect("raw stream command must parse");
            assert_eq!(
                cli.command.output_protocol(),
                OutputProtocol::Raw,
                "{} must retain its command-owned raw stream",
                arguments[1..].join(" ")
            );
            assert!(
                !cli.command.is_json_format(),
                "{} must not select JSON error rendering",
                arguments[1..].join(" ")
            );
        }
    }

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
