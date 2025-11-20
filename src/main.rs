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
  #[command(subcommand)]
  Unify(UnifyCommands),

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
    #[arg(long)]
    dry_run: bool,
  },

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
    /// Show plan without executing (default: execute with confirmation)
    #[arg(long)]
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
    #[arg(long)]
    dry_run: bool,
    /// Output plan in JSON format
    #[arg(long)]
    json: bool,
  },

  // ============================================================================
  // Configuration Management
  // ============================================================================
  /// Configuration management (sync rust-toolchain.toml, etc.)
  #[command(subcommand)]
  Config(ConfigCommands),

  // ============================================================================
  // Release & Publishing
  // ============================================================================
  /// Release automation (version bumping, changelog, publishing)
  #[command(subcommand)]
  Release(ReleaseCommands),

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
    /// Pin transitive-only crates with fragmented features
    #[arg(long)]
    pin_transitives: bool,
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
    /// Pin transitive-only crates with fragmented features
    #[arg(long)]
    pin_transitives: bool,
  },

  /// Check workspace dependencies are properly unified (for CI)
  Check {
    /// Exclude specific dependencies from check
    #[arg(long)]
    exclude: Vec<String>,
    /// Only check normal dependencies
    #[arg(long)]
    normal_only: bool,
    /// Enable per-target validation (runs cargo metadata for each target)
    #[arg(long)]
    validate_targets: bool,
  },
}

#[derive(Subcommand)]
enum ConfigCommands {
  /// Sync rust-toolchain.toml from rail.toml configuration
  Sync {
    /// Check if rust-toolchain.toml matches config (don't modify)
    #[arg(long)]
    check: bool,
  },
}

#[derive(Subcommand)]
enum ReleaseCommands {
  /// Plan a release (version bumping, changelog, validation) - dry-run mode
  Plan {
    /// Crate name(s) to release (omit for --all)
    crate_names: Vec<String>,
    /// Release all workspace crates
    #[arg(short, long)]
    all: bool,
    /// Version bump strategy: major, minor, patch, or explicit version (e.g., "1.2.3")
    #[arg(long, default_value = "patch")]
    bump: String,
    /// Output plan in JSON format
    #[arg(long)]
    json: bool,
  },

  /// Execute a release (publish to crates.io, create git tags, update changelogs)
  Publish {
    /// Crate name(s) to release (omit for --all)
    crate_names: Vec<String>,
    /// Release all workspace crates in dependency order
    #[arg(short, long)]
    all: bool,
    /// Version bump strategy: major, minor, patch, or explicit version (e.g., "1.2.3")
    #[arg(long, default_value = "patch")]
    bump: String,
    /// Execute the release (default is dry-run, requires this flag)
    #[arg(long, short = 'x')]
    execute: bool,
    /// Skip publishing to crates.io (only create tags and update changelogs)
    #[arg(long)]
    skip_publish: bool,
    /// Skip git tag creation
    #[arg(long)]
    skip_tag: bool,
  },

  /// Validate release readiness (for CI)
  Check {
    /// Crate name(s) to check (omit for --all)
    crate_names: Vec<String>,
    /// Check all workspace crates
    #[arg(short, long)]
    all: bool,
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
    Commands::Unify(unify_cmd) => match unify_cmd {
      UnifyCommands::Analyze {
        exclude,
        include,
        normal_only,
        pin_transitives,
      } => commands::run_unify_analyze(&ctx, exclude, include, normal_only, pin_transitives),
      UnifyCommands::Apply {
        exclude,
        include,
        backup,
        normal_only,
        pin_transitives,
      } => commands::run_unify_apply(&ctx, exclude, include, backup, normal_only, pin_transitives),
      UnifyCommands::Check {
        exclude,
        normal_only,
        validate_targets,
      } => commands::run_unify_check(&ctx, exclude, normal_only, validate_targets),
    },

    // Configuration Management
    Commands::Config(config_cmd) => match config_cmd {
      ConfigCommands::Sync { check } => commands::run_config_sync(&ctx, check),
    },

    // Split/Sync
    Commands::Split {
      crate_name,
      all,
      remote,
      dry_run,
      json,
    } => commands::run_split(&ctx, crate_name, all, remote, dry_run, json),
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
    Commands::Release(release_cmd) => match release_cmd {
      ReleaseCommands::Plan {
        crate_names,
        all,
        bump,
        json,
      } => {
        let names = if all { None } else { Some(crate_names) };
        commands::run_release_plan(&ctx, names, bump, json)
      }
      ReleaseCommands::Publish {
        crate_names,
        all,
        bump,
        execute,
        skip_publish,
        skip_tag,
      } => {
        let names = if all { None } else { Some(crate_names) };
        commands::run_release_publish(&ctx, names, all, bump, execute, skip_publish, skip_tag)
      }
      ReleaseCommands::Check { crate_names, all } => {
        let names = if all { None } else { Some(crate_names) };
        commands::run_release_check(&ctx, names, all)
      }
    },

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
