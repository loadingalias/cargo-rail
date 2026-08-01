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
pub use cli::{
  CacheCommand, CacheScope, CargoCli, ChangeCommand, Commands, DoctorCommand, RailCli, ReleaseCommand, SplitCommand,
  generate_completions,
};
pub use common::{ChangeOutputFormat, SplitOutputFormat, TextJsonOutputFormat};
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
  NeedsContext(PreparedContext),
}

#[derive(Clone, Copy)]
enum ContextPreparation {
  Standard,
  HermeticBuild,
}

/// A command paired with the context-construction contract it requires.
#[doc(hidden)]
pub struct PreparedContext {
  command: Box<Commands>,
  preparation: ContextPreparation,
  pre_context_cache_request: bool,
}

impl PreparedContext {
  fn new(command: Commands, pre_context_cache_request: bool) -> RailResult<Self> {
    let preparation = match &command {
      Commands::Run {
        actions,
        profile,
        workflow,
        dry_run: false,
        hermetic: true,
        format,
        run_args,
        ..
      } => {
        if format.is_json_like() {
          return Err(crate::error::RailError::with_help(
            "structured run output is a non-executing action plan",
            "add --dry-run when using --format json or --format github",
          ));
        }
        if profile.is_some()
          || workflow.is_some()
          || actions.is_empty()
          || actions.iter().any(|action| action != "build")
        {
          let requested = actions.first().map_or_else(
            || profile.as_deref().or(workflow.as_deref()).unwrap_or("default"),
            String::as_str,
          );
          let kind =
            crate::action::ActionKind::from_name(requested).map_or("configured", crate::action::ActionKind::as_str);
          return Err(crate::error::RailError::with_help(
            format!("hermetic execution does not yet support action '{requested}' ({kind})"),
            "use the explicit built-in build action (`--action build`); other action classes remain explicitly uncacheable",
          ));
        }
        validate_hermetic_run_arguments(run_args)?;
        ContextPreparation::HermeticBuild
      }
      _ => ContextPreparation::Standard,
    };
    Ok(Self {
      command: Box::new(command),
      preparation,
      pre_context_cache_request,
    })
  }

  /// Build the exact workspace context required by this command.
  #[doc(hidden)]
  pub fn build(self, workspace_root: &Path) -> RailResult<Option<(Commands, WorkspaceContext, bool)>> {
    let context = match self.preparation {
      ContextPreparation::Standard if self.command.requires_workspace_snapshot() => {
        WorkspaceContext::build_with_snapshot(workspace_root)
      }
      ContextPreparation::Standard => {
        WorkspaceContext::build_with_source_capture(workspace_root, self.command.requires_worktree_source_capture())
      }
      ContextPreparation::HermeticBuild => {
        let bootstrap = crate::hermetic::prepare_bootstrap(workspace_root)?;
        WorkspaceContext::build_with_hermetic_snapshot(workspace_root, bootstrap)
      }
    };
    let context = match context {
      Ok(context) => context,
      Err(error) => {
        if run::try_complete_codegen_backend_probe_failure(&self.command, workspace_root, &error)? {
          return Ok(None);
        }
        return Err(error);
      }
    };
    Ok(Some((*self.command, context, self.pre_context_cache_request)))
  }
}

fn validate_hermetic_run_arguments(arguments: &[String]) -> RailResult<()> {
  let mut blocked = std::collections::BTreeSet::new();
  for argument in arguments {
    let option = argument.split_once('=').map_or(argument.as_str(), |(option, _)| option);
    if argument == "--"
      || matches!(
        option,
        "--all"
          | "--artifact-dir"
          | "--build-dir"
          | "--config"
          | "--exclude"
          | "--future-incompat-report"
          | "--lockfile-path"
          | "--manifest-path"
          | "--out-dir"
          | "--package"
          | "--target-dir"
          | "--timings"
          | "--unit-graph"
          | "--workspace"
          | "-C"
          | "-Z"
          | "-m"
          | "-p"
      )
      || ["-C", "-Z", "-m", "-p"]
        .iter()
        .any(|prefix| argument.starts_with(prefix) && !argument.starts_with("--"))
    {
      blocked.insert(option);
    }
  }
  if blocked.is_empty() {
    return Ok(());
  }
  Err(crate::error::RailError::with_help(
    format!(
      "hermetic Cargo arguments override the modeled action boundary: {}",
      blocked.into_iter().collect::<Vec<_>>().join(", ")
    ),
    "select packages with cargo-rail's --all/change scope; workspace, output, configuration, and raw rustc overrides remain explicitly uncacheable",
  ))
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
  if config_override.is_none() && !json && run::try_complete_active_cargo_profile(&cmd, workspace_root)? {
    return Ok(PreContextDispatch::Handled);
  }
  let pre_context_cache_request = config_override.is_none() && !json && cmd.is_pre_context_cache_request();
  if pre_context_cache_request {
    let (print_cmd, explain) = match &cmd {
      Commands::Run { print_cmd, explain, .. } => (*print_cmd, *explain),
      _ => unreachable!("pre-context cache predicate only accepts run commands"),
    };
    match crate::hermetic::try_restore_pre_context(workspace_root)? {
      crate::hermetic::PreContextCacheAttempt::Hit(hit) => {
        run::complete_pre_context_cache_hit(workspace_root, *hit, print_cmd, explain)?;
        return Ok(PreContextDispatch::Handled);
      }
      crate::hermetic::PreContextCacheAttempt::Miss(reason) => {
        if explain {
          println!("action `build` local cache precheck: miss ({reason})");
        }
      }
    }
  }

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

    Commands::Cache { command } => {
      match command {
        cli::CacheCommand::Status { scope, format } => cache::run_status(workspace_root, scope, format)?,
        cli::CacheCommand::Clean { scope, check, format } => {
          cache::run_clean(workspace_root, scope, check, format)?;
        }
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
        false,
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
        false,
      )?))
    }

    other => Ok(PreContextDispatch::NeedsContext(PreparedContext::new(
      other,
      pre_context_cache_request,
    )?)),
  }
}

/// Dispatch a command to its handler
///
/// This is the main command routing logic. It takes a parsed `Commands` enum
/// and the workspace context, then calls the appropriate handler.
pub fn dispatch(cmd: Commands, ctx: &WorkspaceContext, pre_context_cache_request: bool) -> RailResult<()> {
  match cmd {
    Commands::Run {
      since,
      merge_base,
      all,
      actions,
      profile,
      workflow,
      dry_run,
      hermetic,
      no_cache,
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
        hermetic,
        no_cache,
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
        hermeticity_doctor: false,
        pre_context_cache_request,
      },
    ),

    Commands::Doctor {
      command:
        cli::DoctorCommand::Hermeticity {
          actions,
          profile,
          workflow,
          generated,
          ignore_bin_crates,
          format,
        },
    } => run_run(
      ctx,
      run::RunOptions {
        all: true,
        actions,
        profile,
        workflow,
        dry_run: true,
        format: match format {
          TextJsonOutputFormat::Text => common::ActionOutputFormat::Text,
          TextJsonOutputFormat::Json => common::ActionOutputFormat::Json,
        },
        generated,
        explain: true,
        ignore_bin_crates,
        hermeticity_doctor: true,
        hermetic: false,
        ..run::RunOptions::default()
      },
    ),
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
