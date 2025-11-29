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
//! - **affected**: Find crates affected by changes (used by split/sync)
//!
//! All commands accept `&WorkspaceContext` to avoid redundant workspace loads.

/// Find crates affected by changes
pub mod affected;
/// Clean up workspace artifacts
pub mod clean;
/// CLI argument definitions (clap structs)
pub mod cli;
/// Common utilities for command implementations
pub mod common;
/// Configuration management commands
pub mod config;
/// Initialize cargo-rail configuration
pub mod init;
/// Release planning and publishing
pub mod release;
/// Split crates into standalone repositories
pub mod split;
/// Bidirectional sync between monorepo and split repos
pub mod sync;
/// Smart test runner for affected crates
pub mod test;
/// Workspace dependency unification commands
pub mod unify;

pub use affected::run_affected;
pub use clean::run_clean;
pub use cli::{CargoCli, Commands, RailCli};
pub use common::OutputFormat;
pub use config::run_config_validate;
pub use init::{run_init, run_init_standalone};
pub use release::{run_release_check, run_release_init, run_release_plan, run_release_publish};
pub use split::{run_split, run_split_init};
pub use sync::run_sync;
pub use test::run_test;
pub use unify::{run_unify_analyze, run_unify_apply, run_unify_undo};

use crate::error::RailError;
use crate::error::RailResult;
use crate::workspace::WorkspaceContext;

/// Dispatch a command to its handler
///
/// This is the main command routing logic. It takes a parsed `Commands` enum
/// and the workspace context, then calls the appropriate handler.
pub fn dispatch(cmd: Commands, ctx: &WorkspaceContext) -> RailResult<()> {
  match cmd {
    // Graph Commands
    Commands::Affected {
      since,
      from,
      to,
      format,
      all,
      output,
    } => run_affected(ctx, since, from, to, format, all, output),

    Commands::Test {
      since,
      all,
      skip_nextest,
      explain,
      test_args,
    } => {
      let config = test::TestConfig {
        since,
        all,
        explain,
        prefer_nextest: !skip_nextest,
        test_args,
      };
      run_test(ctx, config)
    }

    // Init is handled before WorkspaceContext is built
    Commands::Init { .. } => unreachable!("Init command should be handled before dispatch"),

    // Dependency Unification
    Commands::Unify {
      action,
      check,
      format,
      exclude,
      include,
      backup,
      pin_transitives,
      include_renamed,
      list: _,
      backup_id: _,
      skip_report,
      report_path,
      show_diff,
    } => {
      // Undo should have been handled before WorkspaceContext was built
      if let Some(act) = action {
        if act == "undo" {
          unreachable!("Undo command should be handled before dispatch")
        } else {
          Err(RailError::message(format!(
            "Unknown unify action '{}'. Valid actions: undo",
            act
          )))
        }
      } else if check {
        run_unify_analyze(
          ctx,
          exclude,
          include,
          pin_transitives,
          include_renamed,
          show_diff,
          format,
        )
      } else {
        run_unify_apply(ctx, exclude, include, backup, include_renamed, skip_report, report_path)
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
      if action.as_deref() == Some("init") {
        let crates = if crate_names.is_empty() {
          None
        } else {
          Some(crate_names)
        };
        run_split_init(ctx, crates, check)
      } else {
        let crate_name = action;
        run_split(ctx, crate_name, all, remote, check, format)
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
    } => run_sync(
      ctx,
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
      if action.as_deref() == Some("init") {
        let crates = if crate_names.is_empty() {
          None
        } else {
          Some(crate_names)
        };
        run_release_init(ctx, crates, check)
      } else {
        let mut all_crate_names = crate_names;
        if let Some(first_crate) = action {
          all_crate_names.insert(0, first_crate);
        }

        let names = if all || all_crate_names.is_empty() {
          None
        } else {
          Some(all_crate_names)
        };

        if check {
          run_release_plan(ctx, names, bump, skip_publish, skip_tag, format)
        } else {
          run_release_publish(ctx, names, all, bump, skip_publish, skip_tag)
        }
      }
    }

    Commands::Check {
      crate_names,
      all,
      format,
    } => {
      let names = if all || crate_names.is_empty() {
        None
      } else {
        Some(crate_names)
      };
      run_release_check(ctx, names, all, format)
    }

    // Clean
    Commands::Clean {
      cache,
      backups,
      reports,
      check,
    } => run_clean(ctx, cache, backups, reports, check),

    // Config
    Commands::Config { action, format } => {
      if action == "validate" {
        run_config_validate(ctx, format)
      } else {
        Err(RailError::with_help(
          format!("unknown config action: {}", action),
          "valid actions: validate",
        ))
      }
    }
  }
}
