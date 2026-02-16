//! `cargo rail run` - planner-contract executor.

use super::plan::{PlanOptions, PlanOutput, build_plan_output, render_plan_explain};
use crate::commands::common::PlanOutputFormat;
use crate::error::{RailError, RailResult};
use crate::git::detect_default_base_ref;
use crate::progress;
use crate::test::runner::select_runner;
use crate::workspace::WorkspaceContext;
use std::collections::BTreeSet;
use std::fs;
use std::process::Command;

/// Options for the `run` command.
#[derive(Clone, Debug, Default)]
pub struct RunOptions {
  /// Git ref to compare against.
  pub since: Option<String>,
  /// Use merge-base with default branch.
  pub merge_base: bool,
  /// Skip planner selection and run all crates.
  pub all: bool,
  /// Explicit surfaces to evaluate/execute.
  pub surfaces: Vec<String>,
  /// Named execution profile.
  pub profile: Option<String>,
  /// Named workflow mapping to a profile via `[run.workflow]`.
  pub workflow: Option<String>,
  /// Preview selected executions without running subprocesses.
  pub dry_run: bool,
  /// Print command(s) before execution.
  pub print_cmd: bool,
  /// Print planner explanation and selected targets.
  pub explain: bool,
  /// Skip binary-only crates for package-scoped surfaces.
  pub ignore_bin_crates: bool,
  /// Disable automatic use of cargo-nextest for test surface.
  pub skip_nextest: bool,
  /// Pass additional arguments to the surface runner.
  pub run_args: Vec<String>,
}

/// Execute `run` with planner-driven surface selection.
pub fn run_run(ctx: &WorkspaceContext, opts: RunOptions) -> RailResult<()> {
  let effective = resolve_effective_inputs(ctx, &opts)?;
  let mut plan = None;

  if !opts.all {
    plan = Some(build_plan_output(
      ctx,
      &PlanOptions {
        since: effective.since.clone(),
        from: None,
        to: None,
        merge_base: effective.merge_base,
        format: PlanOutputFormat::Text,
        output: None,
        explain: false,
        confidence_profile: None,
      },
    )?);
  }

  if opts.explain {
    if let Some(ref output) = plan {
      let explain = render_plan_explain(output);
      print!("{}", explain);
      if !explain.ends_with('\n') {
        println!();
      }
      println!();
    } else {
      println!("mode: all (planner selection skipped)\n");
    }
  }

  let test_targets = resolve_targets(ctx, &opts, plan.as_ref(), "test")?;
  let build_targets = resolve_targets(ctx, &opts, plan.as_ref(), "build")?;
  let bench_targets = resolve_targets(ctx, &opts, plan.as_ref(), "bench")?;

  let requested_test_surface = effective.surfaces.iter().any(|surface| surface == "test");
  let mut executed_any = false;
  let surface_count = effective.surfaces.len();
  let mut executed_surfaces = Vec::with_capacity(surface_count);
  let mut skipped_surfaces = Vec::with_capacity(surface_count);
  for surface in &effective.surfaces {
    if !surface_enabled(&opts, plan.as_ref(), surface) {
      if opts.explain || opts.dry_run {
        println!("skip surface `{}` (not enabled by plan)", surface);
      }
      skipped_surfaces.push(surface.clone());
      continue;
    }

    executed_any = true;
    executed_surfaces.push(surface.clone());
    match surface.as_str() {
      "test" => run_test_surface(&opts, &test_targets, &effective.run_args)?,
      "build" => run_workspace_surface(
        &opts,
        "build",
        "cargo",
        &["check", "--workspace"],
        &build_targets,
        &effective.run_args,
      )?,
      "bench" => run_workspace_surface(
        &opts,
        "bench",
        "cargo",
        &["bench", "--workspace"],
        &bench_targets,
        &effective.run_args,
      )?,
      "docs" => run_workspace_surface(
        &opts,
        "docs",
        "cargo",
        &["doc", "--workspace", "--no-deps"],
        &[],
        &effective.run_args,
      )?,
      "infra" => {
        if opts.dry_run || opts.explain {
          println!("surface `infra` selected; no built-in executor yet");
        }
      }
      unknown => {
        return Err(RailError::with_help(
          format!("unsupported surface '{}'", unknown),
          "use --surface build|test|bench|docs|infra",
        ));
      }
    }
  }

  if !executed_any {
    if requested_test_surface {
      println!("no test targets");
    } else {
      println!("no surfaces to execute");
    }
  }

  let receipt_path = write_run_decision_receipt(
    ctx,
    &opts,
    &effective,
    plan.as_ref(),
    &executed_surfaces,
    &skipped_surfaces,
    DecisionTargets {
      test: &test_targets,
      build: &build_targets,
      bench: &bench_targets,
    },
  )?;
  if std::env::var_os("CI").is_some() {
    progress!("decision receipt: {}", receipt_path.display());
  }

  Ok(())
}

#[derive(Debug, Clone)]
struct EffectiveRunInputs {
  surfaces: Vec<String>,
  profile: Option<String>,
  profile_source: Option<&'static str>,
  workflow: Option<String>,
  since: Option<String>,
  merge_base: bool,
  run_args: Vec<String>,
}

fn resolve_effective_inputs(ctx: &WorkspaceContext, opts: &RunOptions) -> RailResult<EffectiveRunInputs> {
  if !opts.surfaces.is_empty() {
    return Ok(EffectiveRunInputs {
      surfaces: dedup_surfaces(opts.surfaces.clone()),
      profile: None,
      profile_source: None,
      workflow: None,
      since: opts.since.clone(),
      merge_base: opts.merge_base && opts.since.is_none(),
      run_args: opts.run_args.clone(),
    });
  }

  let workflow_profile = if let Some(workflow) = opts.workflow.as_ref() {
    let Some(config) = ctx.config.as_ref() else {
      return Err(RailError::with_help(
        format!("workflow '{}' requested but no rail.toml loaded", workflow),
        "define [run.workflow] in rail.toml or pass --profile/--surface",
      ));
    };
    let Some(mapped_profile) = config.run.workflow.get(workflow).cloned() else {
      return Err(RailError::with_help(
        format!("unknown run workflow '{}'", workflow),
        format!(
          "define run.workflow.{} in rail.toml or pass --profile/--surface",
          workflow
        ),
      ));
    };
    Some(mapped_profile)
  } else {
    None
  };

  let config_default = ctx
    .config
    .as_ref()
    .and_then(|cfg| cfg.run.default_profile.as_ref())
    .cloned();

  let (profile_name, profile_source) = if let Some(name) = opts.profile.as_ref() {
    (name.clone(), "cli")
  } else if let Some(name) = workflow_profile.clone() {
    (name, "workflow")
  } else if let Some(name) = config_default {
    (name, "config_default")
  } else {
    ("local".to_string(), "builtin_default")
  };

  let mut profile_run_args = Vec::new();
  let mut profile_since = None;
  let mut profile_merge_base = false;
  let surfaces = if let Some(cfg_profile) = ctx
    .config
    .as_ref()
    .and_then(|cfg| cfg.run.profiles.get(profile_name.as_str()))
  {
    profile_run_args = cfg_profile.run_args.clone();
    profile_since = cfg_profile.since.clone();
    profile_merge_base = cfg_profile.merge_base.unwrap_or(false);
    dedup_surfaces(cfg_profile.surfaces.clone())
  } else if let Some(builtin_surfaces) = builtin_profile_surfaces(profile_name.as_str()) {
    builtin_surfaces
  } else {
    return Err(RailError::with_help(
      format!("unknown run profile '{}'", profile_name),
      "define [run.profile.<name>] in rail.toml, or use --profile local|ci|nightly, or pass --surface",
    ));
  };

  let mut run_args = profile_run_args;
  if let Some(token_idx) = run_args.iter().position(|arg| arg == "{cargo_args}") {
    let mut expanded = Vec::new();
    expanded.extend_from_slice(&run_args[..token_idx]);
    expanded.extend(opts.run_args.clone());
    expanded.extend_from_slice(&run_args[token_idx + 1..]);
    run_args = expanded;
  } else {
    run_args.extend(opts.run_args.clone());
  }

  let base_ref = detect_default_base_ref(ctx.git.git())?;
  let workspace_root = ctx.workspace_root().display().to_string();
  run_args = run_args
    .into_iter()
    .map(|arg| {
      arg
        .replace("{workspace_root}", &workspace_root)
        .replace("{base_ref}", &base_ref)
    })
    .collect();

  profile_since = profile_since.map(|since| {
    since
      .replace("{workspace_root}", &workspace_root)
      .replace("{base_ref}", &base_ref)
  });

  let since = opts.since.clone().or(profile_since);
  let merge_base = if since.is_some() {
    false
  } else {
    opts.merge_base || profile_merge_base
  };

  Ok(EffectiveRunInputs {
    surfaces,
    profile: Some(profile_name),
    profile_source: Some(profile_source),
    workflow: opts.workflow.clone(),
    since,
    merge_base,
    run_args,
  })
}

fn builtin_profile_surfaces(profile: &str) -> Option<Vec<String>> {
  match profile {
    "local" => Some(vec!["test".to_string()]),
    "ci" => Some(vec!["build".to_string(), "test".to_string()]),
    "nightly" => Some(vec!["build".to_string(), "test".to_string(), "docs".to_string()]),
    _ => None,
  }
}

fn dedup_surfaces(mut surfaces: Vec<String>) -> Vec<String> {
  let mut seen = BTreeSet::new();
  surfaces.retain(|surface| seen.insert(surface.clone()));
  surfaces
}

fn resolve_targets(
  ctx: &WorkspaceContext,
  opts: &RunOptions,
  plan: Option<&PlanOutput>,
  surface: &str,
) -> RailResult<Vec<String>> {
  let mut targets: Vec<String> = if opts.all {
    ctx
      .cargo
      .metadata()
      .workspace_packages()
      .iter()
      .map(|p| p.name.to_string())
      .collect()
  } else if let Some(output) = plan {
    let mut selected = output
      .impact
      .direct_crates
      .iter()
      .chain(output.impact.transitive_crates.iter())
      .cloned()
      .collect::<Vec<_>>();
    selected.sort();
    selected.dedup();

    // Conservative fallback for ownerless/global changes on package-scoped surfaces.
    if selected.is_empty()
      && output
        .surfaces
        .get(surface)
        .map(|decision| decision.enabled)
        .unwrap_or(false)
      && matches!(surface, "build" | "test" | "bench")
    {
      ctx
        .cargo
        .metadata()
        .workspace_packages()
        .iter()
        .map(|p| p.name.to_string())
        .collect()
    } else {
      selected
    }
  } else {
    Vec::new()
  };

  if opts.ignore_bin_crates && matches!(surface, "build" | "test" | "bench") {
    targets.retain(|crate_name| !ctx.cargo.is_binary_only(crate_name));
  }

  Ok(targets)
}

fn surface_enabled(opts: &RunOptions, plan: Option<&PlanOutput>, surface: &str) -> bool {
  opts.all
    || plan
      .and_then(|output| output.surfaces.get(surface))
      .map(|decision| decision.enabled)
      .unwrap_or(false)
}

fn run_test_surface(opts: &RunOptions, targets: &[String], run_args: &[String]) -> RailResult<()> {
  if targets.is_empty() {
    println!("no test targets");
    return Ok(());
  }

  let runner = select_runner(!opts.skip_nextest);
  progress!("testing {} crates ({}):", targets.len(), runner.name());
  for target in targets {
    progress!("  {}", target);
  }

  let mut cmd = runner.build_command(targets, run_args);
  run_or_print_command(opts, "test", &mut cmd)
}

fn run_workspace_surface(
  opts: &RunOptions,
  surface: &str,
  bin: &str,
  args: &[&str],
  targets: &[String],
  run_args: &[String],
) -> RailResult<()> {
  let mut cmd = Command::new(bin);
  cmd.args(args);
  cmd.args(run_args);

  if opts.explain {
    println!("surface `{}` targets: {}", surface, targets.len());
    for target in targets {
      println!("  {}", target);
    }
  }

  run_or_print_command(opts, surface, &mut cmd)
}

fn run_or_print_command(opts: &RunOptions, surface: &str, cmd: &mut Command) -> RailResult<()> {
  if opts.print_cmd || opts.dry_run {
    println!("{}: {}", surface, render_command(cmd));
  }
  if opts.dry_run {
    return Ok(());
  }

  let status = cmd
    .status()
    .map_err(|error| RailError::message(format!("{} failed: {}", surface, error)))?;
  if !status.success() {
    return Err(RailError::ExitWithCode {
      code: status.code().unwrap_or(1),
    });
  }
  Ok(())
}

fn render_command(cmd: &Command) -> String {
  let mut parts = Vec::new();
  parts.push(cmd.get_program().to_string_lossy().into_owned());
  parts.extend(cmd.get_args().map(|arg| arg.to_string_lossy().into_owned()));
  parts.join(" ")
}

struct DecisionTargets<'a> {
  test: &'a [String],
  build: &'a [String],
  bench: &'a [String],
}

fn write_run_decision_receipt(
  ctx: &WorkspaceContext,
  opts: &RunOptions,
  effective: &EffectiveRunInputs,
  plan: Option<&PlanOutput>,
  executed_surfaces: &[String],
  skipped_surfaces: &[String],
  targets: DecisionTargets<'_>,
) -> RailResult<std::path::PathBuf> {
  let dir = ctx.workspace_root().join("target").join("cargo-rail").join("receipts");
  fs::create_dir_all(&dir)?;
  let nonce = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
  let path = dir.join(format!("run-decision-{}.json", nonce));

  let receipt = serde_json::json!({
    "artifact": "decision_receipt",
    "version": 1,
    "command": "run",
    "generated_at_utc": chrono::Utc::now().to_rfc3339(),
    "inputs": {
      "profile_requested": opts.profile,
      "profile_effective": effective.profile,
      "profile_source": effective.profile_source,
      "workflow_requested": opts.workflow,
      "workflow_effective": effective.workflow,
      "surfaces_requested": opts.surfaces,
      "surfaces_effective": effective.surfaces,
      "since_requested": opts.since,
      "since_effective": effective.since,
      "merge_base_requested": opts.merge_base,
      "merge_base_effective": effective.merge_base,
      "all": opts.all,
      "run_args_requested": opts.run_args,
      "run_args_effective": effective.run_args,
      "dry_run": opts.dry_run,
    },
    "execution": {
      "executed_surfaces": executed_surfaces,
      "skipped_surfaces": skipped_surfaces,
      "targets": {
        "test": targets.test,
        "build": targets.build,
        "bench": targets.bench,
      }
    },
    "plan": plan,
  });
  let bytes = serde_json::to_vec_pretty(&receipt)
    .map_err(|e| RailError::message(format!("failed to serialize decision receipt: {}", e)))?;
  fs::write(&path, bytes)
    .map_err(|e| RailError::message(format!("failed to write decision receipt '{}': {}", path.display(), e)))?;
  Ok(path)
}
