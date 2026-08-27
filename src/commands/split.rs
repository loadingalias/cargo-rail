//! `cargo rail split` - Extract crates to standalone repositories with full git history.

use std::io::IsTerminal;

use crate::commands::common::{SplitOutputFormat, SplitSyncConfigBuilder, enforce_safety_gate, split_mapping_count};
use crate::config::RailConfig;
use crate::error::{GitError, RailError, RailResult};
use crate::git::SystemGit;
use crate::mutation::{self, MutationAction, MutationRisk, MutationTrace};
use crate::progress;
use crate::split::SplitEngine;
use crate::utils;
use crate::workspace::WorkspaceContext;
use rayon::prelude::*;
use serde::Serialize;

#[derive(Serialize)]
struct SplitAppliedCrate {
    crate_name: String,
    target_repo: std::path::PathBuf,
    mode: &'static str,
    pending_commits: usize,
    status: &'static str,
}

/// Arguments for the split run command
#[derive(Debug)]
pub struct SplitRunArgs {
    /// Crate name to split (mutually exclusive with `all`)
    pub crate_name: Option<String>,
    /// Split all configured crates
    pub all: bool,
    /// Override remote repository URL
    pub remote: Option<String>,
    /// Check for pending changes without executing
    pub check: bool,
    /// Apply from a previously generated mutation plan file
    pub plan_path: Option<std::path::PathBuf>,
    /// Allow running on dirty worktree (uncommitted changes)
    pub allow_dirty: bool,
    /// Skip confirmation prompts (for CI/automation)
    pub yes: bool,
    /// Output format
    pub format: SplitOutputFormat,
}

/// Run the split command
pub fn run_split(ctx: &WorkspaceContext, args: SplitRunArgs) -> RailResult<()> {
    ctx.snapshot()?;
    let machine = args.format != SplitOutputFormat::Text;

    // JSON mode enables structured error output and suppresses progress

    // Dirty worktree check (unless --allow-dirty or --check mode)
    if !args.check && !args.allow_dirty {
        let files = ctx
            .changed_source_paths()?
            .into_iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>();
        if !files.is_empty() {
            return Err(RailError::Git(GitError::DirtyWorktree { files }));
        }
    }

    let builder = SplitSyncConfigBuilder::new(ctx)?
        .with_crate_or_all(args.crate_name.clone(), args.all)?
        .with_remote_override(args.remote)
        .validate()?;

    let config_count = builder.count();
    let configs = builder.build_split_configs()?;
    if configs.is_empty() && args.all {
        return render_empty_all(args.format);
    }
    let snapshots = collect_split_snapshots(ctx, &configs)?;
    let expected_mutation_plan = build_split_mutation_plan(ctx, &configs, args.allow_dirty)?;

    // Check mode: show plan
    if args.check {
        let pending_commits = configs
            .iter()
            .map(|config| SplitEngine::pending_commit_count(ctx, config))
            .collect::<RailResult<Vec<_>>>()?;
        let has_pending = pending_commits.iter().any(|count| *count > 0);
        let result = if has_pending { "pending_changes" } else { "clean" };
        let exit_code = i32::from(has_pending);
        match args.format {
            SplitOutputFormat::Json => {
                let crates: Vec<_> = configs
                    .iter()
                    .zip(&pending_commits)
                    .map(|(config, pending_commits)| {
                        serde_json::json!({
                          "crate_name": config.crate_name,
                          "mode": split_mode_name(&config.mode),
                          "target_repo": config.target_repo_path,
                          "branch": config.branch,
                          "remote_url": config.remote_url,
                          "pending": *pending_commits > 0,
                          "pending_commits": pending_commits,
                        })
                    })
                    .collect();
                let payload = serde_json::json!({
                  "command": "split",
                  "check": true,
                  "crates": crates,
                  "count": configs.len(),
                  "planning": {
                    "source_head": ctx.git()?.git().head_commit().unwrap_or_else(|_| "unknown".to_string()),
                    "targets": snapshots,
                  },
                  "mutation_plan": expected_mutation_plan,
                });
                let output = crate::output::machine_json_envelope("split", "check", result, exit_code, payload);
                println!("{}", serde_json::to_string_pretty(&output)?);
            }
            SplitOutputFormat::NamesOnly => {
                for config in &configs {
                    println!("{}", config.crate_name);
                }
            }
            SplitOutputFormat::JsonLines => {
                for (config, pending_commits) in configs.iter().zip(&pending_commits) {
                    let obj = serde_json::json!({
                      "crate_name": config.crate_name,
                      "mode": split_mode_name(&config.mode),
                      "target_repo": config.target_repo_path.display().to_string(),
                      "branch": config.branch,
                      "remote_url": config.remote_url,
                      "pending": *pending_commits > 0,
                      "pending_commits": pending_commits,
                    });
                    println!("{}", serde_json::to_string(&obj)?);
                }
            }
            SplitOutputFormat::Text => {
                for (config, pending_commits) in configs.iter().zip(&pending_commits) {
                    println!(
                        "{}: local source -> target repository {} ({} pending commit(s), mode {})",
                        config.crate_name,
                        config.target_repo_path.display(),
                        pending_commits,
                        split_mode_name(&config.mode)
                    );
                    if crate::output::is_verbose() {
                        println!("  Members: {}", config.ownership.members.join(", "));
                        if let Some(remote) = &config.remote_url {
                            println!("  Remote: {remote}");
                        }
                        println!("  Branch: {}", config.branch);
                    }
                }
                if has_pending {
                    println!(
                        "Next: cargo rail split run {}",
                        if args.all {
                            "--all"
                        } else {
                            configs[0].crate_name.as_str()
                        }
                    );
                } else {
                    println!("Split targets are current.");
                }
            }
        }
        return if has_pending {
            Err(crate::error::RailError::CheckHasPendingChanges)
        } else {
            Ok(())
        };
    }

    enforce_safety_gate(
        "split apply",
        args.yes,
        args.plan_path.as_deref(),
        std::io::stdin().is_terminal() && !machine,
    )?;

    // Interactive confirmation (unless --yes)
    if !args.yes && std::io::stdin().is_terminal() && !machine {
        println!("splitting {} crate(s):\n", config_count);
        for config in &configs {
            println!("  {} -> {}", config.crate_name, config.target_repo_path.display());
        }

        if !utils::prompt_for_confirmation()? {
            println!("cancelled");
            return Ok(());
        }
    }

    let mutation_plan = if let Some(path) = args.plan_path.as_ref() {
        let from_file = mutation::read_plan_file(path)?;
        if !from_file.operation_id.starts_with("split-") {
            return Err(RailError::with_help(
                format!("plan '{}' is not a split plan", path.display()),
                "generate a split plan using 'cargo rail split run --check -f json'".to_string(),
            ));
        }
        mutation::validate_pre_apply_with_allowed_paths(ctx, &from_file, std::slice::from_ref(path))?;
        mutation::validate_requested_operation(&from_file, &expected_mutation_plan)?;
        from_file
    } else {
        mutation::validate_pre_apply(ctx, &expected_mutation_plan)?;
        expected_mutation_plan
    };

    let plan_receipt = mutation::write_receipt(
        ctx.workspace_root(),
        "split",
        "plan",
        "planned",
        mutation_plan.clone(),
        vec![MutationTrace::new(
            "SPLIT_PLAN_CREATED",
            format!("planned split for {} crate(s)", config_count),
        )],
    )?;
    if crate::output::is_verbose() {
        progress!("plan receipt: {}", plan_receipt.display());
    }

    // Execute splits
    let output_crates = if config_count > 1 && args.all {
        progress!("splitting {} crates...", config_count);
        let results: Vec<RailResult<SplitAppliedCrate>> = configs
            .into_par_iter()
            .map(|config| {
                if crate::output::is_verbose() {
                    progress!("  {}", config.crate_name);
                }
                let engine = SplitEngine::new(ctx)?;
                let pending_commits = engine.split_with_pending_count(&config)?;
                Ok(SplitAppliedCrate {
                    crate_name: config.crate_name,
                    target_repo: config.target_repo_path,
                    mode: split_mode_name(&config.mode),
                    pending_commits,
                    status: if pending_commits == 0 { "current" } else { "applied" },
                })
            })
            .collect();
        results.into_iter().collect::<RailResult<Vec<_>>>()?
    } else {
        let mut output_crates = Vec::with_capacity(configs.len());
        for config in configs {
            if crate::output::is_verbose() {
                progress!("splitting {}...", config.crate_name);
            }
            let engine = SplitEngine::new(ctx)?;
            let pending_commits = engine.split_with_pending_count(&config)?;
            output_crates.push(SplitAppliedCrate {
                crate_name: config.crate_name,
                target_repo: config.target_repo_path,
                mode: split_mode_name(&config.mode),
                pending_commits,
                status: if pending_commits == 0 { "current" } else { "applied" },
            });
        }
        output_crates
    };

    let apply_receipt = mutation::write_receipt(
        ctx.workspace_root(),
        "split",
        "apply",
        "applied",
        mutation_plan,
        vec![
            MutationTrace::new("SPLIT_APPLY_STARTED", "started split apply"),
            MutationTrace::new("SPLIT_APPLY_COMPLETED", "completed split apply"),
        ],
    )?;
    if crate::output::is_verbose() {
        progress!("apply receipt: {}", apply_receipt.display());
    }

    match args.format {
        SplitOutputFormat::Text => {
            for item in &output_crates {
                println!(
                    "{}: local source -> target repository {} ({} pending commit(s), mode {})",
                    item.crate_name,
                    item.target_repo.display(),
                    item.pending_commits,
                    item.mode
                );
            }
            println!(
                "Split complete: {} crate(s), {} pending commit(s) processed.",
                output_crates.len(),
                output_crates.iter().map(|item| item.pending_commits).sum::<usize>()
            );
            println!("Receipt: {}", apply_receipt.display());
        }
        SplitOutputFormat::Json => {
            let payload = serde_json::json!({
              "crates": output_crates,
              "count": output_crates.len(),
              "plan_receipt": plan_receipt,
              "receipt": apply_receipt,
            });
            let output = crate::output::machine_json_envelope("split", "apply", "success", 0, payload);
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        SplitOutputFormat::NamesOnly => {
            for item in &output_crates {
                println!("{}", item.crate_name);
            }
        }
        SplitOutputFormat::JsonLines => {
            for item in &output_crates {
                println!("{}", serde_json::to_string(item)?);
            }
        }
    }
    Ok(())
}

fn split_mode_name(mode: &crate::config::SplitMode) -> &'static str {
    match mode {
        crate::config::SplitMode::Single => "single",
        crate::config::SplitMode::Combined => "combined",
    }
}

fn render_empty_all(format: SplitOutputFormat) -> RailResult<()> {
    match format {
        SplitOutputFormat::Text => println!("No split operations are configured."),
        SplitOutputFormat::Json => {
            let output = crate::output::machine_json_envelope(
                "split",
                "selection",
                "clean",
                0,
                serde_json::json!({ "crates": [], "count": 0 }),
            );
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        SplitOutputFormat::NamesOnly | SplitOutputFormat::JsonLines => {}
    }
    Ok(())
}

/// Initialize split configuration for crates
pub fn run_split_init(ctx: &WorkspaceContext, crates: Option<Vec<String>>, dry_run: bool) -> RailResult<()> {
    ctx.snapshot()?;
    use crate::config::RailConfig;
    use std::fs;

    let requested_crates = crates;

    let splits = detect_workspace_splits(ctx, requested_crates.as_deref())?;

    if splits.is_empty() {
        if let Some(requested) = requested_crates {
            return Err(crate::error::RailError::message(format!(
                "no matching crates: {}",
                requested.join(", ")
            )));
        } else {
            return Err(crate::error::RailError::message("no workspace members found"));
        }
    }

    let workspace_root = ctx.workspace_root();
    let existing_config = RailConfig::load(workspace_root).ok();

    let mut config = existing_config.unwrap_or_else(|| RailConfig {
        targets: vec![],
        unify: crate::config::UnifyConfig::default(),
        release: crate::config::ReleaseConfig::default(),
        surface: crate::config::SurfaceConfig::default(),
        plan: crate::config::PlanConfig::default(),
        crates: Default::default(),
    });

    let existing_names: std::collections::HashSet<_> = config.crates.keys().cloned().collect();
    let new_splits: Vec<_> = splits
        .into_iter()
        .filter(|s| !existing_names.contains(&s.name))
        .collect();

    if new_splits.is_empty() {
        println!("all crates already configured");
        return Ok(());
    }

    println!("adding {} split config(s):", new_splits.len());
    for split in &new_splits {
        println!("  {}", split.name);
    }

    use crate::config::{ChangelogConfig, CrateConfig, CrateReleaseConfig, CrateSplitConfig};

    for split in new_splits {
        let crate_config = CrateConfig {
            split: Some(CrateSplitConfig {
                remote: split.remote,
                branch: split.branch,
                mode: split.mode,
                workspace_mode: split.workspace_mode,
                members: split.members,
                legacy_paths: split.legacy_paths,
                include: split.include,
                exclude: split.exclude,
            }),
            release: Some(CrateReleaseConfig { publish: split.publish }),
            changelog: split.changelog_path.map(|path| ChangelogConfig {
                path: Some(path),
                skip: false,
                ..ChangelogConfig::default()
            }),
        };

        config.crates.insert(split.name, crate_config);
    }

    let config_toml = serialize_splits_config(&config)?;

    if dry_run {
        println!("{}", config_toml);
    } else {
        let config_path =
            RailConfig::find_config_path(workspace_root).unwrap_or_else(|| workspace_root.join(".config/rail.toml"));

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(&config_path, config_toml)?;
        println!("updated: {}", config_path.display());

        println!("\nnext: review and customize the generated config");
        println!("      - edit 'remote' URLs to match your repositories");
        println!("      - for combined splits (multiple crates in one repo):");
        println!("        1. set mode = \"combined\" on one crate");
        println!("        2. add Cargo package names to its 'members' array");
        println!("        3. remove the other crate entries");
    }

    Ok(())
}

/// Detect workspace members and create split configs
///
/// If `requested_crates` is provided, only creates configs for those crates.
/// Otherwise, creates configs for all workspace members.
fn detect_workspace_splits(
    ctx: &WorkspaceContext,
    requested_crates: Option<&[String]>,
) -> RailResult<Vec<crate::config::SplitConfig>> {
    use crate::config::{SplitConfig, SplitMode, WorkspaceMode};

    let members = ctx.cargo().workspace_members();

    let mut splits = Vec::new();

    for pkg in members {
        // Filter by requested crates if specified
        if let Some(requested) = requested_crates
            && !requested.contains(&pkg.name)
        {
            continue;
        }

        // Get relative path from workspace root to crate directory
        let Some(crate_dir) = pkg.manifest_path.parent() else {
            // manifest_path always has a parent directory - skip if somehow malformed
            continue;
        };
        // Detect per-crate CHANGELOG file
        let changelog_path = crate::utils::detect_crate_changelog(crate_dir);
        // Generate a reasonable remote URL placeholder (GitHub org/repo pattern)
        let remote = format!("git@github.com:org/{}.git", pkg.name);

        // Check if crate has publish = false in Cargo.toml
        let publish = crate::workspace::CargoState::is_package_publishable(pkg);

        splits.push(SplitConfig {
            name: pkg.name.to_string(),
            remote,
            branch: "main".to_string(),
            mode: SplitMode::Single,
            workspace_mode: WorkspaceMode::default(),
            members: vec![pkg.name.to_string()],
            legacy_paths: vec![],
            include: vec![],
            exclude: vec![],
            publish,
            changelog_path,
        });
    }

    Ok(splits)
}

/// Serialize RailConfig to TOML
fn serialize_splits_config(config: &RailConfig) -> RailResult<String> {
    toml_edit::ser::to_string_pretty(config)
        .map_err(|e| crate::error::RailError::message(format!("config serialization failed: {}", e)))
}

fn build_split_mutation_plan(
    ctx: &WorkspaceContext,
    configs: &[crate::split::SplitParams],
    allow_dirty: bool,
) -> RailResult<mutation::MutationPlan> {
    let source_head = ctx.git()?.git().head_commit().unwrap_or_else(|_| "unknown".to_string());
    let mut sorted_configs = configs.iter().collect::<Vec<_>>();
    sorted_configs.sort_by(|a, b| a.crate_name.cmp(&b.crate_name));

    let actions = sorted_configs
        .into_iter()
        .map(|config| {
            let target_head = SystemGit::open(&config.target_repo_path)
                .and_then(|git| git.head_commit())
                .unwrap_or_else(|_| "none".to_string());
            let mapping_count =
                split_mapping_count(ctx.workspace_root(), &config.crate_name, &config.target_repo_path)?;
            Ok(MutationAction::new(
                "SPLIT_CRATE",
                config.crate_name.clone(),
                Some(format!(
                    "source_head={}, target={}, target_head={}, mapping_count={}",
                    source_head,
                    config.target_repo_path.display(),
                    target_head,
                    mapping_count
                )),
            ))
        })
        .collect::<RailResult<Vec<_>>>()?;

    let mut risks = Vec::new();
    if allow_dirty {
        risks.push(MutationRisk::new(
            "ALLOW_DIRTY_WORKTREE",
            "medium",
            "split is allowed on a dirty worktree",
        ));
    }

    let trace = vec![MutationTrace::new(
        "SPLIT_CONFIGS_RESOLVED",
        format!("resolved {} split config(s)", configs.len()),
    )];

    mutation::build_plan(ctx, "split", actions, risks, trace)
}

fn collect_split_snapshots(
    ctx: &WorkspaceContext,
    configs: &[crate::split::SplitParams],
) -> RailResult<Vec<serde_json::Value>> {
    let source_head = ctx
        .git()
        .and_then(|git| git.git().head_commit())
        .unwrap_or_else(|_| "unknown".to_string());
    let mut out = Vec::new();

    for config in configs {
        let target_head = SystemGit::open(&config.target_repo_path)
            .and_then(|git| git.head_commit())
            .ok();
        let mapping_count = split_mapping_count(ctx.workspace_root(), &config.crate_name, &config.target_repo_path)?;
        out.push(serde_json::json!({
          "crate_name": config.crate_name,
          "source_head": source_head,
          "target_head": target_head,
          "ownership": {
            "snapshot_id": config.ownership.snapshot_id,
            "members": config.ownership.members,
            "dependency_closure": config.ownership.dependency_closure,
            "release_boundaries": config.ownership.release_boundaries.iter().map(|boundary| serde_json::json!({
              "name": boundary.name,
              "members": boundary.members,
            })).collect::<Vec<_>>(),
          },
          "mapping_snapshot": {
            "mapping_count": mapping_count,
          },
        }));
    }
    Ok(out)
}
