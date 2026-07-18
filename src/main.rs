//! Entry point for cargo-rail
//!
//! This is intentionally thin - all logic lives in the library.
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]

use cargo_rail::commands::{self, CargoCli, PreContextDispatch, RailCli};
use cargo_rail::error::{RailError, RailResult, print_error};
use cargo_rail::instrumentation::DiagnosticSession;
use cargo_rail::workspace;

use clap::Parser;

fn main() {
  if let Some(exit_code) = cargo_rail::compiler::wrapper::run_if_requested() {
    std::process::exit(exit_code);
  }

  let CargoCli::Rail(mut cli) = CargoCli::parse();

  // Apply global --json flag to command format fields
  if cli.json
    && let Err(error) = cli.command.apply_json_override()
  {
    error.exit();
  }

  let diagnostics = match DiagnosticSession::start(cli.diagnostics_file.take()) {
    Ok(diagnostics) => diagnostics,
    Err(error) => exit_with_error(error),
  };
  let result = run(cli);
  let diagnostics_result = diagnostics.finish();

  if let Err(error) = result {
    exit_with_error(error);
  }
  if let Err(error) = diagnostics_result {
    exit_with_error(error);
  }
}

fn run(cli: RailCli) -> RailResult<()> {
  // Initialize output control (quiet mode)
  cargo_rail::output::init(cli.quiet);

  // Detect JSON mode early (before building workspace context)
  // This ensures progress messages during metadata loading are suppressed
  if cli.json || cli.command.is_json_format() {
    cargo_rail::output::set_json_mode(true);
  }

  // Get workspace root (from --workspace-root flag or current directory)
  let workspace_root = if let Some(ref root) = cli.workspace_root {
    if root.is_absolute() {
      root.clone()
    } else {
      match std::env::current_dir() {
        Ok(cwd) => cwd.join(root),
        Err(error) => {
          cargo_rail::error!("failed to get current directory: {}", error);
          return Err(RailError::ExitWithCode { code: 1 });
        }
      }
    }
  } else {
    match std::env::current_dir() {
      Ok(dir) => dir,
      Err(error) => {
        cargo_rail::error!("failed to get current directory: {}", error);
        return Err(RailError::ExitWithCode { code: 1 });
      }
    }
  };

  // Store config override path for commands that need it
  let config_override = cli.config.as_deref();

  let command = match commands::try_dispatch_pre_context(cli.command, &workspace_root, config_override, cli.json) {
    Ok(PreContextDispatch::Handled) => return Ok(()),
    Ok(PreContextDispatch::NeedsContext(cmd)) => cmd,
    Err(error) => return Err(error),
  };

  // Build workspace context (single-load pattern)
  let ctx = if command.requires_workspace_snapshot() {
    workspace::WorkspaceContext::build_with_snapshot(&workspace_root)?
  } else {
    workspace::WorkspaceContext::build_with_source_capture(&workspace_root, command.requires_worktree_source_capture())?
  };
  if let Some(snapshot_id) = ctx.snapshot_id() {
    cargo_rail::instrumentation::record_snapshot_id(snapshot_id.to_string());
  }

  // Dispatch to command handler
  commands::dispatch(command, &ctx)
}

fn exit_with_error(err: RailError) -> ! {
  print_error(&err);
  std::process::exit(err.exit_code().as_i32());
}
