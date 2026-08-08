//! `cargo rail run` - planner-contract executor.

use super::plan::{
  ExecutionScopeMode, PlanOptions, PlanOutput, build_plan_output, render_plan_explain, resolve_action_packages,
};
use crate::action::{
  ActionEnvironmentEntry, ActionExpansion, ActionFeatureSelection, ActionGraph, ActionKind, ActionReason,
  ActionResolutionBinding, ActionSpec, ActionWorkingDirectory, ArgvTemplate, ExpandedAction, PackageArguments,
};
use crate::action_key::{analyze as analyze_action_key, resolution_identity};
use crate::cargo::{ResolutionFeatures, ResolutionPackages, ResolutionRequest, TargetSpecificationIdentity};
use crate::commands::cli::Commands;
use crate::commands::common::{ActionOutputFormat, PlanOutputFormat, format_preview_list};
use crate::compiler::collector::{prepare_direct_cargo_action, prepare_pre_context_direct_cargo_action};
use crate::compiler::native_cache::DirectCacheBypass;
use crate::config::{CapturedDiscoveredConfig, MAX_ACTIONS};
use crate::error::{RailError, RailResult};
use crate::git::detect_default_base_ref;
use crate::progress;
use crate::test::runner::{TestCommandArgs, TestRunnerPreference, select_runner};
use crate::workspace::WorkspaceContext;
use clap::ValueEnum;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

/// Behavior selected for configured generated-output actions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum GeneratedMode {
  /// Run each generator's read-only staleness check.
  Check,
  /// Update each generator's declared outputs.
  #[default]
  Regenerate,
}

impl GeneratedMode {
  const fn as_str(self) -> &'static str {
    match self {
      Self::Check => "check",
      Self::Regenerate => "regenerate",
    }
  }
}

/// Options for the `run` command.
#[derive(Clone, Debug, Default)]
pub struct RunOptions {
  /// Git ref to compare against.
  pub since: Option<String>,
  /// Use merge-base with default branch.
  pub merge_base: bool,
  /// Skip planner selection and run all crates.
  pub all: bool,
  /// Explicit action IDs to evaluate or execute.
  pub actions: Vec<String>,
  /// Named execution profile.
  pub profile: Option<String>,
  /// Named workflow mapping to a profile via `[run.workflow]`.
  pub workflow: Option<String>,
  /// Preview selected executions without running subprocesses.
  pub dry_run: bool,
  /// Execute supported actions inside the isolated hermetic profile.
  pub hermetic: bool,
  /// Disable all Cargo-Rail build-result cache reads and writes.
  pub no_cache: bool,
  /// Rendering contract for execution previews and CI action plans.
  pub format: ActionOutputFormat,
  /// Generated-output behavior.
  pub generated: GeneratedMode,
  /// Print command(s) before execution.
  pub print_cmd: bool,
  /// Print planner explanation and selected targets.
  pub explain: bool,
  /// Skip binary-only crates for package-scoped surfaces.
  pub ignore_bin_crates: bool,
  /// Disable automatic use of cargo-nextest for test surface.
  pub skip_nextest: bool,
  /// Test runner backend preference.
  pub test_runner: TestRunnerPreference,
  /// Options passed only to `cargo test`.
  pub cargo_test_args: Vec<String>,
  /// Options passed only to `cargo nextest run`.
  pub nextest_args: Vec<String>,
  /// Portable test-name filter.
  pub test_filter: Option<String>,
  /// Pass additional arguments to the surface runner.
  pub run_args: Vec<String>,
  /// Render a read-only hermeticity report instead of a run plan.
  #[doc(hidden)]
  pub hermeticity_doctor: bool,
  /// The CLI request passed the exact process-free P7.1 lookup predicate.
  #[doc(hidden)]
  pub pre_context_cache_request: bool,
}

/// Execute `run` with planner-driven action selection.
pub fn run_run(ctx: &WorkspaceContext, opts: RunOptions) -> RailResult<()> {
  if opts.format.is_json_like() && !opts.dry_run {
    return Err(RailError::with_help(
      "structured run output is a non-executing action plan",
      "add --dry-run when using --format json or --format github",
    ));
  }
  let action_expansion_phase = crate::instrumentation::action_expansion_key_construction_phase();
  let effective = resolve_effective_inputs(ctx, &opts)?;
  validate_executable_actions(ctx, &effective.actions)?;
  let snapshot = ctx.snapshot()?;
  let snapshot_id = snapshot.id().to_string();
  let platform = snapshot.toolchain().host_target().to_string();
  let mut selected_targets = snapshot
    .targets()
    .iter()
    .filter(|target| target.is_build_target())
    .map(|target| match target.specification() {
      TargetSpecificationIdentity::BuiltIn(target) => target.clone(),
      TargetSpecificationIdentity::Custom(target) => target.name().to_string(),
    })
    .collect::<Vec<_>>();
  if selected_targets.is_empty() {
    selected_targets.push(platform.clone());
  }
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

  if opts.explain && opts.format == ActionOutputFormat::Text {
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
  let workspace_package_count = ctx.cargo().metadata().workspace_packages().len();
  let selected_features = selected_cargo_features(&effective.run_args);
  let action_base_ref =
    if opts.all && effective.base_ref.is_none() && actions_require_base_ref(ctx, &effective.actions, opts.generated)? {
      Some(detect_default_base_ref(ctx.git()?.git())?)
    } else {
      None
    };
  let base_ref = effective
    .base_ref
    .as_deref()
    .or(action_base_ref.as_deref())
    .or_else(|| plan.as_ref().map(|output| output.inputs.refs.resolved_base.as_str()))
    .unwrap_or("");

  let requested_test_action = effective.actions.iter().any(|action| action == "test");
  let action_count = effective.actions.len();
  let mut skipped_actions = Vec::with_capacity(action_count);
  let mut reasons_by_action = std::collections::BTreeMap::<String, Vec<ActionReason>>::new();
  let mut reason_edges = std::collections::BTreeSet::new();
  for action in &effective.actions {
    if !action_enabled(ctx, &opts, plan.as_ref(), action) {
      continue;
    }
    let reasons = action_reasons(ctx, &opts, plan.as_ref(), action, ActionKind::from_name(action))?;
    merge_action_reasons(&mut reasons_by_action, action, reasons);
    collect_dependency_reasons(ctx, action, &mut reasons_by_action, &mut reason_edges, &mut Vec::new())?;
  }

  let mut steps = Vec::with_capacity(action_count);
  let mut expanded_actions = Vec::with_capacity(action_count);
  let mut scheduled = HashSet::new();
  let expansion = RunExpansionContext {
    ctx,
    opts: &opts,
    run_args: &effective.run_args,
    test_targets: &test_targets,
    build_targets: &build_targets,
    bench_targets: &bench_targets,
    workspace_package_count,
    selected_targets: &selected_targets,
    selected_features: &selected_features,
    platform: &platform,
    base_ref,
    plan: plan.as_ref(),
  };
  for action in &effective.actions {
    if !action_enabled(ctx, &opts, plan.as_ref(), action) {
      skipped_actions.push(action.clone());
      steps.push(RunStep::Skipped(action.clone()));
      continue;
    }
    schedule_action(
      action,
      &reasons_by_action,
      &mut scheduled,
      &mut steps,
      &mut expanded_actions,
      &expansion,
    )?;
  }

  bind_action_resolution_views(ctx, &mut expanded_actions)?;
  for action in &mut expanded_actions {
    action.bind_action_key(analyze_action_key(action, snapshot)?);
  }

  let graph = ActionGraph::new(snapshot_id, expanded_actions)?;
  drop(action_expansion_phase);
  let executed_actions = graph
    .actions()
    .iter()
    .map(|action| action.id().to_string())
    .collect::<Vec<_>>();
  let executed_any = !executed_actions.is_empty();
  if opts.generated == GeneratedMode::Check && !graph.actions().iter().any(ExpandedAction::is_generated) {
    return Err(RailError::with_help(
      "--generated check selected no generated action",
      "select a generated repository action with --action or --profile",
    ));
  }
  if !opts.dry_run {
    for action in graph.actions() {
      action.validate_runtime_environment()?;
    }
  }
  if opts.format == ActionOutputFormat::Text {
    if opts.hermetic && opts.dry_run {
      println!("fetch: cargo fetch --locked (network allowed; produces immutable source inventory)");
    }
    let mut completed_generated_outputs = Vec::new();
    for step in &steps {
      execute_run_step(step, graph.actions(), &opts, ctx, &mut completed_generated_outputs)?;
    }
  } else {
    render_action_plan(&graph, &steps, &opts, &effective, plan.as_ref())?;
  }

  if !executed_any && opts.format == ActionOutputFormat::Text {
    if requested_test_action {
      println!("no test targets");
    } else {
      println!("no actions to execute");
    }
  }

  if !opts.hermeticity_doctor {
    let receipt_path = write_run_decision_receipt(DecisionReceiptInput {
      ctx,
      opts: &opts,
      effective: &effective,
      plan: plan.as_ref(),
      executed_actions: &executed_actions,
      skipped_actions: &skipped_actions,
      graph: &graph,
    })?;
    if std::env::var_os("CI").is_some() {
      progress!("decision receipt: {}", receipt_path.display());
    }
  }

  Ok(())
}

#[derive(Debug, Clone)]
struct EffectiveRunInputs {
  actions: Vec<String>,
  profile: Option<String>,
  profile_source: Option<&'static str>,
  workflow: Option<String>,
  since: Option<String>,
  merge_base: bool,
  run_args: Vec<String>,
  base_ref: Option<String>,
}

fn resolve_effective_inputs(ctx: &WorkspaceContext, opts: &RunOptions) -> RailResult<EffectiveRunInputs> {
  if !opts.actions.is_empty() {
    return Ok(EffectiveRunInputs {
      actions: dedup_actions(opts.actions.clone()),
      profile: None,
      profile_source: None,
      workflow: None,
      since: opts.since.clone(),
      merge_base: opts.merge_base && opts.since.is_none(),
      run_args: opts.run_args.clone(),
      base_ref: None,
    });
  }

  let workflow_profile = if let Some(workflow) = opts.workflow.as_ref() {
    let Some(config) = ctx.config() else {
      return Err(RailError::with_help(
        format!("workflow '{}' requested but no rail.toml loaded", workflow),
        "define [run.workflow] in rail.toml or pass --profile/--action",
      ));
    };
    let Some(mapped_profile) = config.run.workflow.get(workflow).cloned() else {
      return Err(RailError::with_help(
        format!("unknown run workflow '{}'", workflow),
        format!(
          "define run.workflow.{} in rail.toml or pass --profile/--action",
          workflow
        ),
      ));
    };
    Some(mapped_profile)
  } else {
    None
  };

  let config_default = ctx
    .config()
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
  let actions = if let Some(cfg_profile) = ctx
    .config()
    .as_ref()
    .and_then(|cfg| cfg.run.profiles.get(profile_name.as_str()))
  {
    profile_run_args = cfg_profile.run_args.clone();
    match &cfg_profile.baseline {
      Some(crate::config::RunBaseline::Since { reference }) => profile_since = Some(reference.clone()),
      Some(crate::config::RunBaseline::MergeBase) => profile_merge_base = true,
      None => {}
    }
    dedup_actions(cfg_profile.actions.clone())
  } else if let Some(builtin_actions) = builtin_profile_actions(profile_name.as_str()) {
    builtin_actions
  } else {
    return Err(RailError::with_help(
      format!("unknown run profile '{}'", profile_name),
      "define [run.profile.<name>] in rail.toml, or use --profile local|ci|nightly, or pass --action",
    ));
  };

  let needs_profile_base_ref = profile_run_args.iter().any(|arg| arg.contains("{base_ref}"))
    || profile_since
      .as_deref()
      .is_some_and(|reference| reference.contains("{base_ref}"));
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

  let base_ref = needs_profile_base_ref
    .then(|| ctx.git().and_then(|git| detect_default_base_ref(git.git())))
    .transpose()?;
  let workspace_root = ctx.workspace_root().display().to_string();
  run_args = run_args
    .into_iter()
    .map(|arg| {
      arg
        .replace("{workspace_root}", &workspace_root)
        .replace("{base_ref}", base_ref.as_deref().unwrap_or_default())
    })
    .collect();

  profile_since = profile_since.map(|since| {
    since
      .replace("{workspace_root}", &workspace_root)
      .replace("{base_ref}", base_ref.as_deref().unwrap_or_default())
  });

  let since = opts.since.clone().or(profile_since);
  let merge_base = if since.is_some() {
    false
  } else {
    opts.merge_base || profile_merge_base
  };

  Ok(EffectiveRunInputs {
    actions,
    profile: Some(profile_name),
    profile_source: Some(profile_source),
    workflow: opts.workflow.clone(),
    since,
    merge_base,
    run_args,
    base_ref,
  })
}

fn builtin_profile_actions(profile: &str) -> Option<Vec<String>> {
  match profile {
    "local" => Some(vec!["test".to_string()]),
    "ci" => Some(vec!["build".to_string(), "test".to_string()]),
    "nightly" => Some(vec!["build".to_string(), "test".to_string(), "docs".to_string()]),
    _ => None,
  }
}

fn validate_executable_actions(ctx: &WorkspaceContext, actions: &[String]) -> RailResult<()> {
  if actions.len() > MAX_ACTIONS {
    return Err(RailError::message(format!(
      "run request contains {} actions; at most {MAX_ACTIONS} are allowed",
      actions.len()
    )));
  }
  for action in actions {
    if action == "infra" {
      return Err(RailError::with_help(
        "'infra' is a planner output, not an executable action ID",
        "use infra in run.action.<name>.when or select a built-in action",
      ));
    }

    if action.starts_with("custom:") {
      return Err(RailError::with_help(
        format!("'{}' is a planner output, not an executable action ID", action),
        "use the custom surface in run.action.<name>.when, then select that action",
      ));
    }

    if ActionKind::from_name(action).is_none()
      && !ctx
        .config()
        .is_some_and(|config| config.run.actions.contains_key(action))
    {
      return Err(RailError::with_help(
        format!("unsupported action '{}'", action),
        "use a built-in action or define [run.action.<name>]",
      ));
    }
  }

  Ok(())
}

fn dedup_actions(mut actions: Vec<String>) -> Vec<String> {
  let mut seen: HashSet<String> = HashSet::with_capacity(actions.len());
  actions.retain(|action| seen.insert(action.clone()));
  actions
}

fn resolve_targets(
  ctx: &WorkspaceContext,
  opts: &RunOptions,
  plan: Option<&PlanOutput>,
  surface: &str,
) -> RailResult<Vec<String>> {
  let package_scoped_surface = matches!(surface, "build" | "test" | "bench");
  let mut targets: Vec<String> = if opts.all {
    ctx
      .cargo()
      .metadata()
      .workspace_packages()
      .iter()
      .map(|p| p.name.to_string())
      .collect()
  } else if let Some(scope) = plan.and_then(|plan| plan.surfaces.get(surface).map(|decision| &decision.scope)) {
    if !package_scoped_surface {
      Vec::new()
    } else {
      match scope.mode {
        ExecutionScopeMode::Empty => Vec::new(),
        ExecutionScopeMode::Crates => scope.crates.clone(),
        ExecutionScopeMode::Workspace => ctx
          .cargo()
          .metadata()
          .workspace_packages()
          .iter()
          .map(|p| p.name.to_string())
          .collect(),
      }
    }
  } else {
    Vec::new()
  };

  if opts.ignore_bin_crates && package_scoped_surface {
    targets.retain(|crate_name| !ctx.cargo().is_binary_only(crate_name));
  }

  Ok(targets)
}

fn action_enabled(ctx: &WorkspaceContext, opts: &RunOptions, plan: Option<&PlanOutput>, action: &str) -> bool {
  if opts.all {
    return true;
  }
  if let Some(kind) = ActionKind::from_name(action) {
    return kind.planner_surface().is_some_and(|surface| {
      plan.is_some_and(|plan| {
        plan.surfaces.get(surface).is_some_and(|decision| decision.enabled)
          || (plan.has_semantic_seeds() && matches!(surface, "build" | "test" | "bench"))
      })
    });
  }
  ctx
    .config()
    .and_then(|config| config.run.actions.get(action))
    .is_some_and(|action| {
      action.when.iter().any(|surface| {
        plan
          .and_then(|plan| plan.surfaces.get(surface))
          .map(|decision| decision.enabled)
          .unwrap_or(false)
      })
    })
}

fn selected_cargo_features(arguments: &[String]) -> ActionFeatureSelection {
  let mut features = std::collections::BTreeSet::new();
  let mut all_features = false;
  let mut default_features = true;
  let mut arguments = arguments.iter();
  while let Some(argument) = arguments.next() {
    if argument == "--" {
      break;
    } else if argument == "--all-features" {
      all_features = true;
    } else if argument == "--no-default-features" {
      default_features = false;
    } else if argument == "--features" || argument == "-F" {
      if let Some(value) = arguments.next() {
        features.extend(split_feature_values(value));
      }
    } else if let Some(value) = argument.strip_prefix("--features=") {
      features.extend(split_feature_values(value));
    } else if let Some(value) = argument.strip_prefix("-F")
      && !value.is_empty()
    {
      features.extend(split_feature_values(value));
    }
  }
  ActionFeatureSelection::requested(all_features, default_features, features.into_iter().collect())
}

fn selected_cargo_targets(arguments: &[String], defaults: &[String]) -> Vec<String> {
  let mut targets = std::collections::BTreeSet::new();
  let mut arguments = arguments.iter();
  while let Some(argument) = arguments.next() {
    if argument == "--" {
      break;
    } else if argument == "--target" {
      if let Some(value) = arguments.next() {
        targets.insert(value.clone());
      }
    } else if let Some(value) = argument.strip_prefix("--target=")
      && !value.is_empty()
    {
      targets.insert(value.to_string());
    }
  }
  if targets.is_empty() {
    defaults.to_vec()
  } else {
    targets.into_iter().collect()
  }
}

fn bind_action_resolution_views(ctx: &WorkspaceContext, actions: &mut [ExpandedAction]) -> RailResult<()> {
  for action in actions {
    if !action_uses_cargo_resolution(action) {
      continue;
    }
    if !cargo_resolution_cli_is_modeled(action.argv()) {
      // Cargo CLI configuration and unstable flags can change resolution.
      // Do not manufacture a different metadata request: the action remains
      // uncacheable and executes with its original argv.
      continue;
    }

    let selected_ids = selected_action_package_ids(ctx, action)?;
    let root_package_ids = selected_ids.iter().map(ToString::to_string).collect::<Vec<_>>();
    let packages = if action.selected_packages().is_empty() {
      ResolutionPackages::Workspace
    } else {
      ResolutionPackages::Selected(selected_ids.clone())
    };
    let features = action_resolution_features(ctx, &selected_ids, action.selected_features())?;
    let targets = if action.selected_targets().is_empty() {
      vec![Some(ctx.snapshot()?.toolchain().host_target().to_string())]
    } else {
      action.selected_targets().iter().cloned().map(Some).collect()
    };
    let mut bindings = Vec::with_capacity(targets.len());
    for target in targets {
      let request = ResolutionRequest::new(packages.clone(), features.clone(), target.clone())?;
      let view = ctx.resolution_view(request)?;
      let resolution = resolution_identity(ctx.snapshot()?, &view)?;
      bindings.push(ActionResolutionBinding::new(
        root_package_ids.clone(),
        target,
        action.selected_features().clone(),
        resolution,
      ));
    }
    action.bind_resolution_views(bindings);
  }
  Ok(())
}

fn action_uses_cargo_resolution(action: &ExpandedAction) -> bool {
  match action.kind() {
    ActionKind::Build
    | ActionKind::Test
    | ActionKind::Bench
    | ActionKind::Docs
    | ActionKind::Lint
    | ActionKind::Msrv
    | ActionKind::Package
    | ActionKind::Distribution => true,
    ActionKind::Repository | ActionKind::GeneratedArtifact => action
      .argv()
      .first()
      .and_then(|program| std::path::Path::new(program).file_stem())
      .and_then(|program| program.to_str())
      .is_some_and(|program| program == "cargo"),
    ActionKind::Format | ActionKind::Audit => false,
  }
}

fn cargo_resolution_cli_is_modeled(arguments: &[String]) -> bool {
  arguments
    .iter()
    .take_while(|argument| argument.as_str() != "--")
    .all(|argument| {
      argument != "-Z" && !argument.starts_with("-Z") && argument != "--config" && !argument.starts_with("--config=")
    })
}

fn selected_action_package_ids(
  ctx: &WorkspaceContext,
  action: &ExpandedAction,
) -> RailResult<BTreeSet<cargo_metadata::PackageId>> {
  if action.selected_packages().is_empty() {
    return Ok(ctx.cargo().metadata().workspace_members.iter().cloned().collect());
  }
  action
    .selected_packages()
    .iter()
    .map(|name| {
      ctx
        .graph()
        .workspace_package_by_name(name)
        .map(|package| package.id.clone())
    })
    .collect()
}

fn action_resolution_features(
  ctx: &WorkspaceContext,
  package_ids: &BTreeSet<cargo_metadata::PackageId>,
  selection: &ActionFeatureSelection,
) -> RailResult<ResolutionFeatures> {
  if selection.all_features() {
    return Ok(ResolutionFeatures::AllFeatures);
  }
  if selection.named().is_empty() {
    return Ok(if selection.default_features() {
      ResolutionFeatures::Default
    } else {
      ResolutionFeatures::NoDefaultFeatures
    });
  }

  let packages = ctx
    .cargo()
    .metadata()
    .workspace_packages()
    .iter()
    .copied()
    .filter(|package| package_ids.contains(&package.id))
    .collect::<Vec<_>>();
  let mut features = BTreeMap::<cargo_metadata::PackageId, BTreeSet<String>>::new();
  if selection.default_features() {
    for package in &packages {
      if package.features.contains_key("default") {
        features
          .entry(package.id.clone())
          .or_default()
          .insert("default".to_string());
      }
    }
  }

  for requested in selection.named() {
    if let Some((package_name, feature)) = requested.split_once('/') {
      let mut matching_packages = packages.iter().filter(|package| package.name.as_str() == package_name);
      let package = matching_packages.next().ok_or_else(|| {
        RailError::message(format!(
          "action feature '{}' selects package '{}' outside the action package roots",
          requested, package_name
        ))
      })?;
      if matching_packages.next().is_some() {
        return Err(RailError::message(format!(
          "action feature '{}' has an ambiguous workspace package name",
          requested
        )));
      }
      insert_action_feature(&mut features, package, feature, requested)?;
      continue;
    }

    let mut matched = false;
    for package in &packages {
      if package.features.contains_key(requested) {
        features
          .entry(package.id.clone())
          .or_default()
          .insert(requested.clone());
        matched = true;
      }
    }
    if !matched {
      return Err(RailError::message(format!(
        "action feature '{}' is not declared by any selected package",
        requested
      )));
    }
  }

  Ok(ResolutionFeatures::Selected(features))
}

fn insert_action_feature(
  features: &mut BTreeMap<cargo_metadata::PackageId, BTreeSet<String>>,
  package: &cargo_metadata::Package,
  feature: &str,
  requested: &str,
) -> RailResult<()> {
  if !package.features.contains_key(feature) {
    return Err(RailError::message(format!(
      "action feature '{}' names undeclared feature '{}' on package '{}'",
      requested, feature, package.id
    )));
  }
  features
    .entry(package.id.clone())
    .or_default()
    .insert(feature.to_string());
  Ok(())
}

fn split_feature_values(value: &str) -> impl Iterator<Item = String> + '_ {
  value
    .split(|character: char| character == ',' || character.is_ascii_whitespace())
    .filter(|feature| !feature.is_empty())
    .map(str::to_string)
}

enum RunStep {
  Skipped(String),
  NoTargets(ActionKind),
  Action {
    action_index: usize,
    test_runner_name: Option<&'static str>,
  },
}

#[derive(Serialize)]
struct ActionPlanOutput<'a> {
  artifact: &'static str,
  version: u32,
  snapshot_id: &'a str,
  profile_requested: Option<&'a str>,
  profile_effective: Option<&'a str>,
  workflow_requested: Option<&'a str>,
  workflow_effective: Option<&'a str>,
  actions_requested: &'a [String],
  actions_effective: &'a [String],
  since_requested: Option<&'a str>,
  since_effective: Option<&'a str>,
  merge_base_requested: bool,
  merge_base_effective: bool,
  all: bool,
  run_args_requested: &'a [String],
  run_args_effective: &'a [String],
  generated_mode: GeneratedMode,
  execution_profile: &'static str,
  fetch_action: Option<HermeticFetchPlan>,
  actions: &'a [ExpandedAction],
  skipped_actions: Vec<&'a str>,
  no_target_actions: Vec<&'static str>,
  plan: Option<&'a PlanOutput>,
}

#[derive(Serialize)]
struct HermeticFetchPlan {
  id: &'static str,
  argv: &'static [&'static str],
  network: &'static str,
  produces: &'static str,
  consumer_network: &'static str,
}

fn hermetic_fetch_plan(enabled: bool) -> Option<HermeticFetchPlan> {
  enabled.then_some(HermeticFetchPlan {
    id: "fetch",
    argv: &["cargo", "fetch", "--locked"],
    network: "allowed",
    produces: "immutable_cargo_source_inventory",
    consumer_network: "denied",
  })
}

fn render_action_plan<'a>(
  graph: &'a ActionGraph,
  steps: &'a [RunStep],
  opts: &'a RunOptions,
  effective: &'a EffectiveRunInputs,
  plan: Option<&'a PlanOutput>,
) -> RailResult<()> {
  let output = ActionPlanOutput {
    artifact: if opts.hermeticity_doctor {
      "hermeticity_report"
    } else {
      "action_plan"
    },
    version: if opts.hermeticity_doctor { 1 } else { 4 },
    snapshot_id: graph.snapshot_id(),
    profile_requested: opts.profile.as_deref(),
    profile_effective: effective.profile.as_deref(),
    workflow_requested: opts.workflow.as_deref(),
    workflow_effective: effective.workflow.as_deref(),
    actions_requested: &opts.actions,
    actions_effective: &effective.actions,
    since_requested: opts.since.as_deref(),
    since_effective: effective.since.as_deref(),
    merge_base_requested: opts.merge_base,
    merge_base_effective: effective.merge_base,
    all: opts.all,
    run_args_requested: &opts.run_args,
    run_args_effective: &effective.run_args,
    generated_mode: opts.generated,
    execution_profile: if opts.hermetic { "hermetic" } else { "normal" },
    fetch_action: hermetic_fetch_plan(opts.hermetic),
    actions: graph.actions(),
    skipped_actions: steps
      .iter()
      .filter_map(|step| match step {
        RunStep::Skipped(action) => Some(action.as_str()),
        RunStep::NoTargets(_) | RunStep::Action { .. } => None,
      })
      .collect(),
    no_target_actions: steps
      .iter()
      .filter_map(|step| match step {
        RunStep::NoTargets(kind) => Some(kind.as_str()),
        RunStep::Skipped(_) | RunStep::Action { .. } => None,
      })
      .collect(),
    plan,
  };
  match opts.format {
    ActionOutputFormat::Text => Ok(()),
    ActionOutputFormat::Json => {
      let payload = serde_json::to_value(&output)
        .map_err(|error| RailError::message(format!("failed to serialize action plan: {error}")))?;
      let envelope = crate::output::machine_json_envelope(
        if opts.hermeticity_doctor { "doctor" } else { "run" },
        if opts.hermeticity_doctor { "hermeticity" } else { "plan" },
        "success",
        0,
        payload,
      );
      let rendered = serde_json::to_string_pretty(&envelope)
        .map_err(|error| RailError::message(format!("failed to render action plan: {error}")))?;
      println!("{rendered}");
      Ok(())
    }
    ActionOutputFormat::GitHub => {
      use std::fmt::Write as _;

      let action_ids = graph.actions().iter().map(ExpandedAction::id).collect::<Vec<_>>();
      let ids_json = serde_json::to_string(&action_ids)
        .map_err(|error| RailError::message(format!("failed to serialize action IDs: {error}")))?;
      let mut rendered = String::with_capacity(ids_json.len() + graph.snapshot_id().len() + 96);
      let _ = writeln!(rendered, "snapshot_id={}", graph.snapshot_id());
      let _ = writeln!(rendered, "action_count={}", action_ids.len());
      let _ = writeln!(rendered, "action_ids_json={ids_json}");
      let _ = writeln!(rendered, "generated_mode={}", opts.generated.as_str());
      let _ = writeln!(
        rendered,
        "execution_profile={}",
        if opts.hermetic { "hermetic" } else { "normal" }
      );
      print!("{rendered}");
      Ok(())
    }
  }
}

struct RunExpansionContext<'a> {
  ctx: &'a WorkspaceContext,
  opts: &'a RunOptions,
  run_args: &'a [String],
  test_targets: &'a [String],
  build_targets: &'a [String],
  bench_targets: &'a [String],
  workspace_package_count: usize,
  selected_targets: &'a [String],
  selected_features: &'a ActionFeatureSelection,
  platform: &'a str,
  base_ref: &'a str,
  plan: Option<&'a PlanOutput>,
}

fn merge_action_reasons(
  reasons_by_action: &mut std::collections::BTreeMap<String, Vec<ActionReason>>,
  action: &str,
  reasons: Vec<ActionReason>,
) {
  let existing = reasons_by_action.entry(action.to_string()).or_default();
  for reason in reasons {
    if !existing.contains(&reason) {
      existing.push(reason);
    }
  }
}

fn action_dependencies(ctx: &WorkspaceContext, action: &str) -> RailResult<Vec<String>> {
  if ActionKind::from_name(action).is_some() {
    return Ok(Vec::new());
  }
  ctx
    .config()
    .and_then(|config| config.run.actions.get(action))
    .map(|action| action.dependencies.clone())
    .ok_or_else(|| RailError::message(format!("configured action '{action}' is missing")))
}

fn actions_require_base_ref(
  ctx: &WorkspaceContext,
  roots: &[String],
  generated_mode: GeneratedMode,
) -> RailResult<bool> {
  let mut pending = roots.to_vec();
  let mut visited = HashSet::new();
  while let Some(action) = pending.pop() {
    if !visited.insert(action.clone()) || ActionKind::from_name(&action).is_some() {
      continue;
    }
    let config = ctx
      .config()
      .and_then(|config| config.run.actions.get(&action))
      .ok_or_else(|| RailError::message(format!("configured action '{action}' is missing")))?;
    let argv =
      if generated_mode == GeneratedMode::Check && config.kind == crate::config::RepositoryActionKind::Generated {
        &config.check_argv
      } else {
        &config.argv
      };
    if argv.iter().any(|argument| argument == "{base_ref}") {
      return Ok(true);
    }
    pending.extend(config.dependencies.iter().cloned());
  }
  Ok(false)
}

fn collect_dependency_reasons(
  ctx: &WorkspaceContext,
  action: &str,
  reasons_by_action: &mut std::collections::BTreeMap<String, Vec<ActionReason>>,
  visited_edges: &mut std::collections::BTreeSet<(String, String)>,
  stack: &mut Vec<String>,
) -> RailResult<()> {
  if stack.iter().any(|ancestor| ancestor == action) {
    stack.push(action.to_string());
    return Err(RailError::message(format!(
      "action dependency cycle contains: {}",
      stack.join(" -> ")
    )));
  }
  stack.push(action.to_string());
  for dependency in action_dependencies(ctx, action)? {
    if !visited_edges.insert((action.to_string(), dependency.clone())) {
      continue;
    }
    merge_action_reasons(
      reasons_by_action,
      &dependency,
      vec![ActionReason::Dependency {
        action_id: action.to_string(),
      }],
    );
    collect_dependency_reasons(ctx, &dependency, reasons_by_action, visited_edges, stack)?;
  }
  stack.pop();
  Ok(())
}

fn schedule_action(
  action: &str,
  reasons_by_action: &std::collections::BTreeMap<String, Vec<ActionReason>>,
  scheduled: &mut HashSet<String>,
  steps: &mut Vec<RunStep>,
  expanded_actions: &mut Vec<ExpandedAction>,
  expansion: &RunExpansionContext<'_>,
) -> RailResult<()> {
  if scheduled.contains(action) {
    return Ok(());
  }
  for dependency in action_dependencies(expansion.ctx, action)? {
    schedule_action(
      &dependency,
      reasons_by_action,
      scheduled,
      steps,
      expanded_actions,
      expansion,
    )?;
  }
  let reasons = reasons_by_action
    .get(action)
    .cloned()
    .ok_or_else(|| RailError::message(format!("action '{action}' has no authorization reason")))?;
  steps.push(expand_selected_action(action, reasons, expanded_actions, expansion)?);
  scheduled.insert(action.to_string());
  Ok(())
}

fn expand_selected_action(
  action: &str,
  reasons: Vec<ActionReason>,
  expanded_actions: &mut Vec<ExpandedAction>,
  expansion: &RunExpansionContext<'_>,
) -> RailResult<RunStep> {
  let Some(kind) = ActionKind::from_name(action) else {
    let config = expansion
      .ctx
      .config()
      .and_then(|config| config.run.actions.get(action))
      .ok_or_else(|| RailError::message(format!("configured action '{action}' disappeared during expansion")))?;
    let targets = if config.packages == crate::config::RepositoryPackageSelection::None {
      Vec::new()
    } else {
      expansion.build_targets.to_vec()
    };
    let spec = ActionSpec::repository(action, config, expansion.ctx.workspace_root())?;
    let expanded = spec.expand(ActionExpansion {
      selected_packages: targets,
      use_workspace: config.packages == crate::config::RepositoryPackageSelection::WorkspaceOrSelected
        && !expansion.opts.ignore_bin_crates
        && expansion.build_targets.len() == expansion.workspace_package_count,
      selected_targets: expansion.selected_targets.to_vec(),
      selected_features: expansion.selected_features.clone(),
      platform: expansion.platform.to_string(),
      workspace_root: expansion.ctx.workspace_root(),
      base_ref: expansion.base_ref,
      check_generated: expansion.opts.generated == GeneratedMode::Check,
      reasons,
    })?;
    let action_index = expanded_actions.len();
    expanded_actions.push(expanded);
    return Ok(RunStep::Action {
      action_index,
      test_runner_name: None,
    });
  };
  let targets = match kind {
    ActionKind::Test => expansion.test_targets,
    ActionKind::Build
    | ActionKind::Format
    | ActionKind::Lint
    | ActionKind::Msrv
    | ActionKind::Package
    | ActionKind::Distribution => expansion.build_targets,
    ActionKind::Bench => expansion.bench_targets,
    ActionKind::Docs | ActionKind::Audit => &[],
    ActionKind::GeneratedArtifact | ActionKind::Repository => {
      return Err(RailError::message(format!(
        "internal action kind '{}' cannot be selected as a built-in",
        kind.as_str()
      )));
    }
  };
  expand_builtin_action(kind, targets, reasons, expanded_actions, expansion)
}

fn expand_builtin_action(
  kind: ActionKind,
  targets: &[String],
  reasons: Vec<ActionReason>,
  expanded_actions: &mut Vec<ExpandedAction>,
  expansion: &RunExpansionContext<'_>,
) -> RailResult<RunStep> {
  let ctx = expansion.ctx;
  let opts = expansion.opts;
  let run_args = expansion.run_args;
  let workspace_package_count = expansion.workspace_package_count;
  let package_scoped = matches!(
    kind,
    ActionKind::Build
      | ActionKind::Test
      | ActionKind::Bench
      | ActionKind::Format
      | ActionKind::Lint
      | ActionKind::Msrv
      | ActionKind::Package
      | ActionKind::Distribution
  );
  let mut expanded_features = expansion.selected_features.clone();
  let mut expanded_targets = selected_cargo_targets(run_args, expansion.selected_targets);
  let (argv, mut use_workspace, test_runner_name) = match kind {
    ActionKind::Build => (
      builtin_build_template(run_args),
      !opts.ignore_bin_crates && targets.len() == workspace_package_count,
      None,
    ),
    ActionKind::Bench => (
      ArgvTemplate::new(
        "cargo",
        vec!["bench".to_string()],
        PackageArguments::WorkspaceOrSelected,
        run_args.to_vec(),
      ),
      !opts.ignore_bin_crates && targets.len() == workspace_package_count,
      None,
    ),
    ActionKind::Docs => (
      ArgvTemplate::new(
        "cargo",
        vec!["doc".to_string(), "--workspace".to_string(), "--no-deps".to_string()],
        PackageArguments::None,
        run_args.to_vec(),
      ),
      false,
      None,
    ),
    ActionKind::Format => (
      ArgvTemplate::new(
        "cargo",
        vec!["fmt".to_string()],
        PackageArguments::AllOrSelected,
        [vec!["--check".to_string()], run_args.to_vec()].concat(),
      ),
      !opts.ignore_bin_crates && targets.len() == workspace_package_count,
      None,
    ),
    ActionKind::Lint => (
      ArgvTemplate::new(
        "cargo",
        vec!["clippy".to_string()],
        PackageArguments::WorkspaceOrSelected,
        [
          vec!["--all-targets".to_string(), "--all-features".to_string()],
          run_args.to_vec(),
          vec!["--".to_string(), "-D".to_string(), "warnings".to_string()],
        ]
        .concat(),
      ),
      !opts.ignore_bin_crates && targets.len() == workspace_package_count,
      None,
    ),
    ActionKind::Msrv => {
      let msrv = ctx
        .cargo()
        .metadata()
        .workspace_packages()
        .iter()
        .filter_map(|package| package.rust_version.as_ref())
        .max()
        .ok_or_else(|| {
          RailError::with_help(
            "the msrv action requires rust-version on at least one workspace package",
            "set workspace.package.rust-version or package.rust-version before selecting the msrv action",
          )
        })?;
      (
        ArgvTemplate::new(
          "cargo",
          vec![format!("+{msrv}"), "check".to_string()],
          PackageArguments::WorkspaceOrSelected,
          [
            vec![
              "--all-targets".to_string(),
              "--all-features".to_string(),
              "--locked".to_string(),
            ],
            run_args.to_vec(),
          ]
          .concat(),
        ),
        !opts.ignore_bin_crates && targets.len() == workspace_package_count,
        None,
      )
    }
    ActionKind::Package => (
      ArgvTemplate::new(
        "cargo",
        vec!["package".to_string()],
        PackageArguments::WorkspaceOrSelected,
        [vec!["--locked".to_string()], run_args.to_vec()].concat(),
      ),
      !opts.ignore_bin_crates && targets.len() == workspace_package_count,
      None,
    ),
    ActionKind::Audit => (
      ArgvTemplate::new(
        "cargo",
        vec!["deny".to_string(), "check".to_string(), "all".to_string()],
        PackageArguments::None,
        run_args.to_vec(),
      ),
      false,
      None,
    ),
    ActionKind::Distribution => (
      ArgvTemplate::new(
        "cargo",
        vec!["build".to_string()],
        PackageArguments::WorkspaceOrSelected,
        [vec!["--release".to_string(), "--locked".to_string()], run_args.to_vec()].concat(),
      ),
      !opts.ignore_bin_crates && targets.len() == workspace_package_count,
      None,
    ),
    ActionKind::Test => {
      let test_args = TestCommandArgs {
        cargo: opts.cargo_test_args.clone(),
        nextest: opts.nextest_args.clone(),
        filter: opts.test_filter.clone(),
        harness: run_args.to_vec(),
      };
      let preference = if opts.skip_nextest {
        TestRunnerPreference::Cargo
      } else {
        opts.test_runner
      };
      let runner = select_runner(preference, &test_args)?;
      let (before_packages, after_packages) = runner.command_argv_parts(&test_args)?;
      expanded_features = selected_cargo_features(&after_packages);
      expanded_targets = selected_cargo_targets(&after_packages, expansion.selected_targets);
      (
        ArgvTemplate::new("cargo", before_packages, PackageArguments::Selected, after_packages),
        false,
        Some(runner.name()),
      )
    }
    ActionKind::GeneratedArtifact | ActionKind::Repository => {
      return Err(RailError::message("repository action reached built-in expansion"));
    }
  };
  let spec = ActionSpec::builtin(kind, argv);
  if matches!(kind, ActionKind::Lint | ActionKind::Msrv) {
    expanded_features = ActionFeatureSelection::requested(true, true, Vec::new());
  }
  let mut targets = if !opts.all
    && matches!(
      kind,
      ActionKind::Build
        | ActionKind::Test
        | ActionKind::Bench
        | ActionKind::Lint
        | ActionKind::Msrv
        | ActionKind::Package
        | ActionKind::Distribution
    ) {
    let plan = expansion
      .plan
      .ok_or_else(|| RailError::message(format!("action '{}' has no semantic plan", kind.as_str())))?;
    let all_packages: BTreeSet<_> = ctx.cargo().metadata().workspace_members.iter().cloned().collect();
    let features = action_resolution_features(ctx, &all_packages, &expanded_features)?;
    resolve_action_packages(
      ctx,
      plan,
      kind
        .planner_surface()
        .ok_or_else(|| RailError::message(format!("action '{}' has no planner surface", kind.as_str())))?,
      features,
      &expanded_targets,
    )?
  } else {
    targets.to_vec()
  };
  if opts.ignore_bin_crates && package_scoped {
    targets.retain(|crate_name| !ctx.cargo().is_binary_only(crate_name));
  }
  if package_scoped && targets.is_empty() {
    return Ok(RunStep::NoTargets(kind));
  }
  if kind != ActionKind::Test && package_scoped {
    use_workspace = !opts.ignore_bin_crates && targets.len() == workspace_package_count;
  }
  let expanded = spec.expand(ActionExpansion {
    selected_packages: targets,
    use_workspace,
    selected_targets: expanded_targets,
    selected_features: expanded_features,
    platform: expansion.platform.to_string(),
    workspace_root: ctx.workspace_root(),
    base_ref: expansion.base_ref,
    check_generated: opts.generated == GeneratedMode::Check,
    reasons,
  })?;
  let action_index = expanded_actions.len();
  expanded_actions.push(expanded);

  Ok(RunStep::Action {
    action_index,
    test_runner_name,
  })
}

fn builtin_build_template(run_args: &[String]) -> ArgvTemplate {
  ArgvTemplate::new(
    "cargo",
    vec!["check".to_string()],
    PackageArguments::WorkspaceOrSelected,
    run_args.to_vec(),
  )
}

/// Execute one exact all-workspace built-in Cargo action before captured planning.
///
/// Exact cache bypasses delegate unchanged to Cargo. Clean profiles install
/// the existing compiler-unit cache only after a narrow Cargo/toolchain/target
/// capture proves the same direct-action contract. Every ambiguous or
/// unsupported request retains the full captured planner/runner path.
pub(crate) fn try_complete_exact_builtin_cargo_action(
  command: &Commands,
  requested_workspace_root: &std::path::Path,
) -> RailResult<bool> {
  let Commands::Run {
    since: None,
    merge_base: false,
    all: true,
    actions,
    profile: None,
    workflow: None,
    dry_run: false,
    hermetic: false,
    no_cache: false,
    format: ActionOutputFormat::Text,
    generated: GeneratedMode::Regenerate,
    explain: false,
    ignore_bin_crates: false,
    skip_nextest: false,
    test_runner: TestRunnerPreference::Auto,
    cargo_test_args,
    nextest_args,
    test_filter: None,
    run_args,
    print_cmd,
  } = command
  else {
    return Ok(false);
  };
  if !cargo_test_args.is_empty() || !nextest_args.is_empty() {
    return Ok(false);
  }
  let workspace_root = crate::utils::canonicalize_existing(requested_workspace_root)?;
  if !manifest_declares_workspace(&workspace_root) {
    return Ok(false);
  }
  let captured_config = match CapturedDiscoveredConfig::capture(&workspace_root) {
    Ok(config) => config,
    Err(_) => return Ok(false),
  };
  if !rail_configuration_allows_builtin_delegation(&captured_config, &workspace_root) {
    return Ok(false);
  }
  let cache_enabled = captured_config.cache_enabled();
  let l2_alias = captured_config.config().and_then(|config| config.cache.l2.as_deref());
  let (action_id, action_kind, mut arguments) = match actions.as_slice() {
    [action] if action == "build" => (
      "build",
      ActionKind::Build,
      vec!["check".to_string(), "--workspace".to_string()],
    ),
    [action] if action == "distribution" => (
      "distribution",
      ActionKind::Distribution,
      vec![
        "build".to_string(),
        "--workspace".to_string(),
        "--release".to_string(),
        "--locked".to_string(),
      ],
    ),
    _ => return Ok(false),
  };
  arguments.extend(run_args.iter().cloned());
  if cargo_cli_configuration_override(&arguments) || cargo_option_value(&arguments, "--target").is_some() {
    return Ok(false);
  }
  let cargo_config = Arc::new(crate::cargo::CargoConfigSnapshot::capture(&workspace_root)?);
  let target_directory = static_cargo_target_directory(&workspace_root, &arguments, &cargo_config)?;
  let cargo_incremental = std::env::var_os("CARGO_INCREMENTAL");
  let cache_policy = target_directory.as_ref().and_then(|target_directory| {
    native_cache_policy_bypass_reason(
      action_kind,
      &arguments,
      target_directory,
      &workspace_root,
      cargo_incremental.as_deref(),
      std::env::var_os("RUSTC_FORCE_INCREMENTAL").is_some(),
    )
  });
  let pass_through = if !cache_enabled {
    Some(DirectCacheBypass::DisabledByConfiguration)
  } else if cache_policy.is_some() {
    cache_policy
  } else {
    crate::compiler::native_cache::direct_cache_bypass_reason(cargo_config.cache_wrapper_plan(&workspace_root)?)
  };

  let mut argv = Vec::with_capacity(arguments.len() + 1);
  argv.push("cargo".to_string());
  argv.extend(arguments);
  if let Some(reason) = pass_through {
    if !captured_config.validate_unchanged() {
      return Ok(false);
    }
    if *print_cmd {
      println!("{action_id}: {}", argv.join(" "));
    }
    let cargo_program = cargo_config.selected_cargo_program(&workspace_root)?;
    let mut child = Command::new(cargo_program);
    crate::compiler::native_cache::remove_private_environment(&mut child);
    let status = {
      let _cargo_child_execution_phase = crate::instrumentation::cargo_child_execution_phase();
      child
        .args(&argv[1..])
        .current_dir(&workspace_root)
        .status()
        .map_err(|error| RailError::message(format!("{action_id} failed: {error}")))?
    };
    if !status.success() {
      return Err(RailError::ExitWithCode {
        code: status.code().unwrap_or(1),
      });
    }
    progress!(
      "action `{action_id}` native compiler cache: bypassed reason={}",
      reason.as_str()
    );
    write_pre_context_cargo_receipt(
      &workspace_root,
      action_id,
      action_kind,
      &argv,
      run_args,
      PreContextExecution::CargoPassThrough(reason),
    )?;
    return Ok(true);
  }
  if target_directory.is_none() {
    return Ok(false);
  }
  if cargo_incremental.is_some() || !workspace_root.join("Cargo.lock").is_file() {
    return Ok(false);
  }
  if !captured_config.validate_unchanged() {
    return Ok(false);
  }

  let (native_cache, cargo_program) = {
    let _native_cache_setup_phase = crate::instrumentation::native_cache_setup_phase();
    match prepare_pre_context_direct_cargo_action(&workspace_root, cargo_config, cache_enabled, l2_alias, false) {
      Ok((setup @ crate::compiler::native_cache::DirectNativeCacheSetup::Active(_), cargo_program)) => {
        (setup, cargo_program)
      }
      Ok((crate::compiler::native_cache::DirectNativeCacheSetup::Bypassed(_), _)) => return Ok(false),
      Ok((crate::compiler::native_cache::DirectNativeCacheSetup::OperationalFailure(message), _)) => {
        return Err(RailError::message(message));
      }
      Err(_) => return Ok(false),
    }
  };

  if !captured_config.validate_unchanged() {
    return Ok(false);
  }

  let mut child = Command::new(cargo_program);
  if native_cache.remote_active() {
    crate::remote_cache::scrub_child_environment(&mut child);
  }
  if let Some(configuration) = native_cache.cargo_config_argument() {
    child.arg("--config").arg(configuration);
    if std::env::var_os("CARGO_INCREMENTAL").is_none() {
      child.env("CARGO_INCREMENTAL", "0");
    }
  }
  if *print_cmd {
    println!("{action_id}: {}", argv.join(" "));
  }
  let status = {
    let _cargo_child_execution_phase = crate::instrumentation::cargo_child_execution_phase();
    child
      .args(&argv[1..])
      .current_dir(&workspace_root)
      .status()
      .map_err(|error| RailError::message(format!("{action_id} failed: {error}")))?
  };
  let native_cache_report = report_native_cache_decision(action_id, &native_cache, false);
  if !status.success() {
    return Err(RailError::ExitWithCode {
      code: status.code().unwrap_or(1),
    });
  }
  native_cache_report?;
  write_pre_context_cargo_receipt(
    &workspace_root,
    action_id,
    action_kind,
    &argv,
    run_args,
    PreContextExecution::NativeCompilerCache,
  )?;
  Ok(true)
}

const STATIC_WORKSPACE_MANIFEST_MAX_BYTES: u64 = 16 * 1024 * 1024;

fn manifest_declares_workspace(workspace_root: &std::path::Path) -> bool {
  let Ok(manifest) = fs::File::open(workspace_root.join("Cargo.toml")) else {
    return false;
  };
  let Ok(metadata) = manifest.metadata() else {
    return false;
  };
  if !metadata.is_file() || metadata.len() > STATIC_WORKSPACE_MANIFEST_MAX_BYTES {
    return false;
  }
  let mut contents = String::with_capacity(metadata.len() as usize);
  if manifest
    .take(STATIC_WORKSPACE_MANIFEST_MAX_BYTES + 1)
    .read_to_string(&mut contents)
    .is_err()
    || contents.len() as u64 > STATIC_WORKSPACE_MANIFEST_MAX_BYTES
  {
    return false;
  }
  contents
    .parse::<toml_edit::DocumentMut>()
    .is_ok_and(|document| document.get("workspace").is_some())
}

fn rail_configuration_allows_builtin_delegation(
  captured: &CapturedDiscoveredConfig,
  workspace_root: &std::path::Path,
) -> bool {
  captured.config().is_none_or(|config| {
    config.cache.validate().is_ok()
      && config.change_detection.validate().is_ok()
      && config.unify.validate(workspace_root).is_ok()
      && config.run.validate().is_ok()
  })
}

fn static_cargo_target_directory(
  workspace_root: &std::path::Path,
  arguments: &[String],
  cargo_config: &crate::cargo::CargoConfigSnapshot,
) -> RailResult<Option<PathBuf>> {
  let build = cargo_config
    .effective_file_settings()
    .get("build")
    .and_then(serde_json::Value::as_object);
  if cargo_option_value(arguments, "--target").is_none()
    && (build.is_some_and(|build| build.contains_key("target")) || std::env::var_os("CARGO_BUILD_TARGET").is_some())
  {
    return Ok(None);
  }
  let selected = if let Some(directory) = cargo_option_value(arguments, "--target-dir") {
    PathBuf::from(directory)
  } else if let Some(directory) = std::env::var_os("CARGO_TARGET_DIR") {
    if directory.is_empty() {
      return Ok(None);
    }
    PathBuf::from(directory)
  } else {
    if build.is_some_and(|build| build.contains_key("target-dir")) {
      return Ok(None);
    }
    PathBuf::from("target")
  };
  Ok(Some(if selected.is_absolute() {
    selected
  } else {
    workspace_root.join(selected)
  }))
}

#[derive(Clone, Copy)]
enum PreContextExecution {
  CargoPassThrough(DirectCacheBypass),
  NativeCompilerCache,
}

fn write_pre_context_cargo_receipt(
  workspace_root: &std::path::Path,
  action_id: &str,
  action_kind: ActionKind,
  argv: &[String],
  run_args: &[String],
  execution: PreContextExecution,
) -> RailResult<()> {
  let (snapshot_status, action_source, execution_mode, native_cache, native_cache_reason) = match execution {
    PreContextExecution::CargoPassThrough(DirectCacheBypass::ActiveCargoProfile) => (
      "not_loaded_for_active_cargo_profile_delegation",
      "builtin_active_cargo_profile_delegation",
      "active_cargo_profile_delegation",
      "bypassed",
      Some(DirectCacheBypass::ActiveCargoProfile.as_str()),
    ),
    PreContextExecution::CargoPassThrough(reason) => (
      "not_loaded_for_exact_cargo_pass_through",
      "builtin_exact_cargo_pass_through",
      "exact_cargo_pass_through",
      "bypassed",
      Some(reason.as_str()),
    ),
    PreContextExecution::NativeCompilerCache => (
      "not_loaded_for_exact_native_compiler_cache_execution",
      "builtin_exact_native_compiler_cache_execution",
      "exact_native_compiler_cache_execution",
      "active",
      None,
    ),
  };
  let receipt = serde_json::json!({
    "artifact": "decision_receipt",
    "version": 4,
    "command": "run",
    "generated_at_utc": chrono::Utc::now().to_rfc3339(),
    "snapshot_id": null,
    "snapshot_status": snapshot_status,
    "actions": [{
      "id": action_id,
      "kind": action_kind.as_str(),
      "argv": argv,
      "action_key": null,
      "source": action_source,
    }],
    "inputs": {
      "profile_requested": null,
      "profile_effective": null,
      "profile_source": null,
      "workflow_requested": null,
      "workflow_effective": null,
      "actions_requested": [action_id],
      "actions_effective": [action_id],
      "since_requested": null,
      "since_effective": null,
      "merge_base_requested": false,
      "merge_base_effective": false,
      "all": true,
      "run_args_requested": run_args,
      "run_args_effective": run_args,
      "test_runner": TestRunnerPreference::Auto,
      "cargo_test_args": [],
      "nextest_args": [],
      "test_filter": null,
      "dry_run": false,
      "format": ActionOutputFormat::Text,
      "generated_mode": GeneratedMode::Regenerate,
      "execution_profile": "normal",
    },
    "execution": {
      "executed_actions": [action_id],
      "skipped_actions": [],
      "execution_mode": execution_mode,
      "native_cache": native_cache,
      "native_cache_reason": native_cache_reason,
      "cargo_executed": true,
      "compiler_units_executed": null,
      "fetch_action": null,
    },
    "scope": null,
    "plan": null,
  });
  let receipt_path = persist_run_decision_receipt(workspace_root, &receipt)?;
  if std::env::var_os("CI").is_some() {
    progress!("decision receipt: {}", receipt_path.display());
  }
  Ok(())
}

/// Let Cargo own a backend-selection failure that prevented cargo-rail from
/// acquiring its snapshot. This is deliberately a failed-probe fallback, not
/// an eligibility fast path: valid backends retain the ordinary captured
/// execution path and its receipt.
pub(crate) fn try_complete_codegen_backend_probe_failure(
  command: &Commands,
  workspace_root: &std::path::Path,
  error: &RailError,
) -> RailResult<bool> {
  if !error.is_compiler_configuration_probe_failure() || !configured_codegen_backend_present(workspace_root) {
    return Ok(false);
  }
  let Commands::Run {
    since: None,
    merge_base: false,
    all: true,
    actions,
    profile: None,
    workflow: None,
    dry_run: false,
    hermetic: false,
    format: ActionOutputFormat::Text,
    generated: GeneratedMode::Regenerate,
    print_cmd: false,
    explain: false,
    ignore_bin_crates: false,
    skip_nextest: false,
    test_runner: TestRunnerPreference::Auto,
    cargo_test_args,
    nextest_args,
    test_filter: None,
    run_args,
    ..
  } = command
  else {
    return Ok(false);
  };
  if actions.as_slice() != ["build"] || !cargo_test_args.is_empty() || !nextest_args.is_empty() {
    return Ok(false);
  }

  let argv = builtin_build_template(run_args).expand(&[], true)?;
  let (_, arguments) = argv
    .split_first()
    .ok_or_else(|| RailError::message("built-in build argv cannot be empty"))?;
  let cargo = std::env::var_os("CARGO")
    .filter(|program| !program.is_empty())
    .unwrap_or_else(|| std::ffi::OsString::from("cargo"));
  let status = {
    let _cargo_child_execution_phase = crate::instrumentation::cargo_child_execution_phase();
    Command::new(cargo)
      .args(arguments)
      .current_dir(workspace_root)
      .status()
      .map_err(|spawn_error| RailError::message(format!("build failed: {spawn_error}")))?
  };
  if !status.success() {
    return Err(RailError::ExitWithCode {
      code: status.code().unwrap_or(1),
    });
  }
  Ok(true)
}

fn configured_codegen_backend_present(cargo_current_dir: &std::path::Path) -> bool {
  fn contains_backend(value: &serde_json::Value) -> bool {
    match value {
      serde_json::Value::String(value) => value.contains("codegen-backend"),
      serde_json::Value::Array(values) => values.iter().any(contains_backend),
      serde_json::Value::Object(values) => values.values().any(contains_backend),
      serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => false,
    }
  }

  crate::cargo::CargoConfigSnapshot::capture(cargo_current_dir).is_ok_and(|config| {
    config
      .environment()
      .iter()
      .any(|(name, value)| name.ends_with("RUSTFLAGS") && value.contains("codegen-backend"))
      || contains_backend(config.effective_file_settings())
  })
}

fn action_reasons(
  ctx: &WorkspaceContext,
  opts: &RunOptions,
  plan: Option<&PlanOutput>,
  action_id: &str,
  kind: Option<ActionKind>,
) -> RailResult<Vec<ActionReason>> {
  if opts.all {
    return Ok(vec![ActionReason::All]);
  }
  let output = plan.ok_or_else(|| RailError::message(format!("action '{action_id}' has no planner contract")))?;
  let surfaces = if let Some(kind) = kind {
    kind.planner_surface().into_iter().collect::<Vec<_>>()
  } else {
    ctx
      .config()
      .and_then(|config| config.run.actions.get(action_id))
      .ok_or_else(|| RailError::message(format!("configured action '{action_id}' is missing")))?
      .when
      .iter()
      .map(String::as_str)
      .collect()
  };
  let mut reasons = surfaces
    .iter()
    .copied()
    .filter_map(|surface| output.surfaces.get(surface).map(|decision| (surface, decision)))
    .filter(|(_, decision)| decision.enabled)
    .flat_map(|(surface, decision)| {
      decision.reasons.iter().map(move |trace_id| ActionReason::Planner {
        surface: surface.to_string(),
        trace_id: *trace_id,
      })
    })
    .collect::<Vec<_>>();
  if reasons.is_empty() && kind.is_some() && output.has_semantic_seeds() {
    for surface in surfaces
      .into_iter()
      .filter(|surface| matches!(*surface, "build" | "test" | "bench"))
    {
      reasons.extend(
        output
          .semantic_seed_reason_ids()
          .into_iter()
          .map(|trace_id| ActionReason::Planner {
            surface: surface.to_string(),
            trace_id,
          }),
      );
    }
  }
  if reasons.is_empty() {
    return Err(RailError::message(format!(
      "planner enabled '{}' action without a reason",
      action_id
    )));
  }
  Ok(reasons)
}

fn execute_run_step(
  step: &RunStep,
  ordered_actions: &[ExpandedAction],
  opts: &RunOptions,
  ctx: &WorkspaceContext,
  completed_generated_outputs: &mut Vec<PathBuf>,
) -> RailResult<()> {
  let test_runner_name = match step {
    RunStep::Skipped(action) if opts.explain || opts.dry_run => {
      println!("skip action `{}` (not enabled by plan)", action);
      return Ok(());
    }
    RunStep::Skipped(_) => return Ok(()),
    RunStep::NoTargets(kind) => {
      println!("no {} targets", kind.as_str());
      return Ok(());
    }
    RunStep::Action {
      action_index,
      test_runner_name,
    } => (action_index, test_runner_name),
  };
  let (action_index, test_runner_name) = test_runner_name;
  let expanded = ordered_actions
    .get(*action_index)
    .ok_or_else(|| RailError::message("action schedule references a missing expanded action"))?;

  if let Some(runner_name) = test_runner_name {
    let targets = expanded.selected_packages();
    progress!("testing {} crates ({})", targets.len(), runner_name);
    if targets.len() <= 12 {
      for target in targets {
        progress!("  {}", target);
      }
    } else {
      progress!("targets: {}", format_preview_list(targets, 12));
    }
  }

  if opts.explain {
    if let Some(action_key) = expanded.action_key() {
      println!(
        "action `{}` key: {} ({})",
        expanded.id(),
        if action_key.reason_codes().next().is_some() {
          "uncacheable"
        } else {
          "eligible"
        },
        action_key.reason_codes().collect::<Vec<_>>().join(", ")
      );
    }
    match expanded.kind() {
      ActionKind::Build
      | ActionKind::Bench
      | ActionKind::Format
      | ActionKind::Lint
      | ActionKind::Msrv
      | ActionKind::Package
      | ActionKind::Distribution => println!(
        "action `{}` targets ({}): {}",
        expanded.id(),
        expanded.selected_packages().len(),
        format_preview_list(expanded.selected_packages(), 12)
      ),
      ActionKind::Docs => println!("action `docs` targets: workspace"),
      ActionKind::GeneratedArtifact => println!(
        "action `{}` owns: {}",
        expanded.id(),
        format_preview_list(&expanded.repository_outputs().collect::<Vec<_>>(), 12)
      ),
      ActionKind::Test | ActionKind::Audit | ActionKind::Repository => {}
    }
  }

  run_or_print_action(opts, ctx, expanded, completed_generated_outputs)?;
  if !opts.dry_run && opts.generated == GeneratedMode::Regenerate && expanded.is_generated() {
    completed_generated_outputs.extend(
      expanded
        .repository_outputs()
        .map(|output| ctx.workspace_root().join(output)),
    );
  }
  Ok(())
}

fn run_or_print_action(
  opts: &RunOptions,
  ctx: &WorkspaceContext,
  action: &ExpandedAction,
  completed_generated_outputs: &[PathBuf],
) -> RailResult<()> {
  if opts.print_cmd || opts.dry_run {
    println!("{}: {}", action.id(), action.argv().join(" "));
  }
  if opts.dry_run {
    return Ok(());
  }

  if opts.hermetic {
    let lookup_key = opts
      .pre_context_cache_request
      .then(|| crate::hermetic::pre_context_lookup_key(ctx.workspace_root()))
      .and_then(Result::ok);
    let cache_disabled_reason = if opts.no_cache {
      Some("disabled_by_request")
    } else if !ctx.cache_enabled() {
      Some("disabled_by_configuration")
    } else {
      None
    };
    let (report, path) = crate::hermetic::execute(ctx, action, cache_disabled_reason, lookup_key.as_deref())?;
    print_hermetic_result(action.id(), &report, &path, opts.explain);
    return Ok(());
  }

  let (program, arguments) = action
    .argv()
    .split_first()
    .ok_or_else(|| RailError::message("expanded action argv cannot be empty"))?;
  ctx.validate_snapshot_unchanged_excluding(completed_generated_outputs)?;
  let native_cache = if matches!(action.kind(), ActionKind::Build | ActionKind::Distribution) {
    let _native_cache_setup_phase = crate::instrumentation::native_cache_setup_phase();
    Some(if opts.no_cache {
      crate::compiler::native_cache::DirectNativeCacheSetup::Bypassed(DirectCacheBypass::DisabledByRequest)
    } else if !ctx.cache_enabled() {
      crate::compiler::native_cache::DirectNativeCacheSetup::Bypassed(DirectCacheBypass::DisabledByConfiguration)
    } else if cargo_cli_configuration_override(arguments) {
      crate::compiler::native_cache::DirectNativeCacheSetup::Bypassed(DirectCacheBypass::CargoCliConfiguration)
    } else if action_compiler_wrapper_capability(action) {
      crate::compiler::native_cache::DirectNativeCacheSetup::Bypassed(DirectCacheBypass::ActionCompilerWrapper)
    } else if !action.environment().inherit() || !action.environment().entries().is_empty() {
      crate::compiler::native_cache::DirectNativeCacheSetup::Bypassed(DirectCacheBypass::ActionEnvironment)
    } else if let Some(reason) = native_cache_policy_bypass_reason(
      action.kind(),
      arguments,
      ctx.cargo().metadata().target_directory.as_std_path(),
      ctx.execution_workspace_root(),
      std::env::var_os("CARGO_INCREMENTAL").as_deref(),
      std::env::var_os("RUSTC_FORCE_INCREMENTAL").is_some(),
    ) {
      crate::compiler::native_cache::DirectNativeCacheSetup::Bypassed(reason)
    } else {
      match prepare_direct_cargo_action(ctx.snapshot()?, ctx.execution_workspace_root(), opts.explain) {
        Ok(setup) => setup,
        Err(_) => {
          crate::compiler::native_cache::DirectNativeCacheSetup::Bypassed(DirectCacheBypass::IdentityUnavailable)
        }
      }
    })
  } else {
    None
  };
  if let Some(message) = native_cache
    .as_ref()
    .and_then(crate::compiler::native_cache::DirectNativeCacheSetup::operational_failure)
  {
    return Err(RailError::message(message.to_string()));
  }
  let selected_program =
    if program == "cargo" && !matches!(action.kind(), ActionKind::GeneratedArtifact | ActionKind::Repository) {
      ctx.snapshot()?.toolchain().cargo_program()
    } else {
      std::ffi::OsStr::new(program)
    };
  let mut command = Command::new(selected_program);
  if native_cache
    .as_ref()
    .is_some_and(crate::compiler::native_cache::DirectNativeCacheSetup::remote_active)
  {
    crate::remote_cache::scrub_child_environment(&mut command);
  }
  let validated_working_directory = action.validate_paths(ctx.workspace_root())?;
  let working_directory = if action.working_directory() == &ActionWorkingDirectory::Workspace {
    let execution_workspace_root = ctx.execution_workspace_root();
    let current = crate::utils::canonicalize_existing(execution_workspace_root).map_err(|error| {
      RailError::message(format!(
        "workspace execution root '{}' cannot be resolved: {error}",
        execution_workspace_root.display()
      ))
    })?;
    if current != validated_working_directory {
      return Err(RailError::message(format!(
        "workspace execution root '{}' resolved to '{}', expected '{}'",
        execution_workspace_root.display(),
        current.display(),
        validated_working_directory.display()
      )));
    }
    execution_workspace_root.to_path_buf()
  } else {
    validated_working_directory
  };
  if let Some(configuration) = native_cache
    .as_ref()
    .and_then(crate::compiler::native_cache::DirectNativeCacheSetup::cargo_config_argument)
  {
    command.arg("--config").arg(configuration);
    // A clean profile has no incremental state to preserve. Disable Cargo's default
    // incremental mode so every eligible unit can participate in the native cache.
    // Explicit requests and active profiles are rejected by the policy above.
    if std::env::var_os("CARGO_INCREMENTAL").is_none() {
      command.env("CARGO_INCREMENTAL", "0");
    }
  }
  command.args(arguments).current_dir(&working_directory);
  if !action.environment().inherit() {
    command.env_clear();
  }
  for entry in action.environment().entries() {
    match entry {
      ActionEnvironmentEntry::Fixed { name, value } => {
        command.env(name, value);
      }
      ActionEnvironmentEntry::Pass { name } => {
        if let Some(value) = std::env::var_os(name) {
          command.env(name, value);
        }
      }
      ActionEnvironmentEntry::Secret { name } => {
        let value = std::env::var_os(name).ok_or_else(|| {
          RailError::message(format!(
            "action '{}' secret environment capability '{name}' disappeared before execution",
            action.id()
          ))
        })?;
        command.env(name, value);
      }
      ActionEnvironmentEntry::Cargo { name, value } => {
        let value = match value {
          crate::config::CargoEnvironmentValue::WorkspaceRoot => ctx.workspace_root().as_os_str(),
          crate::config::CargoEnvironmentValue::TargetDirectory => {
            ctx.cargo().metadata().target_directory.as_std_path().as_os_str()
          }
        };
        command.env(name, value);
      }
    }
  }
  if native_cache
    .as_ref()
    .is_some_and(|native_cache| native_cache.bypass_reason().is_some())
  {
    crate::compiler::native_cache::remove_private_environment(&mut command);
  }
  ctx.validate_snapshot_unchanged_excluding(completed_generated_outputs)?;
  let status = {
    let _cargo_child_execution_phase = crate::instrumentation::cargo_child_execution_phase();
    command
      .status()
      .map_err(|error| RailError::message(format!("{} failed: {}", action.id(), error)))?
  };
  let native_cache_report = native_cache.as_ref().map_or(Ok(()), |native_cache| {
    report_native_cache_decision(action.id(), native_cache, opts.explain)
  });
  if !status.success() {
    return Err(RailError::ExitWithCode {
      code: status.code().unwrap_or(1),
    });
  }
  native_cache_report?;
  Ok(())
}

fn report_native_cache_decision(
  action_id: &str,
  native_cache: &crate::compiler::native_cache::DirectNativeCacheSetup,
  explain: bool,
) -> RailResult<()> {
  if let Some(reason) = native_cache.bypass_reason() {
    if explain {
      println!("action `{action_id}` native compiler cache: bypassed ({reason})");
    } else {
      progress!("action `{action_id}` native compiler cache: bypassed reason={reason}");
    }
  } else if let Some(report) = {
    let _cache_report_collection_phase = crate::instrumentation::cache_report_collection_phase();
    native_cache.report()
  } {
    let operational_result = validate_native_cache_report(&report);
    if let Some(diagnostics) = report.wrapper_diagnostics.clone() {
      crate::instrumentation::record_native_cache_wrapper_diagnostics(diagnostics);
    }
    if explain {
      let reasons = report
        .reasons
        .iter()
        .map(|(reason, count)| format!("{reason}={count}"))
        .collect::<Vec<_>>()
        .join(",");
      println!(
        "action `{action_id}` native compiler cache: hits={} misses={} bypasses={} setup_bytes_hashed={} bytes_hashed={} cache_bytes_read={} cache_bytes_written={} bytes_restored={} reasons={}",
        report.hits,
        report.misses,
        report.bypasses,
        report.setup_bytes_hashed,
        report.bytes_hashed,
        report.cache_bytes_read,
        report.cache_bytes_written,
        report.bytes_restored,
        reasons,
      );
      if let Some(remote) = report.remote {
        println!(
          "action `{action_id}` remote compiler cache: requests={} bytes={} hits={} misses={} conflicts={} failures={} publications={}",
          remote.requests,
          remote.bytes,
          remote.hits,
          remote.misses,
          remote.conflicts,
          remote.failures,
          remote.publications,
        );
      }
      for event in &report.events {
        if let Ok(encoded) = serde_json::to_string(event) {
          println!("action `{action_id}` native compiler cache event: {encoded}");
        }
      }
    } else {
      progress!(
        "action `{action_id}` native compiler cache: hits={} misses={} bypasses={} bytes_restored={}",
        report.hits,
        report.misses,
        report.bypasses,
        report.bytes_restored,
      );
      if let Some(remote) = report.remote {
        progress!(
          "action `{action_id}` remote compiler cache: requests={} hits={} misses={} conflicts={} failures={} publications={}",
          remote.requests,
          remote.hits,
          remote.misses,
          remote.conflicts,
          remote.failures,
          remote.publications,
        );
      }
    }
    operational_result?;
  }
  Ok(())
}

fn validate_native_cache_report(report: &crate::compiler::native_cache::DirectNativeCacheReport) -> RailResult<()> {
  if report.environment_selector_diverged {
    return Err(RailError::message("native compiler environment selector diverged"));
  }
  Ok(())
}

fn cargo_cli_configuration_override(arguments: &[String]) -> bool {
  arguments
    .iter()
    .take_while(|argument| argument.as_str() != "--")
    .any(|argument| {
      ["--build-dir", "--config"].iter().any(|option| {
        argument == option
          || argument
            .strip_prefix(option)
            .is_some_and(|suffix| suffix.starts_with('='))
      })
    })
}

fn native_cache_policy_bypass_reason(
  action: ActionKind,
  arguments: &[String],
  target_directory: &std::path::Path,
  execution_workspace_root: &std::path::Path,
  cargo_incremental: Option<&std::ffi::OsStr>,
  rustc_force_incremental: bool,
) -> Option<DirectCacheBypass> {
  if rustc_force_incremental {
    return Some(DirectCacheBypass::ForcedIncremental);
  }
  let explicit_non_incremental = match cargo_incremental {
    Some(value) if value == "0" => true,
    Some(_) => return Some(DirectCacheBypass::ExplicitIncremental),
    None => false,
  };
  let target = cargo_option_value(arguments, "--target");
  let target_directory = cargo_option_value(arguments, "--target-dir").map_or_else(
    || target_directory.to_path_buf(),
    |directory| {
      let directory = std::path::Path::new(directory);
      if directory.is_absolute() {
        directory.to_path_buf()
      } else {
        execution_workspace_root.join(directory)
      }
    },
  );
  let target_directory = match crate::utils::canonicalize_allow_missing(&target_directory) {
    Ok(directory) => directory,
    Err(_) => return Some(DirectCacheBypass::TargetDirectoryOutsideSourceRoot),
  };
  let execution_workspace_root = match crate::utils::canonicalize_existing(execution_workspace_root) {
    Ok(root) => root,
    Err(_) => return Some(DirectCacheBypass::SourceRootUnavailable),
  };
  if !target_directory.starts_with(&execution_workspace_root) {
    return Some(DirectCacheBypass::TargetDirectoryOutsideSourceRoot);
  }
  if explicit_non_incremental {
    return None;
  }

  let profile = match cargo_option_value(arguments, "--profile") {
    Some("dev") => "debug",
    Some("release") => "release",
    Some(_) => return Some(DirectCacheBypass::CustomCargoProfile),
    None if action == ActionKind::Distribution || cargo_flag_selected(arguments, "--release") => "release",
    None => "debug",
  };
  let profile_root = target.map_or_else(
    || target_directory.clone(),
    |target| {
      let target = std::path::Path::new(target)
        .file_stem()
        .unwrap_or_else(|| std::ffi::OsStr::new(target));
      target_directory.join(target)
    },
  );
  profile_root
    .join(profile)
    .join(".fingerprint")
    .is_dir()
    .then_some(DirectCacheBypass::ActiveCargoProfile)
}

fn cargo_option_value<'a>(arguments: &'a [String], option: &str) -> Option<&'a str> {
  let mut arguments = arguments.iter().map(String::as_str);
  while let Some(argument) = arguments.next() {
    if argument == "--" {
      break;
    }
    if argument == option {
      return arguments.next();
    }
    if let Some(value) = argument
      .strip_prefix(option)
      .and_then(|suffix| suffix.strip_prefix('='))
    {
      return Some(value);
    }
  }
  None
}

fn cargo_flag_selected(arguments: &[String], flag: &str) -> bool {
  arguments
    .iter()
    .map(String::as_str)
    .take_while(|argument| *argument != "--")
    .any(|argument| argument == flag)
}

fn action_compiler_wrapper_capability(action: &ExpandedAction) -> bool {
  action.environment().entries().iter().any(|entry| {
    let name = match entry {
      ActionEnvironmentEntry::Fixed { name, .. }
      | ActionEnvironmentEntry::Pass { name }
      | ActionEnvironmentEntry::Secret { name }
      | ActionEnvironmentEntry::Cargo { name, .. } => name,
    };
    matches!(
      name.as_str(),
      "RUSTC_WRAPPER" | "RUSTC_WORKSPACE_WRAPPER" | "CARGO_BUILD_RUSTC_WRAPPER" | "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER"
    )
  })
}

fn print_hermetic_result(
  action_id: &str,
  report: &crate::hermetic::HermeticExecutionReport,
  path: &std::path::Path,
  explain: bool,
) {
  println!(
    "{action_id}: hermetic {}{}{}",
    report.support().as_str(),
    report
      .action_key()
      .map(|key| format!(" action_key={key}"))
      .unwrap_or_default(),
    report
      .result_digest()
      .map(|digest| format!(" result={digest}"))
      .unwrap_or_default(),
  );
  println!("{action_id}: hermetic report {}", path.display());
  if explain {
    println!(
      "action `{action_id}` local cache: {} ({}) cargo_check_executed={} compiler_units_executed={}",
      report.cache_status(),
      report.cache_reason(),
      report.cargo_check_executed(),
      report.compiler_units_executed(),
    );
  }
}

/// Finish the ordinary text and receipt surfaces after a process-free cache hit.
pub(crate) fn complete_pre_context_cache_hit(
  workspace_root: &std::path::Path,
  hit: crate::hermetic::PreContextCacheHit,
  print_cmd: bool,
  explain: bool,
) -> RailResult<()> {
  if explain {
    println!("action `build` key: eligible ()");
    println!(
      "action `build` targets ({}): {}",
      hit.selected_packages.len(),
      format_preview_list(&hit.selected_packages, 12)
    );
  }
  if print_cmd {
    println!("build: {}", hit.argv.join(" "));
  }
  print_hermetic_result("build", &hit.report, &hit.report_path, explain);
  let cached_action = serde_json::json!({
    "id": "build",
    "kind": "build",
    "argv": hit.argv,
    "selected_packages": hit.selected_packages,
    "action_key": hit.report.action_key(),
    "source": "verified_local_action_result",
  });
  let receipt = serde_json::json!({
    "artifact": "decision_receipt",
    "version": 4,
    "command": "run",
    "generated_at_utc": chrono::Utc::now().to_rfc3339(),
    "snapshot_id": null,
    "cache_action_key": hit.report.action_key(),
    "snapshot_status": "not_loaded_on_process_free_cache_hit",
    "actions": [cached_action],
    "inputs": {
      "profile_requested": null,
      "profile_effective": null,
      "profile_source": null,
      "workflow_requested": null,
      "workflow_effective": null,
      "actions_requested": ["build"],
      "actions_effective": ["build"],
      "since_requested": null,
      "since_effective": null,
      "merge_base_requested": false,
      "merge_base_effective": false,
      "all": true,
      "run_args_requested": [],
      "run_args_effective": [],
      "test_runner": TestRunnerPreference::Auto,
      "cargo_test_args": [],
      "nextest_args": [],
      "test_filter": null,
      "dry_run": false,
      "format": ActionOutputFormat::Text,
      "generated_mode": GeneratedMode::Regenerate,
      "execution_profile": "hermetic",
    },
    "execution": {
      "executed_actions": ["build"],
      "skipped_actions": [],
      "execution_mode": "verified_local_cache_restore",
      "cargo_check_executed": false,
      "compiler_units_executed": false,
      "fetch_action": null,
    },
    "scope": null,
    "plan": null,
  });
  let receipt_path = persist_run_decision_receipt(workspace_root, &receipt)?;
  if std::env::var_os("CI").is_some() {
    progress!("decision receipt: {}", receipt_path.display());
  }
  Ok(())
}

struct DecisionReceiptInput<'a> {
  ctx: &'a WorkspaceContext,
  opts: &'a RunOptions,
  effective: &'a EffectiveRunInputs,
  plan: Option<&'a PlanOutput>,
  executed_actions: &'a [String],
  skipped_actions: &'a [String],
  graph: &'a ActionGraph,
}

fn write_run_decision_receipt(input: DecisionReceiptInput<'_>) -> RailResult<std::path::PathBuf> {
  let receipt = serde_json::json!({
    "artifact": "decision_receipt",
    "version": 4,
    "command": "run",
    "generated_at_utc": chrono::Utc::now().to_rfc3339(),
    "snapshot_id": input.graph.snapshot_id(),
    "actions": input.graph.actions(),
    "inputs": {
      "profile_requested": input.opts.profile,
      "profile_effective": input.effective.profile,
      "profile_source": input.effective.profile_source,
      "workflow_requested": input.opts.workflow,
      "workflow_effective": input.effective.workflow,
      "actions_requested": input.opts.actions,
      "actions_effective": input.effective.actions,
      "since_requested": input.opts.since,
      "since_effective": input.effective.since,
      "merge_base_requested": input.opts.merge_base,
      "merge_base_effective": input.effective.merge_base,
      "all": input.opts.all,
      "run_args_requested": input.opts.run_args,
      "run_args_effective": input.effective.run_args,
      "test_runner": input.opts.test_runner,
      "cargo_test_args": input.opts.cargo_test_args,
      "nextest_args": input.opts.nextest_args,
      "test_filter": input.opts.test_filter,
      "dry_run": input.opts.dry_run,
      "format": input.opts.format,
      "generated_mode": input.opts.generated,
      "execution_profile": if input.opts.hermetic { "hermetic" } else { "normal" },
    },
    "execution": {
      "executed_actions": input.executed_actions,
      "skipped_actions": input.skipped_actions,
      "fetch_action": input.opts.hermetic.then(|| serde_json::json!({
        "id": "fetch",
        "argv": ["cargo", "fetch", "--locked"],
        "network": "allowed",
        "produces": "immutable_cargo_source_inventory",
        "consumer_network": "denied",
      })),
    },
    "scope": input.plan.map(|output| &output.scope),
    "plan": input.plan,
  });
  persist_run_decision_receipt(input.ctx.workspace_root(), &receipt)
}

fn persist_run_decision_receipt(
  workspace_root: &std::path::Path,
  receipt: &serde_json::Value,
) -> RailResult<std::path::PathBuf> {
  let dir = crate::workspace::cargo_rail_state_root(workspace_root).join("receipts");
  fs::create_dir_all(&dir)?;
  let nonce = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
  let path = dir.join(format!("run-decision-{}.json", nonce));
  let bytes = serde_json::to_vec_pretty(&receipt)
    .map_err(|e| RailError::message(format!("failed to serialize decision receipt: {}", e)))?;
  let mut file = OpenOptions::new()
    .write(true)
    .create_new(true)
    .open(&path)
    .map_err(|e| RailError::message(format!("failed to create decision receipt '{}': {}", path.display(), e)))?;
  file
    .write_all(&bytes)
    .map_err(|e| RailError::message(format!("failed to write decision receipt '{}': {}", path.display(), e)))?;
  // Decision receipts are observational evidence, not recovery authority. A synchronous
  // flush on every successful no-op adds latency without providing directory durability.
  Ok(path)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
  }

  #[test]
  fn active_incremental_delegation_requires_a_workspace_manifest() {
    let root = tempfile::tempdir().expect("workspace directory");
    let manifest = root.path().join("Cargo.toml");
    fs::write(&manifest, "[package]\nname = \"member\"\nversion = \"0.1.0\"\n").expect("member manifest");
    assert!(!manifest_declares_workspace(root.path()));

    fs::write(&manifest, "[workspace]\nmembers = []\n").expect("workspace manifest");
    assert!(manifest_declares_workspace(root.path()));
  }

  #[test]
  fn active_profile_delegation_rejects_invalid_rail_configuration() {
    let root = tempfile::tempdir().expect("workspace directory");
    fs::create_dir(root.path().join(".config")).expect("configuration directory");
    let configuration = root.path().join(".config/rail.toml");
    fs::write(&configuration, "[run\n").expect("invalid configuration");
    assert!(CapturedDiscoveredConfig::capture(root.path()).is_err());

    fs::write(&configuration, "# Empty configuration.\n").expect("valid configuration");
    let captured = CapturedDiscoveredConfig::capture(root.path()).expect("captured configuration");
    assert!(rail_configuration_allows_builtin_delegation(&captured, root.path()));
  }

  #[test]
  fn native_cache_policy_preserves_active_incremental_profiles() {
    let target = tempfile::tempdir().expect("target directory");
    fs::create_dir_all(target.path().join("debug/.fingerprint")).expect("debug fingerprints");

    assert_eq!(
      native_cache_policy_bypass_reason(
        ActionKind::Build,
        &args(&["check"]),
        target.path(),
        target.path(),
        None,
        false
      ),
      Some(DirectCacheBypass::ActiveCargoProfile)
    );
    assert_eq!(
      native_cache_policy_bypass_reason(
        ActionKind::Build,
        &args(&["check"]),
        target.path(),
        target.path(),
        Some(std::ffi::OsStr::new("0")),
        false,
      ),
      None,
      "an explicit non-incremental request remains eligible"
    );
  }

  #[test]
  fn native_cache_policy_enables_clean_profiles_and_preserves_explicit_incremental_requests() {
    let target = tempfile::tempdir().expect("target directory");
    assert_eq!(
      native_cache_policy_bypass_reason(
        ActionKind::Build,
        &args(&["check"]),
        target.path(),
        target.path(),
        None,
        false
      ),
      None
    );
    assert_eq!(
      native_cache_policy_bypass_reason(
        ActionKind::Build,
        &args(&["check"]),
        target.path(),
        target.path(),
        Some(std::ffi::OsStr::new("1")),
        false,
      ),
      Some(DirectCacheBypass::ExplicitIncremental)
    );
    assert_eq!(
      native_cache_policy_bypass_reason(
        ActionKind::Build,
        &args(&["check"]),
        target.path(),
        target.path(),
        None,
        true
      ),
      Some(DirectCacheBypass::ForcedIncremental)
    );
  }

  #[test]
  fn native_cache_policy_selects_the_exact_release_and_target_profile() {
    let target = tempfile::tempdir().expect("target directory");
    fs::create_dir_all(target.path().join("aarch64-unknown-linux-gnu/release/.fingerprint"))
      .expect("target fingerprints");

    assert_eq!(
      native_cache_policy_bypass_reason(
        ActionKind::Distribution,
        &args(&["build", "--target", "aarch64-unknown-linux-gnu"]),
        target.path(),
        target.path(),
        None,
        false,
      ),
      Some(DirectCacheBypass::ActiveCargoProfile)
    );
    assert_eq!(
      native_cache_policy_bypass_reason(
        ActionKind::Build,
        &args(&["check", "--target", "aarch64-unknown-linux-gnu"]),
        target.path(),
        target.path(),
        None,
        false,
      ),
      None,
      "a clean debug profile remains eligible even when release outputs exist"
    );
    assert_eq!(
      native_cache_policy_bypass_reason(
        ActionKind::Build,
        &args(&["check", "--profile", "custom"]),
        target.path(),
        target.path(),
        None,
        false,
      ),
      Some(DirectCacheBypass::CustomCargoProfile)
    );
    fs::create_dir_all(target.path().join("debug/.fingerprint")).expect("debug fingerprints");
    assert_eq!(
      native_cache_policy_bypass_reason(
        ActionKind::Build,
        &args(&["check", "--profile=dev"]),
        target.path(),
        target.path(),
        None,
        false,
      ),
      Some(DirectCacheBypass::ActiveCargoProfile)
    );
  }

  #[test]
  fn direct_cache_statically_rejects_unmodeled_cargo_configuration() {
    assert!(cargo_cli_configuration_override(&args(&[
      "check",
      "--config",
      "net.offline=true"
    ])));
    assert!(!cargo_cli_configuration_override(&args(&[
      "check",
      "--target-dir=elsewhere"
    ])));
    assert!(cargo_cli_configuration_override(&args(&[
      "check",
      "--build-dir",
      "elsewhere"
    ])));
    assert!(!cargo_cli_configuration_override(&args(&[
      "check",
      "--target",
      "wasm32-wasip1"
    ])));
    assert!(!cargo_cli_configuration_override(&args(&[
      "test",
      "--",
      "--config=ordinary-harness-value"
    ])));
  }

  #[test]
  fn native_cache_policy_inspects_the_selected_target_directory() {
    let workspace = tempfile::tempdir().expect("workspace");
    let default_target = workspace.path().join("target");
    fs::create_dir_all(workspace.path().join("elsewhere/debug/.fingerprint")).expect("selected fingerprints");

    assert_eq!(
      native_cache_policy_bypass_reason(
        ActionKind::Build,
        &args(&["check", "--target-dir=elsewhere"]),
        &default_target,
        workspace.path(),
        None,
        false,
      ),
      Some(DirectCacheBypass::ActiveCargoProfile)
    );
  }

  #[test]
  fn native_cache_policy_bypasses_target_directories_outside_the_source_root() {
    let workspace = tempfile::tempdir().expect("workspace");
    let external = tempfile::tempdir().expect("external target parent");
    let default_target = workspace.path().join("target");
    let external_target = external.path().join("missing-target");
    let external_target = external_target.to_string_lossy();

    assert_eq!(
      native_cache_policy_bypass_reason(
        ActionKind::Build,
        &args(&["check", "--target-dir", &external_target]),
        &default_target,
        workspace.path(),
        Some(std::ffi::OsStr::new("0")),
        false,
      ),
      Some(DirectCacheBypass::TargetDirectoryOutsideSourceRoot)
    );
    assert_eq!(
      native_cache_policy_bypass_reason(
        ActionKind::Build,
        &args(&["check", "--target-dir", "missing-target"]),
        &default_target,
        workspace.path(),
        Some(std::ffi::OsStr::new("0")),
        false,
      ),
      None,
      "a missing target directory within the source root remains eligible"
    );
  }

  #[cfg(unix)]
  #[test]
  fn native_cache_policy_resolves_target_directory_symlink_escapes() {
    use std::os::unix::fs::symlink;

    let workspace = tempfile::tempdir().expect("workspace");
    let external = tempfile::tempdir().expect("external target parent");
    let default_target = workspace.path().join("target");
    symlink(external.path(), workspace.path().join("escaped-target")).expect("external target symlink");

    assert_eq!(
      native_cache_policy_bypass_reason(
        ActionKind::Build,
        &args(&["check", "--target-dir", "escaped-target/missing"]),
        &default_target,
        workspace.path(),
        Some(std::ffi::OsStr::new("0")),
        false,
      ),
      Some(DirectCacheBypass::TargetDirectoryOutsideSourceRoot)
    );
  }

  #[test]
  fn native_cache_report_rejects_only_selector_divergence() {
    let mut report = crate::compiler::native_cache::DirectNativeCacheReport::default();
    report.reasons.insert("local_cache_store_failed".to_string(), 1);
    assert!(validate_native_cache_report(&report).is_ok());

    report.environment_selector_diverged = true;
    assert_eq!(
      validate_native_cache_report(&report)
        .expect_err("selector divergence must fail the command")
        .to_string(),
      "native compiler environment selector diverged"
    );
  }
}
