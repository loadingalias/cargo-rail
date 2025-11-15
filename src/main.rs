mod cargo;
mod checks;
mod commands;
mod core;
mod graph;
mod lint;
mod release;
mod ui;
mod utils;

use clap::{Parser, Subcommand};
use core::error::{RailError, print_error};

/// Split Rust crates from monorepos, keep them in sync
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
  // Setup & Inspection
  // ============================================================================
  /// Initialize cargo-rail configuration for a workspace
  Init {
    /// Process all crates in the workspace
    #[arg(short, long)]
    all: bool,
  },

  /// Run health checks and diagnostics
  Doctor {
    /// Run thorough checks (includes network tests)
    #[arg(long)]
    thorough: bool,
    /// Output results in JSON format
    #[arg(long)]
    json: bool,
  },

  /// Show status of all configured crates
  Status {
    /// Output status in JSON format
    #[arg(long)]
    json: bool,
  },

  /// Inspect git-notes mappings for a crate
  Mappings {
    /// Name of the crate to inspect
    crate_name: String,
    /// Validate mapping integrity
    #[arg(long)]
    check: bool,
    /// Output mappings in JSON format
    #[arg(long)]
    json: bool,
  },

  // ============================================================================
  // Split/Sync (Pillar 2)
  // ============================================================================
  /// Split a crate from monorepo to separate repo with history
  Split {
    /// Name of the crate to split
    crate_name: Option<String>,
    /// Split all configured crates
    #[arg(short, long)]
    all: bool,
    /// Override remote repository path (useful for testing)
    #[arg(long)]
    remote: Option<String>,
    /// Actually perform the split (default: dry-run mode showing plan)
    #[arg(long)]
    apply: bool,
    /// Output plan in JSON format (useful for CI/automation)
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
    /// Override remote repository path (useful for testing)
    #[arg(long)]
    remote: Option<String>,
    /// Only sync from remote to monorepo
    #[arg(long)]
    from_remote: bool,
    /// Only sync from monorepo to remote
    #[arg(long)]
    to_remote: bool,
    /// Conflict resolution strategy: ours (use monorepo), theirs (use remote), manual (create markers), union (combine both)
    #[arg(long, visible_alias = "conflict", default_value = "manual")]
    strategy: String,
    /// Disable protected branch checks (useful for testing)
    #[arg(long)]
    no_protected_branches: bool,
    /// Actually perform the sync (default: dry-run mode showing plan)
    #[arg(long)]
    apply: bool,
    /// Output plan in JSON format (useful for CI/automation)
    #[arg(long)]
    json: bool,
  },

  // ============================================================================
  // Graph Orchestration (Pillar 1)
  // ============================================================================
  /// Graph-aware workspace operations (affected analysis, smart CI)
  #[command(subcommand)]
  Graph(GraphCommands),

  // ============================================================================
  // Policy & Linting (Pillar 3)
  // ============================================================================
  /// Workspace policy enforcement and linting
  #[command(subcommand)]
  Lint(LintCommands),

  // ============================================================================
  // Release & Publishing (Pillar 4)
  // ============================================================================
  /// Release planning and publishing orchestration
  #[command(subcommand)]
  Release(ReleaseCommands),
}

// Graph subcommands (Pillar 1)
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
    /// Output format: text (default), json, names-only
    #[arg(long, default_value = "text")]
    format: String,
    /// Show dry-run plan without execution
    #[arg(long)]
    dry_run: bool,
  },

  /// Run tests for affected crates
  Test {
    /// Git ref to compare against (default: origin/main)
    #[arg(long)]
    since: Option<String>,
    /// Run tests for entire workspace instead of affected crates
    #[arg(long)]
    workspace: bool,
    /// Show dry-run plan without execution
    #[arg(long)]
    dry_run: bool,
    /// Additional arguments to pass to cargo test
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    cargo_args: Vec<String>,
  },

  /// Run check for affected crates
  Check {
    /// Git ref to compare against (default: origin/main)
    #[arg(long)]
    since: Option<String>,
    /// Run check for entire workspace instead of affected crates
    #[arg(long)]
    workspace: bool,
    /// Show dry-run plan without execution
    #[arg(long)]
    dry_run: bool,
    /// Additional arguments to pass to cargo check
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    cargo_args: Vec<String>,
  },

  /// Run clippy for affected crates
  Clippy {
    /// Git ref to compare against (default: origin/main)
    #[arg(long)]
    since: Option<String>,
    /// Run clippy for entire workspace instead of affected crates
    #[arg(long)]
    workspace: bool,
    /// Show dry-run plan without execution
    #[arg(long)]
    dry_run: bool,
    /// Additional arguments to pass to cargo clippy
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    cargo_args: Vec<String>,
  },
}

// Lint subcommands (Pillar 3)
#[derive(Subcommand)]
enum LintCommands {
  /// Check workspace dependency usage and inheritance
  Deps {
    /// Automatically fix issues (requires --apply)
    #[arg(long)]
    fix: bool,
    /// Actually apply fixes (default: dry-run)
    #[arg(long)]
    apply: bool,
    /// Output results in JSON format
    #[arg(long)]
    json: bool,
    /// Treat warnings as errors (exit code 1)
    #[arg(long)]
    strict: bool,
  },

  /// Detect and fix duplicate dependency versions
  Versions {
    /// Automatically fix issues (requires --apply)
    #[arg(long)]
    fix: bool,
    /// Actually apply fixes (default: dry-run)
    #[arg(long)]
    apply: bool,
    /// Output results in JSON format
    #[arg(long)]
    json: bool,
    /// Treat warnings as errors (exit code 1)
    #[arg(long)]
    strict: bool,
  },

  /// Validate Cargo.toml manifest quality and policy compliance
  Manifest {
    /// Output results in JSON format
    #[arg(long)]
    json: bool,
    /// Treat warnings as errors (exit code 1)
    #[arg(long)]
    strict: bool,
  },
}

// Release subcommands (Pillar 4)
#[derive(Subcommand)]
enum ReleaseCommands {
  /// Plan a release (analyze changes, suggest version bump)
  Plan {
    /// Name of the release to plan (from rail.toml `[[releases]]`)
    name: Option<String>,
    /// Plan all configured releases
    #[arg(short, long)]
    all: bool,
    /// Output results in JSON format
    #[arg(long)]
    json: bool,
  },

  /// Apply a release (update version, create tag, sync to split)
  Apply {
    /// Name of the release to apply
    name: String,
    /// Show what would happen without making changes
    #[arg(long)]
    dry_run: bool,
    /// Skip syncing to split repo (if configured)
    #[arg(long)]
    skip_sync: bool,
  },
}

fn get_styles() -> clap::builder::Styles {
  clap::builder::Styles::styled()
    .usage(
      anstyle::Style::new()
        .bold()
        .underline()
        .fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Yellow))),
    )
    .header(
      anstyle::Style::new()
        .bold()
        .underline()
        .fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Yellow))),
    )
    .literal(anstyle::Style::new().fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Green))))
    .invalid(
      anstyle::Style::new()
        .bold()
        .fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Red))),
    )
    .error(
      anstyle::Style::new()
        .bold()
        .fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Red))),
    )
    .valid(
      anstyle::Style::new()
        .bold()
        .underline()
        .fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Green))),
    )
    .placeholder(anstyle::Style::new().fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::White))))
}

fn main() {
  let CargoCli::Rail(cli) = CargoCli::parse();

  // Build workspace context once (loads metadata, graph, config)
  // Some commands don't need all of this, but the cost is negligible vs. the benefit
  // of a unified, single-load architecture
  let workspace_root = match std::env::current_dir() {
    Ok(dir) => dir,
    Err(e) => {
      eprintln!("Error: Failed to get current directory: {}", e);
      std::process::exit(1);
    }
  };

  let ctx = match core::context::WorkspaceContext::build(&workspace_root) {
    Ok(ctx) => ctx,
    Err(e) => {
      // Some commands (like init) may run before rail.toml exists
      // For those, we'll handle the error in the command itself
      // For now, print a warning but continue
      if !matches!(cli.command, Commands::Init { .. }) {
        eprintln!("Warning: Could not build full workspace context: {}", e);
      }
      // Create a minimal context for commands that can run without full setup
      match try_minimal_context(&workspace_root) {
        Ok(ctx) => ctx,
        Err(e) => {
          handle_error(e);
        }
      }
    }
  };

  let result = match cli.command {
    // Setup & Inspection
    Commands::Init { all } => commands::run_init(all),
    Commands::Doctor { thorough, json } => commands::run_doctor(thorough, json),
    Commands::Status { json } => commands::run_status(json),
    Commands::Mappings {
      crate_name,
      check,
      json,
    } => commands::run_mappings(crate_name, check, json),

    // Split/Sync (Pillar 2)
    Commands::Split {
      crate_name,
      all,
      remote,
      apply,
      json,
    } => commands::run_split(crate_name, all, remote, apply, json),
    Commands::Sync {
      crate_name,
      all,
      remote,
      from_remote,
      to_remote,
      strategy,
      no_protected_branches,
      apply,
      json,
    } => commands::run_sync(
      crate_name,
      all,
      remote,
      from_remote,
      to_remote,
      strategy,
      no_protected_branches,
      apply,
      json,
    ),

    // Graph Commands (Pillar 1) - New grouped interface
    Commands::Graph(graph_cmd) => match graph_cmd {
      GraphCommands::Affected {
        since,
        from,
        to,
        format,
        dry_run,
      } => commands::run_affected(&ctx, since, from, to, format, dry_run),
      GraphCommands::Test {
        since,
        workspace,
        dry_run,
        cargo_args,
      } => commands::run_test(&ctx, since, workspace, dry_run, cargo_args),
      GraphCommands::Check {
        since,
        workspace,
        dry_run,
        cargo_args,
      } => commands::run_check(&ctx, since, workspace, dry_run, cargo_args),
      GraphCommands::Clippy {
        since,
        workspace,
        dry_run,
        cargo_args,
      } => commands::run_clippy(&ctx, since, workspace, dry_run, cargo_args),
    },

    // Lint Commands (Pillar 3)
    Commands::Lint(lint_cmd) => match lint_cmd {
      LintCommands::Deps {
        fix,
        apply,
        json,
        strict,
      } => commands::run_lint_deps(fix, apply, json, strict),
      LintCommands::Versions {
        fix,
        apply,
        json,
        strict,
      } => commands::run_lint_versions(fix, apply, json, strict),
      LintCommands::Manifest { json, strict } => commands::run_lint_manifest(json, strict),
    },

    // Release Commands (Pillar 4)
    Commands::Release(release_cmd) => match release_cmd {
      ReleaseCommands::Plan { name, all, json } => commands::run_release_plan(name, all, json),
      ReleaseCommands::Apply {
        name,
        dry_run,
        skip_sync,
      } => commands::run_release_apply(name, dry_run, skip_sync),
    },
  };

  if let Err(err) = result {
    handle_error(err);
  }
}

fn handle_error(err: RailError) -> ! {
  print_error(&err);
  std::process::exit(err.exit_code().as_i32());
}

/// Try to build a minimal context when full context fails
/// This allows commands like init to run before rail.toml exists
fn try_minimal_context(workspace_root: &std::path::Path) -> core::error::RailResult<core::context::WorkspaceContext> {
  use crate::cargo::metadata::WorkspaceMetadata;
  use crate::graph::workspace_graph::WorkspaceGraph;
  use std::sync::Arc;

  let metadata = WorkspaceMetadata::load(workspace_root)?;
  let graph = Arc::new(WorkspaceGraph::load(workspace_root)?);

  Ok(core::context::WorkspaceContext {
    root: workspace_root.to_path_buf(),
    metadata,
    graph,
    config: None, // No config - this is minimal mode
  })
}
