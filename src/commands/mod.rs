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
pub use clean::{CleanContext, run_clean};
#[doc(hidden)]
pub use cli::{
    CacheCommand, CacheScope, ChangeCommand, Commands, DoctorCommand, RailCli, ReleaseCommand, SplitCommand,
    generate_completions,
};
pub use common::{ChangeOutputFormat, SplitOutputFormat, SurfaceOutputFormat, TextJsonOutputFormat};
pub use config::{
    StrictnessMode, run_config_explain, run_config_locate, run_config_migrate, run_config_print,
    run_config_validate_standalone,
};
pub use doctor::run_native_cache_doctor;
pub use init::{run_init, run_init_standalone};
pub use plan::{PlanOptions, run_plan};
pub use release::{
    ReleaseFinalizeOptions, run_release_finalize, run_release_init, run_release_plan, run_release_publication_check,
    run_release_publish, run_release_status_standalone,
};
pub use split::{run_split, run_split_init};
pub use surface::{SurfaceOptions, run_surface};
pub use sync::run_sync;
pub use unify::{UnifyAnalyzeOptions, run_unify_analyze, run_unify_apply, run_unify_doctor, run_unify_undo};

use crate::error::RailResult;
use crate::workspace::WorkspaceContext;
use std::path::{Path, PathBuf};

/// Result of attempting to dispatch a command without building WorkspaceContext.
#[derive(Debug)]
#[doc(hidden)]
pub enum PreContextDispatch {
    /// The command ran and the process should exit.
    Handled,
    /// The command requires a WorkspaceContext to run.
    NeedsContext(PreparedContext),
}

/// A command paired with the context-construction contract it requires.
#[derive(Debug)]
#[doc(hidden)]
pub struct PreparedContext {
    command: Box<Commands>,
    config_override: Option<PathBuf>,
    plan_options: Option<PlanOptions>,
}

impl PreparedContext {
    fn new(command: Commands, config_override: Option<&Path>) -> RailResult<Self> {
        Ok(Self {
            command: Box::new(command),
            config_override: config_override.map(Path::to_path_buf),
            plan_options: None,
        })
    }

    fn new_plan(command: Commands, config_override: Option<&Path>, plan_options: PlanOptions) -> Self {
        Self {
            command: Box::new(command),
            config_override: config_override.map(Path::to_path_buf),
            plan_options: Some(plan_options),
        }
    }

    /// Build the exact workspace context required by this command.
    #[doc(hidden)]
    pub fn build(mut self, workspace_root: &Path) -> RailResult<(Commands, WorkspaceContext, Option<PlanOptions>)> {
        let historical = self
            .plan_options
            .as_ref()
            .map(|options| options.comparison.resolve_objects_before_context(workspace_root))
            .transpose()?
            .flatten();
        let context = if let Some((from, to)) = historical {
            let context = WorkspaceContext::build_historical_planning_with_config(
                workspace_root,
                &from,
                &to,
                self.config_override.as_deref(),
            );
            if let Some(options) = self.plan_options.as_mut() {
                options.comparison.replace_objects(from, to);
            }
            context
        } else if self.command.requires_workspace_snapshot() {
            WorkspaceContext::build_with_snapshot_and_config(workspace_root, self.config_override.as_deref())
        } else if self.command.requires_planning_source_capture() {
            WorkspaceContext::build_with_planning_capture_and_config(workspace_root, self.config_override.as_deref())
        } else {
            WorkspaceContext::build_with_source_capture_and_config(
                workspace_root,
                self.command.requires_worktree_source_capture(),
                self.config_override.as_deref(),
            )
        };
        Ok((*self.command, context?, self.plan_options))
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
        Commands::Plan {
            verify: Some(plan_file),
            ..
        } => {
            plan::verify_saved_plan(workspace_root, config_override, &plan_file)?;
            Ok(PreContextDispatch::Handled)
        }

        Commands::Plan { schema: true, .. } => {
            plan::print_plan_schema();
            Ok(PreContextDispatch::Handled)
        }

        Commands::Plan {
            since,
            from,
            to,
            merge_base,
            json,
            explain,
            explain_work,
            all,
            evidence,
            verify: None,
            schema: false,
        } => {
            let comparison = plan::PlanComparison::from_cli(&since, &from, &to, merge_base)?;
            let command = Commands::Plan {
                since,
                from,
                to,
                merge_base,
                json,
                explain,
                explain_work: explain_work.clone(),
                all,
                evidence: evidence.clone(),
                verify: None,
                schema: false,
            };
            Ok(PreContextDispatch::NeedsContext(PreparedContext::new_plan(
                command,
                config_override,
                PlanOptions {
                    comparison,
                    json,
                    explain,
                    explain_work,
                    all,
                    evidence,
                },
            )))
        }

        Commands::Surface { schema: true, .. } => {
            surface::print_surface_schema();
            Ok(PreContextDispatch::Handled)
        }

        command @ Commands::Surface { .. } => {
            crate::compiler::driver::CompilerFactDriverAuthority::require_surface_installation()?;
            Ok(PreContextDispatch::NeedsContext(PreparedContext::new(
                command,
                config_override,
            )?))
        }

        Commands::Init {
            output,
            force,
            dry_run,
            targets,
            detect_targets,
        } => {
            init::run_init_standalone(workspace_root, &output, force, dry_run, json, &targets, detect_targets)?;
            Ok(PreContextDispatch::Handled)
        }

        Commands::Unify {
            command:
                Some(cli::UnifyCommand::Undo {
                    list,
                    backup_id,
                    format,
                }),
            ..
        } => {
            unify::run_unify_undo(workspace_root, list, backup_id, format)?;
            Ok(PreContextDispatch::Handled)
        }

        Commands::Config {
            command: cli::ConfigCommand::Migrate { check, format },
        } => {
            config::run_config_migrate(workspace_root, config_override, check, format)?;
            Ok(PreContextDispatch::Handled)
        }

        Commands::Config {
            command:
                cli::ConfigCommand::Validate {
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
            command: cli::ConfigCommand::Explain { fields, all, format },
        } => {
            config::run_config_explain(workspace_root, config_override, &fields, all, format)?;
            Ok(PreContextDispatch::Handled)
        }

        Commands::Completions { shell } => {
            cli::generate_completions(shell);
            Ok(PreContextDispatch::Handled)
        }

        Commands::Clean {
            all,
            cache,
            prune_backups,
            all_backups,
            reports,
            release_journal,
            check,
            format,
        } => {
            let context = clean::CleanContext::capture(workspace_root, config_override)?;
            run_clean(
                &context,
                clean::CleanOptions {
                    all,
                    cache,
                    prune_backups,
                    all_backups,
                    reports,
                    release_journal,
                    check,
                    format,
                },
            )?;
            Ok(PreContextDispatch::Handled)
        }

        Commands::Cache { command } => {
            match command {
                cli::CacheCommand::Setup(setup) => {
                    let cli::CacheSetupArgs {
                        local_dir,
                        max_size,
                        remote,
                        remote_mode,
                        remote_environment,
                        root_portability,
                        local_only,
                        distributed_local,
                        distributed_endpoint,
                        distributed_server_name,
                        distributed_capability,
                        distributed_authority,
                        distributed_client_certificate,
                        distributed_client_private_key,
                        distributed_policy,
                        check,
                        format,
                    } = *setup;
                    cache::run_setup(
                        workspace_root,
                        crate::cache::installation::SetupRequest {
                            local_dir,
                            max_bytes: max_size,
                            remote_url: remote,
                            remote_mode,
                            remote_environment,
                            root_portability,
                            local_only,
                            distributed_local,
                            distributed_endpoint,
                            distributed_server_name,
                            distributed_capability,
                            distributed_authority,
                            distributed_client_certificate,
                            distributed_client_private_key,
                            distributed_policy,
                        },
                        check,
                        format,
                    )?;
                }
                cli::CacheCommand::Normalize {
                    url,
                    mode,
                    environment,
                    format,
                } => cache::run_normalize(&url, mode.as_deref(), environment, format)?,
                cli::CacheCommand::Probe { format } => cache::run_probe(workspace_root, format)?,
                cli::CacheCommand::Status { scope, format } => cache::run_status(workspace_root, scope, format)?,
                cli::CacheCommand::Recover { check, format } => {
                    cache::run_recover(workspace_root, check, format)?;
                }
                cli::CacheCommand::Clean { scope, check, format } => {
                    cache::run_clean(workspace_root, scope, check, format)?;
                }
                cli::CacheCommand::Remove { check, format } => cache::run_remove(workspace_root, check, format)?,
            }
            Ok(PreContextDispatch::Handled)
        }

        Commands::Release {
            command: cli::ReleaseCommand::Status { state, history, format },
        } => {
            release::run_release_status_standalone(workspace_root, state.as_deref(), history, format)?;
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
pub fn dispatch(cmd: Commands, ctx: &WorkspaceContext, prepared_plan: Option<PlanOptions>) -> RailResult<()> {
    match cmd {
        Commands::Doctor {
            command: cli::DoctorCommand::NativeCache { format },
        } => run_native_cache_doctor(ctx, format),

        Commands::Plan { .. } => run_plan(
            ctx,
            prepared_plan.ok_or_else(|| crate::error::RailError::message("plan comparison was not prepared"))?,
        ),

        Commands::Surface {
            prepare,
            check,
            fix,
            resume,
            dry_run,
            backup,
            format,
            output,
            explain,
            only,
            schema: _,
        } => run_surface(
            ctx,
            SurfaceOptions {
                prepare,
                check,
                fix,
                resume,
                dry_run,
                backup,
                format,
                output,
                explain,
                only,
            },
        ),

        // Init is handled before WorkspaceContext is built
        Commands::Init { .. } => Err(crate::error::RailError::message(
            "init command reached workspace dispatch",
        )),

        // Dependency Unification
        Commands::Unify {
            command,
            check,
            format,
            report,
            report_path,
            output,
            show_diff,
            explain,
        } => match command {
            Some(cli::UnifyCommand::Doctor { format }) => run_unify_doctor(ctx, format),
            Some(cli::UnifyCommand::Apply {
                plan,
                backup,
                report,
                report_path,
                format,
            }) => run_unify_apply(ctx, backup, !report, report_path, plan, format),
            Some(cli::UnifyCommand::Undo { .. }) => Err(crate::error::RailError::message(
                "unify undo reached workspace dispatch",
            )),
            None => {
                if !check {
                    crate::warn!(
                        "bare 'cargo rail unify' is preview-only; use 'cargo rail unify apply' to modify manifests"
                    );
                }
                run_unify_analyze(
                    ctx,
                    UnifyAnalyzeOptions {
                        check,
                        show_diff,
                        explain,
                        format,
                        output: output.as_ref(),
                        backup: false,
                        no_report: !report,
                        report_path: report_path.as_ref(),
                    },
                )
            }
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
                publish,
                skip_publish,
                skip_tag,
                pr,
                include_dependents,
                yes,
                allow_non_default_branch,
                format,
            } => {
                let names = if all || crate_names.is_empty() {
                    None
                } else {
                    Some(crate_names)
                };

                let publish = publish && !skip_publish;
                if check {
                    crate::warn!("'release run --check' is deprecated; use 'cargo rail release check'");
                    run_release_plan(ctx, names, bump, publish, skip_tag, include_dependents, format)
                } else {
                    run_release_publish(
                        ctx,
                        release::ReleasePublishArgs {
                            crate_names: names,
                            all,
                            bump,
                            publish,
                            skip_tag,
                            pr,
                            include_dependents,
                            yes,
                            allow_non_default_branch,
                            plan_path: plan,
                            format,
                        },
                    )
                }
            }
            cli::ReleaseCommand::Check {
                crate_names,
                all,
                bump,
                publication,
                extended,
                skip_tag,
                include_dependents,
                format,
            } => {
                let names = if all || crate_names.is_empty() {
                    None
                } else {
                    Some(crate_names)
                };
                if publication {
                    run_release_publication_check(ctx, names, all, extended, include_dependents, format)
                } else {
                    run_release_plan(ctx, names, bump, false, skip_tag, include_dependents, format)
                }
            }
            cli::ReleaseCommand::Finalize {
                crate_names,
                all,
                publish,
                skip_publish,
                skip_tag,
                include_dependents,
                yes,
                allow_non_default_branch,
                format,
            } => {
                let names = if all || crate_names.is_empty() {
                    None
                } else {
                    Some(crate_names)
                };
                release::run_release_finalize(
                    ctx,
                    release::ReleaseFinalizeOptions {
                        crate_names: names,
                        all,
                        publish: publish && !skip_publish,
                        skip_tag,
                        include_dependents,
                        yes,
                        allow_non_default_branch,
                        format,
                    },
                )
            }
            cli::ReleaseCommand::Resume { state } => release::run_release_resume(ctx, &state),
            cli::ReleaseCommand::Status { .. } => Err(crate::error::RailError::message(
                "release status reached workspace dispatch",
            )),
            cli::ReleaseCommand::Abort { state, yes } => release::run_release_abort(ctx, &state, yes),
        },

        // Clean
        Commands::Clean { .. } => Err(crate::error::RailError::message(
            "clean command reached workspace dispatch",
        )),

        Commands::Cache { .. } => Err(crate::error::RailError::message(
            "cache command reached workspace dispatch",
        )),

        // Config commands are handled before WorkspaceContext is built
        Commands::Config { command } => match command {
            cli::ConfigCommand::Locate { .. } => Err(crate::error::RailError::message(
                "config locate reached workspace dispatch",
            )),
            cli::ConfigCommand::Print { .. } => Err(crate::error::RailError::message(
                "config print reached workspace dispatch",
            )),
            cli::ConfigCommand::Explain { .. } => Err(crate::error::RailError::message(
                "config explain reached workspace dispatch",
            )),
            cli::ConfigCommand::Validate { .. } => Err(crate::error::RailError::message(
                "config validate reached workspace dispatch",
            )),
            cli::ConfigCommand::Migrate { .. } => Err(crate::error::RailError::message(
                "config migrate reached workspace dispatch",
            )),
        },

        // Completions is handled before WorkspaceContext is built
        Commands::Completions { .. } => Err(crate::error::RailError::message(
            "completions command reached workspace dispatch",
        )),
    }
}
