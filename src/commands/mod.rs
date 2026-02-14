//! CLI commands for cargo-rail
//!
//! This module contains all user-facing command implementations:
//!
//! ## Dependency Unification
//! - **unify**: Eliminate workspace-hack crates via native workspace dependency unification
//!
//! ## Configuration Management
//! - **init**: Initialize cargo-rail configuration (rail.toml)
//! - **config**: Validate and manage configuration
//!
//! ## Split & Sync
//! - **split**: Split monorepo crates to separate repositories
//! - **sync**: Bidirectional sync between monorepo and split repos
//!
//! ## Inspection
//! - **plan**: Deterministic file-first planner (primary planning surface)
//! - **run**: Surface-driven executor using planner contract
//!
//! All commands accept `&WorkspaceContext` to avoid redundant workspace loads.

/// Clean up workspace artifacts
pub mod clean;
/// CLI argument definitions (clap structs) - internal, not part of stable API.
#[doc(hidden)]
pub mod cli;
/// Common utilities for command implementations
pub mod common;
/// Configuration management commands
pub mod config;
/// Planner reasoning graph command
pub mod graph;
/// Planner hash and diff introspection commands
pub mod hash;
/// Initialize cargo-rail configuration
pub mod init;
/// Deterministic file-first change planner
pub mod plan;
/// Release planning and publishing
pub mod release;
/// Surface-driven execution built on planner contract
pub mod run;
/// Split crates into standalone repositories
pub mod split;
/// Bidirectional sync between monorepo and split repos
pub mod sync;
/// Workspace dependency unification commands
pub mod unify;

pub use clean::run_clean;
#[doc(hidden)]
pub use cli::{CargoCli, Commands, RailCli, ReleaseCommand, SplitCommand, generate_completions};
pub use common::OutputFormat;
pub use config::{
  StrictnessMode, run_config_locate, run_config_print, run_config_sync, run_config_validate_standalone,
};
pub use graph::run_graph;
pub use hash::{run_diff_hash, run_hash};
pub use init::{run_init, run_init_standalone};
pub use plan::{PlanOptions, run_plan};
pub use release::{run_release_check, run_release_init, run_release_plan, run_release_publish};
pub use run::run_run;
pub use split::{run_split, run_split_init};
pub use sync::run_sync;
pub use unify::{run_unify_analyze, run_unify_apply, run_unify_undo};

use crate::error::RailResult;
use crate::workspace::WorkspaceContext;
use std::path::Path;

/// Result of attempting to dispatch a command without building WorkspaceContext.
#[doc(hidden)]
pub enum PreContextDispatch {
  /// The command ran and the process should exit.
  Handled,
  /// The command requires a WorkspaceContext to run.
  NeedsContext(Commands),
}

/// Handle commands that don't need WorkspaceContext.
///
/// Centralizes "pre-context" routing so `main.rs` stays thin.
#[doc(hidden)]
pub fn try_dispatch_pre_context(
  cmd: Commands,
  workspace_root: &Path,
  config_override: Option<&Path>,
  json: bool,
) -> RailResult<PreContextDispatch> {
  match cmd {
    Commands::Init { output, force, check } => {
      init::run_init_standalone(workspace_root, &output, force, check, json)?;
      Ok(PreContextDispatch::Handled)
    }

    Commands::Unify {
      command: Some(cli::UnifyCommand::Undo { list, backup_id }),
      ..
    } => {
      unify::run_unify_undo(workspace_root, list, backup_id)?;
      Ok(PreContextDispatch::Handled)
    }

    Commands::Config {
      command: cli::ConfigCommand::Sync { check, format },
    } => {
      config::run_config_sync(workspace_root, config_override, check, format)?;
      Ok(PreContextDispatch::Handled)
    }

    Commands::Config {
      command: cli::ConfigCommand::Validate {
        format,
        strict,
        no_strict,
      },
    } => {
      let strictness = if strict {
        StrictnessMode::Strict
      } else if no_strict {
        StrictnessMode::NoStrict
      } else {
        StrictnessMode::Auto
      };
      config::run_config_validate_standalone(workspace_root, config_override, format, strictness)?;
      Ok(PreContextDispatch::Handled)
    }

    Commands::Config {
      command: cli::ConfigCommand::Locate { format },
    } => {
      config::run_config_locate(workspace_root, config_override, format)?;
      Ok(PreContextDispatch::Handled)
    }

    Commands::Config {
      command: cli::ConfigCommand::Print { format },
    } => {
      config::run_config_print(workspace_root, config_override, format)?;
      Ok(PreContextDispatch::Handled)
    }

    Commands::Completions { shell } => {
      cli::generate_completions(shell);
      Ok(PreContextDispatch::Handled)
    }

    other => Ok(PreContextDispatch::NeedsContext(other)),
  }
}

/// Dispatch a command to its handler
///
/// This is the main command routing logic. It takes a parsed `Commands` enum
/// and the workspace context, then calls the appropriate handler.
pub fn dispatch(cmd: Commands, ctx: &WorkspaceContext) -> RailResult<()> {
  match cmd {
    Commands::Run {
      since,
      merge_base,
      all,
      surfaces,
      profile,
      workflow,
      dry_run,
      print_cmd,
      explain,
      ignore_bin_crates,
      skip_nextest,
      run_args,
    } => run_run(
      ctx,
      run::RunOptions {
        since,
        merge_base,
        all,
        surfaces,
        profile,
        workflow,
        dry_run,
        print_cmd,
        explain,
        ignore_bin_crates,
        skip_nextest,
        run_args,
      },
    ),

    Commands::Plan {
      since,
      from,
      to,
      merge_base,
      format,
      output,
      explain,
      confidence_profile,
    } => run_plan(
      ctx,
      PlanOptions {
        since,
        from,
        to,
        merge_base,
        format,
        output,
        explain,
        confidence_profile,
      },
    ),

    // Init is handled before WorkspaceContext is built
    Commands::Init { .. } => unreachable!("Init command should be handled before dispatch"),

    // Dependency Unification
    Commands::Unify {
      command,
      check,
      plan,
      format,
      backup,
      skip_report,
      report_path,
      output,
      show_diff,
      explain,
    } => {
      // Undo subcommand is handled before WorkspaceContext is built
      if command.is_some() {
        unreachable!("Undo subcommand should be handled before dispatch")
      } else if check {
        run_unify_analyze(ctx, show_diff, explain, format, output.as_ref())
      } else {
        run_unify_apply(ctx, backup, skip_report, report_path, plan)
      }
    }

    // Split/Sync
    Commands::Split { command } => match command {
      cli::SplitCommand::Init { crate_names, check } => {
        let crates = if crate_names.is_empty() {
          None
        } else {
          Some(crate_names)
        };
        run_split_init(ctx, crates, check)
      }
      cli::SplitCommand::Run {
        crate_name,
        all,
        remote,
        check,
        plan,
        allow_dirty,
        yes,
        format,
      } => run_split(
        ctx,
        split::SplitRunArgs {
          crate_name,
          all,
          remote,
          check,
          plan_path: plan,
          allow_dirty,
          yes,
          format,
        },
      ),
    },

    Commands::Sync {
      crate_name,
      all,
      remote,
      from_remote,
      to_remote,
      strategy,
      check,
      plan,
      allow_dirty,
      yes,
      format,
    } => run_sync(
      ctx,
      sync::SyncArgs {
        crate_name,
        all,
        remote,
        from_remote,
        to_remote,
        strategy,
        check,
        plan_path: plan,
        allow_dirty,
        yes,
        format,
      },
    ),

    // Release
    Commands::Release { command } => match command {
      cli::ReleaseCommand::Init { crate_names, check } => {
        let crates = if crate_names.is_empty() {
          None
        } else {
          Some(crate_names)
        };
        run_release_init(ctx, crates, check)
      }
      cli::ReleaseCommand::Run {
        crate_names,
        all,
        bump,
        check,
        plan,
        skip_publish,
        skip_tag,
        yes,
        format,
      } => {
        let names = if all || crate_names.is_empty() {
          None
        } else {
          Some(crate_names)
        };

        if check {
          run_release_plan(ctx, names, bump, skip_publish, skip_tag, format)
        } else {
          run_release_publish(
            ctx,
            release::ReleasePublishArgs {
              crate_names: names,
              all,
              bump,
              skip_publish,
              skip_tag,
              yes,
              plan_path: plan,
            },
          )
        }
      }
      cli::ReleaseCommand::Check {
        crate_names,
        all,
        extended,
        format,
      } => {
        let names = if all || crate_names.is_empty() {
          None
        } else {
          Some(crate_names)
        };
        run_release_check(ctx, names, all, extended, format)
      }
    },

    // Clean
    Commands::Clean {
      cache,
      backups,
      reports,
      check,
      format,
    } => run_clean(ctx, cache, backups, reports, check, format),

    // Config commands are handled before WorkspaceContext is built
    Commands::Config { command } => match command {
      cli::ConfigCommand::Locate { .. } => unreachable!("Config locate should be handled before dispatch"),
      cli::ConfigCommand::Print { .. } => unreachable!("Config print should be handled before dispatch"),
      cli::ConfigCommand::Validate { .. } => unreachable!("Config validate should be handled before dispatch"),
      cli::ConfigCommand::Sync { .. } => unreachable!("Config sync should be handled before dispatch"),
    },

    Commands::Hash {
      since,
      from,
      to,
      merge_base,
      confidence_profile,
      format,
    } => run_hash(
      ctx,
      hash::HashOptions {
        since,
        from,
        to,
        merge_base,
        confidence_profile,
        format,
      },
    ),

    Commands::DiffHash { a, b, format } => run_diff_hash(a, b, format),

    Commands::Graph {
      since,
      from,
      to,
      merge_base,
      confidence_profile,
      dot,
      output,
    } => run_graph(
      ctx,
      graph::GraphOptions {
        since,
        from,
        to,
        merge_base,
        confidence_profile,
        dot,
        output,
      },
    ),

    // Completions is handled before WorkspaceContext is built
    Commands::Completions { .. } => unreachable!("Completions should be handled before dispatch"),
  }
}
