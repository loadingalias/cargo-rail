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
    /// Output format: text, json, names-only, github, github-matrix, jsonl
    #[arg(long, default_value = "text")]
    format: String,
    /// Show all workspace crates (ignore changes)
    #[arg(long, short = 'a')]
    all: bool,
    /// Write output to file (e.g., $GITHUB_OUTPUT)
    #[arg(long, short = 'o')]
    output_file: Option<std::path::PathBuf>,
  },

  /// Run tests only for affected crates (smart test runner)
  Test {
    /// Git ref to compare against (auto-detects origin/main, origin/master, or HEAD~1)
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

  /// Workspace dependency unification (eliminates workspace-hack crates)
  ///
  /// Usage:
  ///   cargo rail unify                 - Analyze and apply unification
  ///   cargo rail unify --check         - Preview unification plan (no changes)
  ///   cargo rail unify undo            - Restore most recent backup
  ///   cargo rail unify undo --list     - List available backups
  ///   cargo rail unify undo --backup <id> - Restore specific backup
  Unify {
    /// Action: 'undo' to restore a backup
    action: Option<String>,
    /// Show plan without executing (check mode)
    #[arg(long, short = 'c')]
    check: bool,
    /// Exclude specific dependencies from unification
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
    /// Show diff of changes to each manifest (in check mode)
    #[arg(long)]
    show_diff: bool,
  },

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
    #[arg(long, short = 'c')]
    check: bool,
  },

  /// Split a crate from monorepo to separate repo with history
  ///
  /// Usage:
  ///   cargo rail split init             - Initialize split config for all workspace crates
  ///   cargo rail split <crate>          - Execute split for a crate
  ///   cargo rail split --all            - Execute split for all configured crates
  ///   cargo rail split --check          - Preview split operations
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
    #[arg(long, short = 'c')]
    check: bool,
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
    #[arg(long, short = 'c')]
    check: bool,
    /// Output plan in JSON format
    #[arg(long)]
    json: bool,
  },

  /// Release automation (version bumping, changelog, publishing)
  Release {
    /// Optional action: init (configure release settings for crates)
    action: Option<String>,
    /// Crate name(s) to release (omit for --all)
    crate_names: Vec<String>,
    /// Release all workspace crates in dependency order
    #[arg(short, long)]
    all: bool,
    /// Version bump strategy: major, minor, patch, or explicit version (e.g., "1.2.3")
    #[arg(long, default_value = "patch")]
    bump: String,
    /// Show plan without executing (check mode)
    #[arg(long, short = 'c')]
    check: bool,
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

  /// Clean workspace artifacts (cache, backups, reports)
  Clean {
    /// Clean only metadata cache
    #[arg(long)]
    cache: bool,
    /// Clean/prune backups (default: prune, --all: delete all)
    #[arg(long)]
    backups: bool,
    /// Clean generated reports
    #[arg(long)]
    reports: bool,
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
      output_file,
    } => commands::run_affected(&ctx, since, from, to, format, all, output_file),
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
        )
      } else {
        commands::run_unify_apply(&ctx, exclude, include, backup, include_renamed, no_report, report_path)
      }
    }

    // Split/Sync
    Commands::Split {
      crate_name,
      all,
      remote,
      check,
      json,
    } => {
      // Check if this is 'split init <crates>'
      if let Some(name) = crate_name {
        if name == "init" {
          // cargo rail split init (all crates)
          commands::run_split_init(&ctx, None, check)
        } else if name.starts_with("init,") || name.starts_with("init ") {
          // cargo rail split "init,crate1,crate2" (specific crates)
          let crates = name
            .strip_prefix("init,")
            .or_else(|| name.strip_prefix("init "))
            .unwrap()
            .trim();
          commands::run_split_init(&ctx, Some(crates), check)
        } else {
          // cargo rail split mycrate (regular split)
          commands::run_split(&ctx, Some(name.clone()), all, remote.clone(), check, json)
        }
      } else {
        // cargo rail split --all or cargo rail split --check
        commands::run_split(&ctx, None, all, remote.clone(), check, json)
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
      check,
      json,
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
      json,
    } => {
      // Handle init subcommand
      if action.as_deref() == Some("init") {
        // cargo rail release init <crates>
        let crates_str = if crate_names.is_empty() {
          None
        } else {
          Some(crate_names.join(","))
        };
        commands::run_release_init(&ctx, crates_str.as_deref(), check)
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
          commands::run_release_plan(&ctx, names, bump, skip_publish, skip_tag, json)
        } else {
          // Execute mode: perform the release
          commands::run_release_publish(&ctx, names, all, bump, skip_publish, skip_tag)
        }
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

    // Clean
    Commands::Clean {
      cache,
      backups,
      reports,
    } => commands::run_clean(&ctx, cache, backups, reports),
  };

  if let Err(err) = result {
    handle_error(err);
  }
}

fn handle_error(err: RailError) -> ! {
  print_error(&err);
  std::process::exit(err.exit_code().as_i32());
}
