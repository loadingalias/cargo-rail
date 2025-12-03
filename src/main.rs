//! Binary entry point for cargo-rail
//!
//! This is intentionally thin - all logic lives in the library.
//! See rules.md: "Thin main.rs" principle.

use cargo_rail::commands::cli::UnifyCommand;
use cargo_rail::commands::{self, CargoCli, Commands};
use cargo_rail::error::{RailError, print_error};
use cargo_rail::workspace;

use clap::Parser;

fn main() {
  let CargoCli::Rail(mut cli) = CargoCli::parse();

  // Apply global --json flag to command format fields
  if cli.json {
    cli.command.apply_json_override();
  }

  // Initialize output control (quiet mode)
  cargo_rail::output::init(cli.quiet);

  // Detect JSON mode early (before building workspace context)
  // This ensures progress messages during metadata loading are suppressed
  if cli.json || cli.command.is_json_format() {
    cargo_rail::output::set_json_mode(true);
  }

  // Get workspace root
  let workspace_root = match std::env::current_dir() {
    Ok(dir) => dir,
    Err(e) => {
      eprintln!("Error: Failed to get current directory: {}", e);
      std::process::exit(1);
    }
  };

  // Handle init command specially - it doesn't require a valid workspace
  if let Commands::Init { output, force, check } = cli.command {
    if let Err(e) = commands::run_init_standalone(&workspace_root, &output, force, check) {
      exit_with_error(e);
    }
    return;
  }

  // Handle unify undo specially - it needs to work even with corrupted Cargo.toml
  if let Commands::Unify {
    command: Some(UnifyCommand::Undo { list, ref backup_id }),
    ..
  } = cli.command
  {
    if let Err(e) = commands::run_unify_undo(&workspace_root, list, backup_id.clone()) {
      exit_with_error(e);
    }
    return;
  }

  // Build workspace context (single-load pattern)
  let ctx = match workspace::WorkspaceContext::build(&workspace_root) {
    Ok(ctx) => ctx,
    Err(e) => exit_with_error(e),
  };

  // Dispatch to command handler
  if let Err(e) = commands::dispatch(cli.command, &ctx) {
    exit_with_error(e);
  }
}

fn exit_with_error(err: RailError) -> ! {
  print_error(&err);
  std::process::exit(err.exit_code().as_i32());
}
