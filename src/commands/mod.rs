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

/// Intent-file management.
pub mod change;
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

pub use change::{ChangeCheckOptions, run_change_add, run_change_check, run_change_status};
pub use clean::run_clean;
#[doc(hidden)]
pub use cli::{CargoCli, ChangeCommand, Commands, RailCli, ReleaseCommand, SplitCommand, generate_completions};
pub use common::{ChangeOutputFormat, SplitOutputFormat, TextJsonOutputFormat};
pub use config::{
  StrictnessMode, run_config_explain, run_config_locate, run_config_migrate, run_config_print,
  run_config_validate_standalone,
};
pub use graph::run_graph;
pub use hash::{run_diff_hash, run_hash};
pub use init::{run_init, run_init_standalone};
pub use plan::{PlanOptions, run_plan};
pub use release::{
  run_release_check, run_release_finalize, run_release_init, run_release_plan, run_release_publish,
  run_release_status_standalone,
};
pub use run::run_run;
pub use split::{run_split, run_split_init};
pub use sync::run_sync;
pub use unify::{run_unify_analyze, run_unify_apply, run_unify_doctor, run_unify_undo};

use crate::error::RailResult;
use crate::workspace::WorkspaceContext;
use std::path::Path;

/// Result of attempting to dispatch a command without building WorkspaceContext.
#[doc(hidden)]
pub enum PreContextDispatch {
  /// The command ran and the process should exit.
  Handled,
  /// The command requires a WorkspaceContext to run.
  NeedsContext(Box<Commands>),
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
    Commands::Plan { schema: true, .. } => {
      plan::print_plan_schema();
      Ok(PreContextDispatch::Handled)
    }

    Commands::Init { output, force, dry_run } => {
      init::run_init_standalone(workspace_root, &output, force, dry_run, json)?;
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
      command: cli::ConfigCommand::Migrate { check, format },
    } => {
      config::run_config_migrate(workspace_root, config_override, check, format)?;
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

    Commands::Config {
      command: cli::ConfigCommand::Explain { format },
    } => {
      config::run_config_explain(workspace_root, config_override, format)?;
      Ok(PreContextDispatch::Handled)
    }

    Commands::Completions { shell } => {
      cli::generate_completions(shell);
      Ok(PreContextDispatch::Handled)
    }

    Commands::Release {
      command: cli::ReleaseCommand::Status { state, format },
    } => {
      release::run_release_status_standalone(workspace_root, state.as_deref(), format)?;
      Ok(PreContextDispatch::Handled)
    }

    Commands::Release {
      command: cli::ReleaseCommand::Resume { state },
    } => {
      if state.exists() {
        crate::release::state::prepare_recovery(workspace_root, &state)?;
      }
      Ok(PreContextDispatch::NeedsContext(Box::new(Commands::Release {
        command: cli::ReleaseCommand::Resume { state },
      })))
    }

    Commands::Release {
      command: cli::ReleaseCommand::Abort { state, yes },
    } => {
      crate::release::state::prepare_recovery(workspace_root, &state)?;
      Ok(PreContextDispatch::NeedsContext(Box::new(Commands::Release {
        command: cli::ReleaseCommand::Abort { state, yes },
      })))
    }

    other => Ok(PreContextDispatch::NeedsContext(Box::new(other))),
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
      actions,
      profile,
      workflow,
      dry_run,
      format,
      generated,
      print_cmd,
      explain,
      ignore_bin_crates,
      skip_nextest,
      test_runner,
      cargo_test_args,
      nextest_args,
      test_filter,
      run_args,
    } => run_run(
      ctx,
      run::RunOptions {
        since,
        merge_base,
        all,
        actions,
        profile,
        workflow,
        dry_run,
        format,
        generated,
        print_cmd,
        explain,
        ignore_bin_crates,
        skip_nextest,
        test_runner,
        cargo_test_args,
        nextest_args,
        test_filter,
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
      schema: _,
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
    } => match command {
      Some(cli::UnifyCommand::Doctor { format }) => run_unify_doctor(ctx, format),
      Some(cli::UnifyCommand::Undo { .. }) => unreachable!("Undo subcommand should be handled before dispatch"),
      None if check => run_unify_analyze(ctx, show_diff, explain, format, output.as_ref()),
      None => run_unify_apply(ctx, backup, skip_report, report_path, plan, format),
    },

    // Split/Sync
    Commands::Split { command } => match command {
      cli::SplitCommand::Init { crate_names, dry_run } => {
        let crates = if crate_names.is_empty() {
          None
        } else {
          Some(crate_names)
        };
        run_split_init(ctx, crates, dry_run)
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
      resume,
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
        resume,
        allow_dirty,
        yes,
        format,
      },
    ),

    Commands::Change { command } => match command {
      cli::ChangeCommand::Add {
        crate_names,
        bump,
        message,
        name,
        format,
      } => run_change_add(ctx, crate_names, bump, message, name, format),
      cli::ChangeCommand::Status { format } => run_change_status(ctx, format),
      cli::ChangeCommand::Check {
        since,
        merge_base,
        all,
        required,
        format,
      } => run_change_check(
        ctx,
        ChangeCheckOptions {
          since,
          merge_base,
          all,
          required,
          format,
        },
      ),
    },

    // Release
    Commands::Release { command } => match command {
      cli::ReleaseCommand::Init { crate_names, dry_run } => {
        let crates = if crate_names.is_empty() {
          None
        } else {
          Some(crate_names)
        };
        run_release_init(ctx, crates, dry_run)
      }
      cli::ReleaseCommand::Run {
        crate_names,
        all,
        bump,
        check,
        plan,
        skip_publish,
        skip_tag,
        pr,
        include_dependents,
        yes,
        format,
      } => {
        let names = if all || crate_names.is_empty() {
          None
        } else {
          Some(crate_names)
        };

        if check {
          run_release_plan(ctx, names, bump, skip_publish, skip_tag, include_dependents, format)
        } else {
          run_release_publish(
            ctx,
            release::ReleasePublishArgs {
              crate_names: names,
              all,
              bump,
              skip_publish,
              skip_tag,
              pr,
              include_dependents,
              yes,
              plan_path: plan,
              format,
            },
          )
        }
      }
      cli::ReleaseCommand::Check {
        crate_names,
        all,
        extended,
        include_dependents,
        format,
      } => {
        let names = if all || crate_names.is_empty() {
          None
        } else {
          Some(crate_names)
        };
        run_release_check(ctx, names, all, extended, include_dependents, format)
      }
      cli::ReleaseCommand::Finalize {
        crate_names,
        all,
        skip_publish,
        skip_tag,
        include_dependents,
        yes,
        format,
      } => {
        let names = if all || crate_names.is_empty() {
          None
        } else {
          Some(crate_names)
        };
        release::run_release_finalize(ctx, names, all, skip_publish, skip_tag, include_dependents, yes, format)
      }
      cli::ReleaseCommand::Resume { state } => release::run_release_resume(ctx, &state),
      cli::ReleaseCommand::Status { .. } => unreachable!("release status should be handled before context loading"),
      cli::ReleaseCommand::Abort { state, yes } => release::run_release_abort(ctx, &state, yes),
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
      cli::ConfigCommand::Explain { .. } => unreachable!("Config explain should be handled before dispatch"),
      cli::ConfigCommand::Validate { .. } => unreachable!("Config validate should be handled before dispatch"),
      cli::ConfigCommand::Migrate { .. } => unreachable!("Config migrate should be handled before dispatch"),
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
