use cargo_rail::{commands, error, workspace};

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

  /// Run tests only for affected crates (smart test runner)
  Test {
    /// Git ref to compare against (auto-detects origin/main, origin/master, or HEAD~1)
    #[arg(long)]
    since: Option<String>,
    /// Use cargo-nextest if available
    #[arg(long)]
    nextest: bool,
    /// Explain why tests are being run
    #[arg(long)]
    explain: bool,
    /// Watch for file changes and re-run tests automatically
    #[arg(long)]
    watch: bool,
    /// Watch mode backend: bacon, cargo-watch, auto (default: auto)
    #[arg(long, default_value = "auto")]
    watch_mode: String,
    /// Pass additional arguments to the test runner
    #[arg(last = true)]
    test_args: Vec<String>,
  },

  // ============================================================================
  // Dependency Unification
  // ============================================================================
  /// Workspace dependency unification (eliminates workspace-hack crates)
  Unify {
    /// Show plan without executing (analyze mode)
    #[arg(long, visible_alias = "dr", short = 'd')]
    dry_run: bool,
    /// Exclude specific dependencies from unification
    #[arg(long)]
    exclude: Vec<String>,
    /// Force include specific dependencies
    #[arg(long)]
    include: Vec<String>,
    /// Create .bak backups of all modified files
    #[arg(long)]
    backup: bool,
    /// Consolidate transitive-only crates with fragmented features
    #[arg(long)]
    consolidate_transitives: bool,
  },

  // ============================================================================
  // Configuration Management
  // ============================================================================
  /// Initialize cargo-rail configuration (rail.toml)
  Init {
    /// Output path for rail.toml (default: .config/rail.toml)
    #[arg(long, short, default_value = ".config/rail.toml")]
    output: String,
    /// Overwrite existing rail.toml
    #[arg(long)]
    force: bool,
    /// Skip interactive prompts and use all defaults
    #[arg(long)]
    non_interactive: bool,
    /// Output the generated config to stdout instead of writing to file
    #[arg(long, visible_alias = "dr", short = 'd')]
    dry_run: bool,
  },

  // ============================================================================
  // Split/Sync Orchestration
  // ============================================================================
  /// Split a crate from monorepo to separate repo with history
  ///
  /// Usage:
  ///   cargo rail split init <crate>     - Initialize split config for crate(s)
  ///   cargo rail split <crate>          - Execute split for a crate
  ///   cargo rail split --all            - Execute split for all configured crates
  ///   cargo rail split --dry-run        - Preview split operations
  Split {
    /// Crate name to split, or 'init' to configure splits
    crate_name: Option<String>,
    /// Split all configured crates
    #[arg(short, long)]
    all: bool,
    /// Override remote repository path
    #[arg(long)]
    remote: Option<String>,
    /// Show plan without executing (default: execute with confirmation)
    #[arg(long, visible_alias = "dr", short = 'd')]
    dry_run: bool,
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
    /// Disable protected branch checks (allow direct commits to main/master)
    #[arg(long)]
    no_protected_branches: bool,
    /// Show plan without executing (default: execute with confirmation)
    #[arg(long, visible_alias = "dr", short = 'd')]
    dry_run: bool,
    /// Output plan in JSON format
    #[arg(long)]
    json: bool,
  },

  // ============================================================================
  // Release & Publishing
  // ============================================================================
  /// Release automation (version bumping, changelog, publishing)
  Release {
    /// Crate name(s) to release (omit for --all)
    crate_names: Vec<String>,
    /// Release all workspace crates in dependency order
    #[arg(short, long)]
    all: bool,
    /// Version bump strategy: major, minor, patch, or explicit version (e.g., "1.2.3")
    #[arg(long, default_value = "patch")]
    bump: String,
    /// Show plan without executing (dry-run mode)
    #[arg(long, visible_alias = "dr", short = 'd')]
    dry_run: bool,
    /// Skip publishing to crates.io (only create tags and update changelogs)
    #[arg(long)]
    skip_publish: bool,
    /// Skip git tag creation
    #[arg(long)]
    skip_tag: bool,
    /// Output plan in JSON format
    #[arg(long)]
    json: bool,
  },

  /// Validate release readiness (for CI)
  Check {
    /// Crate name(s) to check (omit for --all)
    crate_names: Vec<String>,
    /// Check all workspace crates
    #[arg(short, long)]
    all: bool,
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

fn get_styles() -> clap::builder::Styles {
  clap::builder::Styles::styled()
}

fn main() {
  let CargoCli::Rail(cli) = CargoCli::parse();

  // Get workspace root
  let workspace_root = match std::env::current_dir() {
    Ok(dir) => dir,
    Err(e) => {
      eprintln!("Error: Failed to get current directory: {}", e);
      std::process::exit(1);
    }
  };

  // Handle init command specially - it doesn't require a valid workspace
  if let Commands::Init {
    output,
    force,
    non_interactive,
    dry_run,
  } = cli.command
  {
    let result = commands::run_init_standalone(&workspace_root, &output, force, non_interactive, dry_run);
    if let Err(e) = result {
      handle_error(e);
    }
    return;
  }

  // Build workspace context once (single-load pattern) for all other commands
  let ctx = match workspace::WorkspaceContext::build(&workspace_root) {
    Ok(ctx) => ctx,
    Err(e) => {
      handle_error(e);
    }
  };

  let result = match cli.command {
    // Graph Commands
    Commands::Affected {
      since,
      from,
      to,
      format,
    } => commands::run_affected(&ctx, since, from, to, format, false),
    Commands::Test {
      since,
      nextest,
      explain,
      watch,
      watch_mode,
      test_args,
    } => {
      let config = commands::test::TestConfig {
        since,
        explain,
        prefer_nextest: nextest,
        test_args,
      };

      if watch {
        // Parse watch mode
        let mode = match watch_mode.as_str() {
          "bacon" => commands::watch::WatchMode::Bacon,
          "cargo-watch" => commands::watch::WatchMode::CargoWatch,
          "auto" => commands::watch::WatchMode::Auto,
          _ => commands::watch::WatchMode::Auto,
        };
        commands::run_test_watch(&ctx, config, mode)
      } else {
        commands::run_test(&ctx, config)
      }
    }

    // Configuration Management (Init is handled above before building WorkspaceContext)
    Commands::Init { .. } => unreachable!("Init command should be handled earlier"),

    // Dependency Unification
    Commands::Unify {
      dry_run,
      exclude,
      include,
      backup,
      consolidate_transitives,
    } => {
      if dry_run {
        commands::run_unify_analyze(&ctx, exclude, include, consolidate_transitives)
      } else {
        commands::run_unify_apply(&ctx, exclude, include, backup, consolidate_transitives)
      }
    }

    // Split/Sync
    Commands::Split {
      crate_name,
      all,
      remote,
      dry_run,
      json,
    } => {
      // Check if this is 'split init <crates>'
      if let Some(name) = crate_name {
        if name == "init" {
          // cargo rail split init (all crates)
          commands::run_split_init(&ctx, None, dry_run)
        } else if name.starts_with("init,") || name.starts_with("init ") {
          // cargo rail split "init,crate1,crate2" (specific crates)
          let crates = name
            .strip_prefix("init,")
            .or_else(|| name.strip_prefix("init "))
            .unwrap()
            .trim();
          commands::run_split_init(&ctx, Some(crates), dry_run)
        } else {
          // cargo rail split mycrate (regular split)
          commands::run_split(&ctx, Some(name.clone()), all, remote.clone(), dry_run, json)
        }
      } else {
        // cargo rail split --all or cargo rail split --dry-run
        commands::run_split(&ctx, None, all, remote.clone(), dry_run, json)
      }
    }
    Commands::Sync {
      crate_name,
      all,
      remote,
      from_remote,
      to_remote,
      strategy,
      no_protected_branches,
      dry_run,
      json,
    } => commands::run_sync(
      &ctx,
      crate_name,
      all,
      remote,
      from_remote,
      to_remote,
      strategy,
      no_protected_branches,
      dry_run,
      json,
    ),

    // Release
    Commands::Release {
      crate_names,
      all,
      bump,
      dry_run,
      skip_publish,
      skip_tag,
      json,
    } => {
      // If --all is specified OR no crate names provided, use None (all crates)
      let names = if all || crate_names.is_empty() {
        None
      } else {
        Some(crate_names)
      };

      if dry_run {
        // Dry-run mode: show plan
        commands::run_release_plan(&ctx, names, bump, json)
      } else {
        // Execute mode: perform the release
        commands::run_release_publish(&ctx, names, all, bump, true, skip_publish, skip_tag)
      }
    }

    Commands::Check { crate_names, all } => {
      // If --all is specified OR no crate names provided, use None (all crates)
      let names = if all || crate_names.is_empty() {
        None
      } else {
        Some(crate_names)
      };
      commands::run_release_check(&ctx, names, all)
    }

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
