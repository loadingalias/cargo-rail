//! Entry point for cargo-rail
//!
//! This is intentionally thin - all logic lives in the library.
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]

use cargo_rail::commands::{self, CargoCli, PreContextDispatch};
use cargo_rail::error::{RailError, print_error};
use cargo_rail::workspace;

use clap::Parser;

fn main() {
  let CargoCli::Rail(mut cli) = CargoCli::parse();

  // Apply global --json flag to command format fields
  if cli.json
    && let Err(error) = cli.command.apply_json_override()
  {
    error.exit();
  }

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
        Err(e) => {
          cargo_rail::error!("failed to get current directory: {}", e);
          std::process::exit(1);
        }
      }
    }
  } else {
    match std::env::current_dir() {
      Ok(dir) => dir,
      Err(e) => {
        cargo_rail::error!("failed to get current directory: {}", e);
        std::process::exit(1);
      }
    }
  };

  // Store config override path for commands that need it
  let config_override = cli.config.as_deref();

  let command = match commands::try_dispatch_pre_context(cli.command, &workspace_root, config_override, cli.json) {
    Ok(PreContextDispatch::Handled) => return,
    Ok(PreContextDispatch::NeedsContext(cmd)) => cmd,
    Err(e) => exit_with_error(e),
  };

  // Build workspace context (single-load pattern)
  let ctx = match workspace::WorkspaceContext::build(&workspace_root) {
    Ok(ctx) => ctx,
    Err(e) => exit_with_error(e),
  };

  // Dispatch to command handler
  if let Err(e) = commands::dispatch(command, &ctx) {
    exit_with_error(e);
  }
}

fn exit_with_error(err: RailError) -> ! {
  print_error(&err);
  std::process::exit(err.exit_code().as_i32());
}
