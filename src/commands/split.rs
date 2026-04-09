//! `cargo rail split` - Extract crates to standalone repositories with full git history.

use std::io::IsTerminal;

use crate::commands::common::{OutputFormat, SplitSyncConfigBuilder, enforce_safety_gate};
use crate::config::RailConfig;
use crate::error::{GitError, RailError, RailResult};
use crate::git::SystemGit;
use crate::git::mappings::MappingStore;
use crate::mutation::{self, MutationAction, MutationRisk, MutationTrace};
use crate::progress;
use crate::split::SplitEngine;
use crate::utils;
use crate::workspace::WorkspaceContext;
use rayon::prelude::*;

/// Arguments for the split run command
pub struct SplitRunArgs {
  /// Crate name to split (mutually exclusive with `all`)
  pub crate_name: Option<String>,
  /// Split all configured crates
  pub all: bool,
  /// Override remote repository URL
  pub remote: Option<String>,
  /// Dry-run mode: preview changes without executing
  pub check: bool,
  /// Apply from a previously generated mutation plan file
  pub plan_path: Option<std::path::PathBuf>,
  /// Allow running on dirty worktree (uncommitted changes)
  pub allow_dirty: bool,
  /// Skip confirmation prompts (for CI/automation)
  pub yes: bool,
  /// Output format
  pub format: OutputFormat,
}

/// Run the split command
pub fn run_split(ctx: &WorkspaceContext, args: SplitRunArgs) -> RailResult<()> {
  let json = args.format.is_json();

  // JSON mode enables structured error output and suppresses progress
  if json {
    crate::output::set_json_mode(true);
  }

  // Dirty worktree check (unless --allow-dirty or --check mode)
  if !args.check && !args.allow_dirty && ctx.git.git().is_dirty()? {
    let files = ctx.git.git().dirty_files()?;
    return Err(RailError::Git(GitError::DirtyWorktree { files }));
  }

  let builder = SplitSyncConfigBuilder::new(ctx)?
    .with_crate_or_all(args.crate_name.clone(), args.all)?
    .with_remote_override(args.remote)
    .validate()?;

  let config_count = builder.count();
  let configs = builder.build_split_configs()?;
  let snapshots = collect_split_snapshots(ctx, &configs);
  let expected_mutation_plan = build_split_mutation_plan(ctx, &configs, args.allow_dirty)?;

  // Check mode: show plan
  if args.check {
    match args.format {
      OutputFormat::Json => {
        let crates: Vec<_> = configs
          .iter()
          .map(|config| {
            serde_json::json!({
              "crate_name": config.crate_name,
              "mode": format!("{:?}", config.mode),
              "target_repo": config.target_repo_path,
              "branch": config.branch,
              "remote_url": config.remote_url,
            })
          })
          .collect();
        let payload = serde_json::json!({
          "command": "split",
          "check": true,
          "crates": crates,
          "count": configs.len(),
          "planning": {
            "source_head": ctx.git.git().head_commit().unwrap_or_else(|_| "unknown".to_string()),
            "targets": snapshots,
          },
          "mutation_plan": expected_mutation_plan,
        });
        let output = crate::output::machine_json_envelope("split", "check", "pending_changes", 1, payload);
        println!("{}", serde_json::to_string_pretty(&output)?);
      }
      OutputFormat::NamesOnly => {
        for config in &configs {
          println!("{}", config.crate_name);
        }
      }
      OutputFormat::JsonLines => {
        for config in &configs {
          let obj = serde_json::json!({
            "crate_name": config.crate_name,
            "mode": format!("{:?}", config.mode),
            "target_repo": config.target_repo_path.display().to_string(),
            "branch": config.branch,
            "remote_url": config.remote_url,
          });
          println!("{}", serde_json::to_string(&obj)?);
        }
      }
      _ => {
        // Text, GitHub, GitHubMatrix all fall back to text output
        println!("split plan:\n");
        for config in &configs {
          println!("  {}", config.crate_name);
          println!("    mode: {:?}", config.mode);
          println!("    paths:");
          for path in &config.crate_paths {
            println!("      {}", path.display());
          }
          println!("    target: {}", config.target_repo_path.display());
          if let Some(ref remote) = config.remote_url {
            println!("    remote: {}", remote);
          }
          println!("    branch: {}", config.branch);
        }
        println!("\nChanges detected. Run without --check to apply.");
      }
    }
    return Err(crate::error::RailError::CheckHasPendingChanges);
  }

  enforce_safety_gate(
    "split apply",
    args.yes,
    args.plan_path.as_deref(),
    std::io::stdin().is_terminal() && !json,
  )?;

  // Interactive confirmation (unless --yes)
  if !args.yes && std::io::stdin().is_terminal() && !json {
    println!("splitting {} crate(s):\n", config_count);
    for config in &configs {
      println!("  {} -> {}", config.crate_name, config.target_repo_path.display());
    }

    if !utils::prompt_for_confirmation("\nproceed? [Enter/Ctrl+C]")? {
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
    mutation::validate_pre_apply(ctx, &from_file)?;
    if from_file.inputs_fingerprint != expected_mutation_plan.inputs_fingerprint {
      return Err(RailError::with_help(
        "provided split plan does not match current requested operation",
        "regenerate the split plan and rerun with --plan",
      ));
    }
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
  progress!("receipt: {}", plan_receipt.display());

  // Execute splits
  if config_count > 1 && args.all {
    progress!("splitting {} crates...", config_count);
    let results: Vec<RailResult<()>> = configs
      .into_par_iter()
      .map(|config| {
        progress!("  {}", config.crate_name);
        let engine = SplitEngine::new(ctx)?;
        engine.split(&config)
      })
      .collect();

    for result in results {
      result?;
    }
  } else {
    for config in configs {
      progress!("splitting {}...", config.crate_name);
      let engine = SplitEngine::new(ctx)?;
      engine.split(&config)?;
    }
  }

  println!("split complete");
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
  progress!("receipt: {}", apply_receipt.display());
  Ok(())
}

/// Initialize split configuration for crates
pub fn run_split_init(ctx: &WorkspaceContext, crates: Option<Vec<String>>, check: bool) -> RailResult<()> {
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
    change_detection: crate::config::ChangeDetectionConfig::default(),
    run: crate::config::RunConfig::default(),
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
        paths: split.paths,
        include: split.include,
        exclude: split.exclude,
      }),
      release: Some(CrateReleaseConfig { publish: split.publish }),
      changelog: split.changelog_path.map(|path| ChangelogConfig {
        path: Some(path),
        skip: false,
      }),
      sync: None,
    };

    config.crates.insert(split.name, crate_config);
  }

  let config_toml = serialize_splits_config(&config)?;

  if check {
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
    println!("        2. add other crate paths to its 'paths' array");
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
  use crate::config::{CratePath, SplitConfig, SplitMode, WorkspaceMode};

  let workspace_root = ctx.workspace_root();
  let members = ctx.cargo.workspace_members();

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
    let rel_path = match crate_dir.strip_prefix(workspace_root) {
      Ok(p) => p.to_path_buf(),
      Err(_) => continue, // Skip if not under workspace root
    };

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
      paths: vec![CratePath { path: rel_path.into() }],
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
  let source_head = ctx.git.git().head_commit().unwrap_or_else(|_| "unknown".to_string());
  let mut sorted_configs = configs.iter().collect::<Vec<_>>();
  sorted_configs.sort_by(|a, b| a.crate_name.cmp(&b.crate_name));

  let actions = sorted_configs
    .into_iter()
    .map(|config| {
      let target_head = SystemGit::open(&config.target_repo_path)
        .and_then(|git| git.head_commit())
        .unwrap_or_else(|_| "none".to_string());
      let mapping_count = mapping_count_for(ctx.workspace_root(), &config.crate_name, &config.target_repo_path);
      MutationAction::new(
        "SPLIT_CRATE",
        config.crate_name.clone(),
        Some(format!(
          "source_head={}, target={}, target_head={}, mapping_count={}",
          source_head,
          config.target_repo_path.display(),
          target_head,
          mapping_count
        )),
      )
    })
    .collect();

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

fn collect_split_snapshots(ctx: &WorkspaceContext, configs: &[crate::split::SplitParams]) -> Vec<serde_json::Value> {
  let source_head = ctx.git.git().head_commit().unwrap_or_else(|_| "unknown".to_string());
  let mut out = Vec::new();

  for config in configs {
    let target_head = SystemGit::open(&config.target_repo_path)
      .and_then(|git| git.head_commit())
      .ok();
    let mapping_count = mapping_count_for(ctx.workspace_root(), &config.crate_name, &config.target_repo_path);
    out.push(serde_json::json!({
      "crate_name": config.crate_name,
      "source_head": source_head,
      "target_head": target_head,
      "mapping_snapshot": {
        "mapping_count": mapping_count,
      },
    }));
  }
  out
}

fn mapping_count_for(workspace_root: &std::path::Path, crate_name: &str, target_repo_path: &std::path::Path) -> usize {
  let mut store = MappingStore::new(crate_name.to_string());
  let _ = store.load(workspace_root);
  let _ = store.load(target_repo_path);
  store.count()
}
