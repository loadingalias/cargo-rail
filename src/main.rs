//! Entry point for cargo-rail
//!
//! This is intentionally thin - all logic lives in the library.

use cargo_rail::commands::cli::{ConfigCommand, UnifyCommand};
use cargo_rail::commands::{self, CargoCli, Commands, StrictnessMode};
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

  // Handle init command specially - it doesn't require a valid workspace
  if let Commands::Init { output, force, check } = cli.command {
    if let Err(e) = commands::run_init_standalone(&workspace_root, &output, force, check, cli.json) {
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

  // Handle config sync specially - it only needs workspace root, not full metadata
  if let Commands::Config {
    command: ConfigCommand::Sync { check, format },
  } = cli.command
  {
    if let Err(e) = commands::run_config_sync(&workspace_root, check, format) {
      exit_with_error(e);
    }
    return;
  }

  // Handle config validate specially - it can diagnose parse errors before WorkspaceContext
  if let Commands::Config {
    command: ConfigCommand::Validate {
      format,
      strict,
      no_strict,
    },
  } = cli.command
  {
    let strictness = if strict {
      StrictnessMode::Strict
    } else if no_strict {
      StrictnessMode::NoStrict
    } else {
      StrictnessMode::Auto
    };
    if let Err(e) = commands::run_config_validate_standalone(&workspace_root, format, strictness) {
      exit_with_error(e);
    }
    return;
  }

  // Handle config locate - only needs workspace root and optional config override
  if let Commands::Config {
    command: ConfigCommand::Locate { format },
  } = cli.command
  {
    if let Err(e) = commands::run_config_locate(&workspace_root, config_override, format) {
      exit_with_error(e);
    }
    return;
  }

  // Handle config print - only needs workspace root and optional config override
  if let Commands::Config {
    command: ConfigCommand::Print { format },
  } = cli.command
  {
    if let Err(e) = commands::run_config_print(&workspace_root, config_override, format) {
      exit_with_error(e);
    }
    return;
  }

  // Handle completions - no workspace needed at all
  if let Commands::Completions { shell } = cli.command {
    commands::generate_completions(shell);
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
