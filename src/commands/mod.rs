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

pub use affected::{AffectedOptions, run_affected};
pub use clean::run_clean;
pub use cli::{CargoCli, Commands, RailCli, ReleaseCommand, SplitCommand};
pub use common::OutputFormat;
pub use config::run_config_validate;
pub use init::{run_init, run_init_standalone};
pub use release::{run_release_check, run_release_init, run_release_plan, run_release_publish};
pub use split::{run_split, run_split_init};
pub use sync::run_sync;
pub use test::run_test;
pub use unify::{run_unify_analyze, run_unify_apply, run_unify_undo};

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
      explain,
    } => run_affected(
      ctx,
      AffectedOptions {
        since,
        from,
        to,
        format,
        all,
        output,
        explain,
      },
    ),

    Commands::Test {
      since,
      all,
      skip_nextest,
      explain,
      format,
      test_args,
    } => {
      let config = test::TestConfig {
        since,
        all,
        explain,
        format,
        prefer_nextest: !skip_nextest,
        test_args,
      };
      run_test(ctx, config)
    }

    // Init is handled before WorkspaceContext is built
    Commands::Init { .. } => unreachable!("Init command should be handled before dispatch"),

    // Dependency Unification
    Commands::Unify {
      command,
      check,
      format,
      backup,
      skip_report,
      report_path,
      show_diff,
    } => {
      // Undo subcommand is handled before WorkspaceContext is built
      if command.is_some() {
        unreachable!("Undo subcommand should be handled before dispatch")
      } else if check {
        run_unify_analyze(ctx, show_diff, format)
      } else {
        run_unify_apply(ctx, backup, skip_report, report_path)
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
        format,
      } => run_split(ctx, crate_name, all, remote, check, format),
    },

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
        skip_publish,
        skip_tag,
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
          run_release_publish(ctx, names, all, bump, skip_publish, skip_tag)
        }
      }
    },

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
    Commands::Config { command } => match command {
      cli::ConfigCommand::Validate { format } => run_config_validate(ctx, format),
    },
  }
}
