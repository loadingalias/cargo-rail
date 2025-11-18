mod cargo;
mod commands;
mod config;
mod error;
mod git;
mod graph;
mod split;
mod sync;
mod utils;
mod workspace;

use clap::{Parser, Subcommand};
use error::{RailError, print_error};

/// Graph-aware workspace orchestration for Rust monorepos
#[derive(Parser)]
#[command(name = "cargo")]
#[command(bin_name = "cargo")]
#[command(styles = get_styles())]
enum CargoCli {
  Rail(RailCli),
}

#[derive(Parser)]
#[command(name = "rail")]
#[command(version, about, long_about = None)]
#[command(propagate_version = true)]
#[command(styles = get_styles())]
struct RailCli {
  #[command(subcommand)]
  command: Commands,
}

#[derive(Subcommand)]
enum Commands {
  // ============================================================================
  // Graph-Aware CI Optimization
  // ============================================================================
  /// Graph-aware workspace operations
  #[command(subcommand)]
  Graph(GraphCommands),

  // ============================================================================
  // Dependency Unification
  // ============================================================================
  /// Workspace dependency unification (eliminates workspace-hack crates)
  #[command(subcommand)]
  Unify(UnifyCommands),

  // ============================================================================
  // Split/Sync Orchestration
  // ============================================================================
  /// Split a crate from monorepo to separate repo with history
  Split {
    /// Name of the crate to split
    crate_name: Option<String>,
    /// Split all configured crates
    #[arg(short, long)]
    all: bool,
    /// Override remote repository path
    #[arg(long)]
    remote: Option<String>,
    /// Actually perform the split (default: dry-run)
    #[arg(long)]
    apply: bool,
    /// Output plan in JSON format
    #[arg(long)]
    json: bool,
  },

  /// Sync changes between monorepo and split repos
  Sync {
    /// Name of the crate to sync
    crate_name: Option<String>,
    /// Sync all configured crates
    #[arg(short, long)]
    all: bool,
    /// Override remote repository path
    #[arg(long)]
    remote: Option<String>,
    /// Only sync from remote to monorepo
    #[arg(long)]
    from_remote: bool,
    /// Only sync from monorepo to remote
    #[arg(long)]
    to_remote: bool,
    /// Conflict resolution strategy: ours, theirs, manual, union
    #[arg(long, default_value = "manual")]
    strategy: String,
    /// Actually perform the sync (default: dry-run)
    #[arg(long)]
    apply: bool,
    /// Output plan in JSON format
    #[arg(long)]
    json: bool,
  },

  // ============================================================================
  // Workspace Inspection
  // ============================================================================
  /// Show status of all configured crates
  Status {
    /// Output status in JSON format
    #[arg(long)]
    json: bool,
  },
}

#[derive(Subcommand)]
enum GraphCommands {
  /// Show which crates are affected by changes
  Affected {
    /// Git ref to compare against (default: origin/main)
    #[arg(long, default_value = "origin/main")]
    since: String,
    /// Start ref (for SHA pair mode)
    #[arg(long, conflicts_with = "since")]
    from: Option<String>,
    /// End ref (for SHA pair mode)
    #[arg(long, requires = "from")]
    to: Option<String>,
    /// Output format: text, json, names-only
    #[arg(long, default_value = "text")]
    format: String,
  },
}

#[derive(Subcommand)]
enum UnifyCommands {
  /// Analyze dependencies and show unification plan (dry-run)
  Analyze {
    /// Exclude specific dependencies from unification
    #[arg(long)]
    exclude: Vec<String>,
    /// Force include specific dependencies
    #[arg(long)]
    include: Vec<String>,
    /// Only unify normal dependencies (exclude dev and build dependencies)
    #[arg(long)]
    normal_only: bool,
  },

  /// Apply workspace dependency unification (modifies Cargo.toml files)
  Apply {
    /// Exclude specific dependencies from unification
    #[arg(long)]
    exclude: Vec<String>,
    /// Force include specific dependencies
    #[arg(long)]
    include: Vec<String>,
    /// Create .bak backups of all modified files
    #[arg(long)]
    backup: bool,
    /// Only unify normal dependencies (exclude dev and build dependencies)
    #[arg(long)]
    normal_only: bool,
  },

  /// Check workspace dependencies are properly unified (for CI)
  Check {
    /// Exclude specific dependencies from check
    #[arg(long)]
    exclude: Vec<String>,
    /// Only check normal dependencies
    #[arg(long)]
    normal_only: bool,
  },
}

fn get_styles() -> clap::builder::Styles {
  clap::builder::Styles::styled()
}

fn main() {
  let CargoCli::Rail(cli) = CargoCli::parse();

  // Build workspace context once (single-load pattern)
  let workspace_root = match std::env::current_dir() {
    Ok(dir) => dir,
    Err(e) => {
      eprintln!("Error: Failed to get current directory: {}", e);
      std::process::exit(1);
    }
  };

  let ctx = match workspace::WorkspaceContext::build(&workspace_root) {
    Ok(ctx) => ctx,
    Err(e) => {
      handle_error(e);
    }
  };

  let result = match cli.command {
    // Graph Commands
    Commands::Graph(graph_cmd) => match graph_cmd {
      GraphCommands::Affected {
        since,
        from,
        to,
        format,
      } => commands::run_affected(&ctx, since, from, to, format, false),
    },

    // Dependency Unification
    Commands::Unify(unify_cmd) => match unify_cmd {
      UnifyCommands::Analyze {
        exclude,
        include,
        normal_only,
      } => commands::run_unify_analyze(&ctx, exclude, include, normal_only),
      UnifyCommands::Apply {
        exclude,
        include,
        backup,
        normal_only,
      } => commands::run_unify_apply(&ctx, exclude, include, backup, normal_only),
      UnifyCommands::Check { exclude, normal_only } => commands::run_unify_check(&ctx, exclude, normal_only),
    },

    // Split/Sync
    Commands::Split {
      crate_name,
      all,
      remote,
      apply,
      json,
    } => commands::run_split(&ctx, crate_name, all, remote, apply, json),
    Commands::Sync {
      crate_name,
      all,
      remote,
      from_remote,
      to_remote,
      strategy,
      apply,
      json,
    } => commands::run_sync(
      &ctx,
      crate_name,
      all,
      remote,
      from_remote,
      to_remote,
      strategy,
      false, // no_protected_branches
      apply,
      json,
    ),

    // Status
    Commands::Status { json } => commands::run_status(&ctx, json),
  };

  if let Err(err) = result {
    handle_error(err);
  }
}

fn handle_error(err: RailError) -> ! {
  print_error(&err);
  std::process::exit(err.exit_code().as_i32());
}
