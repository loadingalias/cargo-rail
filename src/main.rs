use cargo_rail::{commands, error, workspace};

use clap::{Parser, Subcommand};
use error::{RailError, print_error};

/// Monorepo tooling for Rust workspaces
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
    output: Option<std::path::PathBuf>,
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
    no_nextest: bool,
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
    /// Preview changes without modifying files
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
    /// Consolidate transitive-only crates with fragmented features
    #[arg(long)]
    consolidate_transitives: bool,
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
    no_report: bool,
    /// Custom path for the unify report (default: target/cargo-rail/unify-report.md)
    #[arg(long)]
    report_path: Option<std::path::PathBuf>,
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
    /// Preview generated config without writing
    #[arg(long, short = 'c')]
    check: bool,
  },

  /// Split a crate to a standalone repository with git history
  Split {
    /// Action: 'init' to configure splits, or crate name to split
    action: Option<String>,
    /// Additional crate name(s) for init
    crate_names: Vec<String>,
    /// Split all configured crates
    #[arg(short, long)]
    all: bool,
    /// Override remote repository
    #[arg(long)]
    remote: Option<String>,
    /// Preview changes without executing
    #[arg(long, short = 'c')]
    check: bool,
    /// Output format [text, json]
    #[arg(long, short = 'f', default_value = "text")]
    format: String,
  },

  /// Sync changes between monorepo and split repos
  Sync {
    /// Crate name to sync
    crate_name: Option<String>,
    /// Sync all configured crates
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
    /// Preview changes without executing
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
    /// Crate name(s) to release
    crate_names: Vec<String>,
    /// Release all workspace crates
    #[arg(short, long)]
    all: bool,
    /// Version bump [major, minor, patch, or "x.y.z"]
    #[arg(long, default_value = "patch")]
    bump: String,
    /// Preview release plan without executing
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
    /// Crate name(s) to check
    crate_names: Vec<String>,
    /// Check all workspace crates
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
    /// Preview what would be cleaned
    #[arg(long, short = 'c')]
    check: bool,
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
    check,
  } = cli.command
  {
    let result = commands::run_init_standalone(&workspace_root, &output, force, non_interactive, check);
    if let Err(e) = result {
      handle_error(e);
    }
    return;
  }

  // Handle unify undo command specially - it needs to work even with corrupted Cargo.toml files
  // (since that's exactly what it's trying to fix). This must be done BEFORE building
  // WorkspaceContext, which would fail if metadata can't be loaded.
  if let Commands::Unify {
    action: Some(ref act),
    list,
    ref backup_id,
    ..
  } = cli.command
    && act == "undo"
  {
    let result = commands::run_unify_undo(&workspace_root, list, backup_id.clone());
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
      all,
      output,
    } => commands::run_affected(&ctx, since, from, to, format, all, output),
    Commands::Test {
      since,
      all,
      no_nextest,
      explain,
      test_args,
    } => {
      let config = commands::test::TestConfig {
        since,
        all,
        explain,
        prefer_nextest: !no_nextest,
        test_args,
      };
      commands::run_test(&ctx, config)
    }

    // Configuration Management (Init is handled above before building WorkspaceContext)
    Commands::Init { .. } => unreachable!("Init command should be handled earlier"),

    // Dependency Unification
    Commands::Unify {
      action,
      check,
      format,
      exclude,
      include,
      backup,
      consolidate_transitives,
      include_renamed,
      list: _,
      backup_id: _,
      no_report,
      report_path,
      show_diff,
    } => {
      // Check if this is an undo action (should have been handled earlier)
      if let Some(act) = action {
        if act == "undo" {
          // This should have been handled earlier, before WorkspaceContext was built
          unreachable!("Undo command should be handled before workspace context creation")
        } else {
          Err(RailError::message(format!(
            "Unknown unify action '{}'. Valid actions: undo",
            act
          )))
        }
      } else if check {
        commands::run_unify_analyze(
          &ctx,
          exclude,
          include,
          consolidate_transitives,
          include_renamed,
          show_diff,
          format,
        )
      } else {
        commands::run_unify_apply(&ctx, exclude, include, backup, include_renamed, no_report, report_path)
      }
    }

    // Split/Sync
    Commands::Split {
      action,
      crate_names,
      all,
      remote,
      check,
      format,
    } => {
      // Handle init subcommand: cargo rail split init [crate1 crate2 ...]
      if action.as_deref() == Some("init") {
        let crates = if crate_names.is_empty() {
          None
        } else {
          Some(crate_names)
        };
        commands::run_split_init(&ctx, crates, check)
      } else {
        // Regular split command
        // If action is provided and not "init", treat it as the crate name
        let crate_name = action;
        commands::run_split(&ctx, crate_name, all, remote.clone(), check, format)
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
      check,
      format,
    } => commands::run_sync(
      &ctx,
      crate_name,
      all,
      remote,
      from_remote,
      to_remote,
      strategy,
      no_protected_branches,
      check,
      format,
    ),

    // Release
    Commands::Release {
      action,
      crate_names,
      all,
      bump,
      check,
      skip_publish,
      skip_tag,
      format,
    } => {
      // Handle init subcommand
      if action.as_deref() == Some("init") {
        // cargo rail release init <crates>
        let crates = if crate_names.is_empty() {
          None
        } else {
          Some(crate_names)
        };
        commands::run_release_init(&ctx, crates, check)
      } else {
        // Regular release command
        // If action is provided and not "init", treat it as the first crate name
        let mut all_crate_names = crate_names;
        if let Some(first_crate) = action {
          all_crate_names.insert(0, first_crate);
        }

        // If --all is specified OR no crate names provided, use None (all crates)
        let names = if all || all_crate_names.is_empty() {
          None
        } else {
          Some(all_crate_names)
        };

        if check {
          commands::run_release_plan(&ctx, names, bump, skip_publish, skip_tag, format)
        } else {
          // Execute mode: perform the release
          commands::run_release_publish(&ctx, names, all, bump, skip_publish, skip_tag)
        }
      }
    }

    Commands::Check {
      crate_names,
      all,
      format,
    } => {
      // If --all is specified OR no crate names provided, use None (all crates)
      let names = if all || crate_names.is_empty() {
        None
      } else {
        Some(crate_names)
      };
      commands::run_release_check(&ctx, names, all, format)
    }

    // Clean
    Commands::Clean {
      cache,
      backups,
      reports,
      check,
    } => commands::run_clean(&ctx, cache, backups, reports, check),
  };

  if let Err(err) = result {
    handle_error(err);
  }
}

fn handle_error(err: RailError) -> ! {
  print_error(&err);
  std::process::exit(err.exit_code().as_i32());
}
