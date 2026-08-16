//! CLI commands for cargo-rail
//!
//! This module contains all user-facing command implementations:
//!
//! ## Dependency Unification
//! - **unify**: Analyze and repair workspace dependency coherence
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
//!
//! All commands accept `&WorkspaceContext` to avoid redundant workspace loads.

pub(crate) mod cache;
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
/// Read-only workspace and toolchain diagnostics
pub mod doctor;
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
/// Split crates into standalone repositories
pub mod split;
/// Complete Rust declaration reachability and visibility analysis.
pub mod surface;
/// Bidirectional sync between monorepo and split repos
pub mod sync;
/// Workspace dependency unification commands
pub mod unify;

pub use change::{ChangeCheckOptions, run_change_add, run_change_check, run_change_status};
pub use clean::run_clean;
#[doc(hidden)]
pub use cli::{
  CacheCommand, CacheScope, CargoCli, ChangeCommand, Commands, DoctorCommand, RailCli, ReleaseCommand, SplitCommand,
  generate_completions,
};
pub use common::{ChangeOutputFormat, SplitOutputFormat, SurfaceOutputFormat, TextJsonOutputFormat};
pub use config::{
  StrictnessMode, run_config_explain, run_config_locate, run_config_migrate, run_config_print,
  run_config_validate_standalone,
};
pub use doctor::run_native_cache_doctor;
pub use graph::run_graph;
pub use hash::{run_diff_hash, run_hash};
pub use init::{run_init, run_init_standalone};
pub use plan::{PlanOptions, run_plan};
pub use release::{
  run_release_check, run_release_finalize, run_release_init, run_release_plan, run_release_publish,
  run_release_status_standalone,
};
pub use split::{run_split, run_split_init};
pub use surface::{SurfaceOptions, run_surface};
pub use sync::run_sync;
pub use unify::{UnifyAnalyzeOptions, run_unify_analyze, run_unify_apply, run_unify_doctor, run_unify_undo};

use crate::error::RailResult;
use crate::workspace::WorkspaceContext;
use std::path::{Path, PathBuf};

/// Result of attempting to dispatch a command without building WorkspaceContext.
#[doc(hidden)]
pub enum PreContextDispatch {
  /// The command ran and the process should exit.
  Handled,
  /// The command requires a WorkspaceContext to run.
  NeedsContext(PreparedContext),
}

/// A command paired with the context-construction contract it requires.
#[doc(hidden)]
pub struct PreparedContext {
  command: Box<Commands>,
  config_override: Option<PathBuf>,
}

impl PreparedContext {
  fn new(command: Commands, config_override: Option<&Path>) -> RailResult<Self> {
    Ok(Self {
      command: Box::new(command),
      config_override: config_override.map(Path::to_path_buf),
    })
  }

  /// Build the exact workspace context required by this command.
  #[doc(hidden)]
  pub fn build(self, workspace_root: &Path) -> RailResult<(Commands, WorkspaceContext)> {
    let context = if self.command.requires_workspace_snapshot() {
      WorkspaceContext::build_with_snapshot_and_config(workspace_root, self.config_override.as_deref())
    } else {
      WorkspaceContext::build_with_source_capture_and_config(
        workspace_root,
        self.command.requires_worktree_source_capture(),
        self.config_override.as_deref(),
      )
    };
    Ok((*self.command, context?))
  }
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

    Commands::Surface { schema: true, .. } => {
      surface::print_surface_schema();
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

    Commands::Cache { command } => {
      match command {
        cli::CacheCommand::Setup {
          local_dir,
          max_size,
          remote,
          remote_mode,
          remote_environment,
          local_only,
          check,
          format,
        } => cache::run_setup(
          workspace_root,
          crate::cache::installation::SetupRequest {
            local_dir,
            max_bytes: max_size,
            remote_url: remote,
            remote_mode,
            remote_environment,
            local_only,
          },
          check,
          format,
        )?,
        cli::CacheCommand::Normalize {
          url,
          mode,
          environment,
          format,
        } => cache::run_normalize(&url, mode.as_deref(), environment, format)?,
        cli::CacheCommand::Status { scope, format } => cache::run_status(workspace_root, scope, format)?,
        cli::CacheCommand::Clean { scope, check, format } => {
          cache::run_clean(workspace_root, scope, check, format)?;
        }
        cli::CacheCommand::Remove { check, format } => cache::run_remove(workspace_root, check, format)?,
      }
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
      Ok(PreContextDispatch::NeedsContext(PreparedContext::new(
        Commands::Release {
          command: cli::ReleaseCommand::Resume { state },
        },
        config_override,
      )?))
    }

    Commands::Release {
      command: cli::ReleaseCommand::Abort { state, yes },
    } => {
      crate::release::state::prepare_recovery(workspace_root, &state)?;
      Ok(PreContextDispatch::NeedsContext(PreparedContext::new(
        Commands::Release {
          command: cli::ReleaseCommand::Abort { state, yes },
        },
        config_override,
      )?))
    }

    other => Ok(PreContextDispatch::NeedsContext(PreparedContext::new(
      other,
      config_override,
    )?)),
  }
}

/// Dispatch a command to its handler
///
/// This is the main command routing logic. It takes a parsed `Commands` enum
/// and the workspace context, then calls the appropriate handler.
pub fn dispatch(cmd: Commands, ctx: &WorkspaceContext) -> RailResult<()> {
  match cmd {
    Commands::Doctor {
      command: cli::DoctorCommand::NativeCache { format },
    } => run_native_cache_doctor(ctx, format),

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

    Commands::Surface {
      check,
      fix,
      dry_run,
      backup,
      format,
      output,
      explain,
      schema: _,
    } => run_surface(
      ctx,
      SurfaceOptions {
        check,
        fix,
        dry_run,
        backup,
        format,
        output,
        explain,
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
      None if check => run_unify_analyze(
        ctx,
        UnifyAnalyzeOptions {
          show_diff,
          explain,
          format,
          output: output.as_ref(),
          backup,
          no_report: skip_report,
          report_path: report_path.as_ref(),
        },
      ),
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

    Commands::Cache { .. } => unreachable!("cache commands should be handled before context loading"),

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
