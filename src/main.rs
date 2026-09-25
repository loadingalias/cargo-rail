//! Entry point for cargo-rail
//!
//! This is intentionally thin - all logic lives in the library.
use cargo_rail::commands::{self, PreContextDispatch, RailCli};
use cargo_rail::error::{RailError, RailResult, print_error};
use cargo_rail::instrumentation::DiagnosticSession;

use clap::Parser;
use std::time::Instant;

fn main() {
    if let Some(exit_code) = cargo_rail::release::dispatch_credential_provider() {
        std::process::exit(exit_code);
    }
    match cargo_rail::compiler::invocation::dispatch() {
        cargo_rail::compiler::invocation::PreClapDispatch::Cli => {}
        cargo_rail::compiler::invocation::PreClapDispatch::Exit(exit_code) => std::process::exit(exit_code),
    }

    let cli_preparation_started = Instant::now();
    let mut argv = std::env::args_os().collect::<Vec<_>>();
    if argv.get(1).is_some_and(|argument| argument == "rail") {
        argv.remove(1);
    }
    let mut cli = match RailCli::try_parse_from(&argv) {
        Ok(cli) => cli,
        Err(error) => {
            if error.kind() == clap::error::ErrorKind::InvalidSubcommand
                && let Some(removed) = commands::removed::removed_command(&argv[1..])
            {
                let json = argv.iter().any(|argument| argument == "--json");
                cargo_rail::output::init(cargo_rail::output::InvocationOutput::capture_protocol_with_progress(
                    false,
                    false,
                    if json {
                        cargo_rail::output::OutputProtocol::Json
                    } else {
                        cargo_rail::output::OutputProtocol::Text
                    },
                    false,
                ));
                exit_with_error(removed);
            }
            error.exit();
        }
    };

    let protocol = if cli.json {
        cargo_rail::output::OutputProtocol::Json
    } else {
        cli.command.output_protocol()
    };
    let redirected_progress =
        protocol == cargo_rail::output::OutputProtocol::Json && cli.command.retains_machine_progress();
    cargo_rail::output::init(cargo_rail::output::InvocationOutput::capture_protocol_with_progress(
        cli.quiet,
        cli.verbose,
        protocol,
        redirected_progress,
    ));

    if cli.json
        && let Err(error) = cli.command.apply_json_override()
    {
        error.exit();
    }

    let diagnostics = match DiagnosticSession::start(cli.diagnostics_file.take()) {
        Ok(diagnostics) => diagnostics,
        Err(error) => exit_with_error(error),
    };
    let result = {
        let _heartbeat = cargo_rail::output::Heartbeat::start();
        let result = run(cli, cli_preparation_started);
        cargo_rail::output::end_phase();
        result
    };
    let diagnostics_result = diagnostics.finish();

    if let Err(error) = result {
        exit_with_error(error);
    }
    if let Err(error) = diagnostics_result {
        exit_with_error(error);
    }
}

fn run(cli: RailCli, cli_preparation_started: Instant) -> RailResult<()> {
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

    let config_override = cli.config.as_deref();

    let prepared = commands::try_dispatch_pre_context(cli.command, &workspace_root, config_override, cli.json);
    cargo_rail::instrumentation::record_cli_pre_context_preparation(cli_preparation_started);
    let prepared = match prepared {
        Ok(PreContextDispatch::Handled) => return Ok(()),
        Ok(PreContextDispatch::NeedsContext(prepared)) => prepared,
        Err(error) => return Err(error),
    };

    let workspace_capture_started = Instant::now();
    let context = prepared.build(&workspace_root);
    cargo_rail::instrumentation::record_workspace_capture_cargo_metadata(workspace_capture_started);
    let (command, ctx, prepared_plan) = context?;
    if let Some(snapshot_id) = ctx.captured_authority_id() {
        cargo_rail::instrumentation::record_snapshot_id(snapshot_id);
    }

    commands::dispatch(command, &ctx, prepared_plan)
}

fn exit_with_error(err: RailError) -> ! {
    if err.is_broken_pipe() {
        std::process::exit(0);
    }
    print_error(&err);
    std::process::exit(err.exit_code().as_i32());
}
