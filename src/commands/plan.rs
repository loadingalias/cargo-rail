//! `cargo rail plan` - deterministic file-first change planner.

use crate::commands::common::{PlanOutputFormat, format_preview_list};
use crate::config::ConfidenceProfile;
use crate::error::{RailError, RailResult};
use crate::git::detect_default_base_ref;
use crate::utils::{config_fingerprint, toolchain_fingerprint};
use crate::workspace::WorkspaceContext;
use glob::Pattern;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Options for the `plan` command.
#[derive(Debug, Default)]
pub struct PlanOptions {
  /// Git ref to compare against.
  pub since: Option<String>,
  /// Start of SHA range (used with `to` for SHA pair mode).
  pub from: Option<String>,
  /// End of SHA range (used with `from` for SHA pair mode).
  pub to: Option<String>,
  /// Use merge-base with default branch.
  pub merge_base: bool,
  /// Output format.
  pub format: PlanOutputFormat,
  /// Write output to file instead of stdout.
  pub output: Option<PathBuf>,
  /// Show concise human reasoning.
  pub explain: bool,
  /// Planner confidence profile override.
  pub confidence_profile: Option<String>,
}

#[derive(Debug, Clone)]
struct ResolvedRefs {
  since: Option<String>,
  from: Option<String>,
  to: Option<String>,
  merge_base: bool,
  resolved_base: String,
  resolved_head: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct PlanOutput {
  pub(crate) plan_contract_version: u32,
  pub(crate) inputs: PlanInputs,
  pub(crate) files: Vec<PlannedFile>,
  pub(crate) impact: PlanImpact,
  pub(crate) scope: ExecutionScope,
  pub(crate) surfaces: BTreeMap<String, SurfaceDecision>,
  pub(crate) trace: Vec<TraceReason>,
  pub(crate) reproducibility: Reproducibility,
}

#[derive(Debug, Serialize)]
pub(crate) struct PlanInputs {
  pub(crate) refs: PlanRefs,
  pub(crate) workspace_root: String,
  pub(crate) config_fingerprint: String,
  pub(crate) toolchain_fingerprint: String,
  pub(crate) confidence_profile: String,
  pub(crate) confidence_profile_source: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct PlanRefs {
  pub(crate) since: Option<String>,
  pub(crate) from: Option<String>,
  pub(crate) to: Option<String>,
  pub(crate) merge_base: bool,
  pub(crate) resolved_base: String,
  pub(crate) resolved_head: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct PlannedFile {
  pub(crate) path: String,
  pub(crate) kind: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub(crate) sub_kind: Option<String>,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub(crate) custom_surfaces: Vec<String>,
  pub(crate) owners: Vec<String>,
  pub(crate) owner_scope: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct PlanImpact {
  pub(crate) direct_crates: Vec<String>,
  pub(crate) transitive_crates: Vec<String>,
}

/// Execution-focused projection of the planner contract.
///
/// This is the stable handoff for runners and CI transport layers.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExecutionScope {
  pub(crate) scope_contract_version: u32,
  pub(crate) resolved_base: String,
  pub(crate) resolved_head: String,
  pub(crate) mode: ExecutionScopeMode,
  pub(crate) crates: Vec<String>,
  pub(crate) surfaces: BTreeMap<String, bool>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecutionScopeMode {
  Empty,
  Crates,
  Workspace,
}

#[derive(Debug, Serialize)]
pub(crate) struct SurfaceDecision {
  pub(crate) enabled: bool,
  pub(crate) reasons: Vec<u32>,
}

#[derive(Debug, Serialize)]
pub(crate) struct TraceReason {
  pub(crate) id: u32,
  pub(crate) code: &'static str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub(crate) file: Option<String>,
  #[serde(rename = "crate", skip_serializing_if = "Option::is_none")]
  pub(crate) crate_name: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub(crate) depends_on: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub(crate) surface: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Reproducibility {
  pub(crate) cargo_rail_version: &'static str,
  pub(crate) config_hash: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub(crate) git_merge_base: Option<String>,
  pub(crate) git_shallow_clone: bool,
}

#[derive(Debug)]
struct FileKind {
  kind: String,
  sub_kind: Option<String>,
  reason_code: &'static str,
}

const RC_FILE_KIND_RUST_SRC: &str = "FILE_KIND_RUST_SRC";
const RC_FILE_KIND_RUST_TEST: &str = "FILE_KIND_RUST_TEST";
const RC_FILE_KIND_RUST_BENCH: &str = "FILE_KIND_RUST_BENCH";
const RC_FILE_KIND_TOML_MANIFEST: &str = "FILE_KIND_TOML_MANIFEST";
const RC_FILE_KIND_TOML_WORKSPACE: &str = "FILE_KIND_TOML_WORKSPACE";
const RC_FILE_KIND_TOML_TOOLING: &str = "FILE_KIND_TOML_TOOLING";
const RC_FILE_KIND_CI: &str = "FILE_KIND_CI";
const RC_FILE_KIND_SCRIPT: &str = "FILE_KIND_SCRIPT";
const RC_FILE_KIND_DOCS: &str = "FILE_KIND_DOCS";
const RC_FILE_KIND_REPO_CONFIG: &str = "FILE_KIND_REPO_CONFIG";
const RC_FILE_KIND_CUSTOM: &str = "FILE_KIND_CUSTOM";
const RC_FILE_KIND_UNCLASSIFIED: &str = "FILE_KIND_UNCLASSIFIED";
const RC_FILE_OWNS_CRATE_DIRECT: &str = "FILE_OWNS_CRATE_DIRECT";
const RC_TRANSITIVE_DEPENDS_ON_DIRECT: &str = "TRANSITIVE_DEPENDS_ON_DIRECT";
const RC_OWNER_UNCERTAIN_FALLBACK: &str = "OWNER_UNCERTAIN_FALLBACK";
const RC_CONFIDENCE_PROFILE_STRICT: &str = "CONFIDENCE_PROFILE_STRICT";
const RC_CONFIDENCE_PROFILE_BALANCED: &str = "CONFIDENCE_PROFILE_BALANCED";
const RC_CONFIDENCE_PROFILE_FAST: &str = "CONFIDENCE_PROFILE_FAST";
const RC_CONFIDENCE_STRICT_OWNER_EXPANSION: &str = "CONFIDENCE_STRICT_OWNER_EXPANSION";
const RC_CONFIDENCE_FAST_SKIP_TRANSITIVE: &str = "CONFIDENCE_FAST_SKIP_TRANSITIVE";
const RC_BOT_PR_CONFIDENCE_OVERRIDE: &str = "BOT_PR_CONFIDENCE_OVERRIDE";
const PLAN_CONTRACT_VERSION: u32 = 3;
const SCOPE_CONTRACT_VERSION: u32 = 1;
const PACKAGE_SCOPED_SURFACES: &[&str] = &["build", "test", "bench"];

#[derive(Debug, Clone, Copy)]
struct EffectiveConfidenceProfile {
  profile: ConfidenceProfile,
  source: &'static str,
  bot_override: bool,
}

fn json_err(error: serde_json::Error) -> RailError {
  RailError::message(format!("JSON serialization failed: {}", error))
}

fn to_json<T: serde::Serialize>(value: &T) -> RailResult<String> {
  serde_json::to_string(value).map_err(json_err)
}

fn to_json_pretty<T: serde::Serialize>(value: &T) -> RailResult<String> {
  serde_json::to_string_pretty(value).map_err(json_err)
}

/// Run the plan command.
pub fn run_plan(ctx: &WorkspaceContext, opts: PlanOptions) -> RailResult<()> {
  if opts.format.is_json_like() {
    crate::output::set_json_mode(true);
  }

  let output = build_plan_output(ctx, &opts)?;

  let rendered = match opts.format {
    PlanOutputFormat::Text => format_text(&output, opts.explain),
    PlanOutputFormat::Json => {
      let payload = serde_json::to_value(&output).map_err(json_err)?;
      let envelope = crate::output::machine_json_envelope("plan", "inspect", "success", 0, payload);
      to_json_pretty(&envelope)?
    }
    PlanOutputFormat::GitHub => format_github(&output, false)?,
    PlanOutputFormat::GitHubDebug => format_github(&output, true)?,
  };

  write_output(&rendered, opts.output.as_ref())
}

/// Build the planner output contract without rendering it.
pub(crate) fn build_plan_output(ctx: &WorkspaceContext, opts: &PlanOptions) -> RailResult<PlanOutput> {
  let refs = resolve_refs(ctx, opts)?;
  let changed_files = collect_changed_files(ctx, &refs)?;
  let confidence = resolve_confidence_profile(ctx, opts)?;

  let custom_patterns = compile_custom_patterns(ctx);

  let changed_file_count = changed_files.len();
  let mut trace = Vec::with_capacity(changed_file_count * 4); // Multiple trace entries per file
  let mut surface_refs: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();

  let mut planned_files = Vec::with_capacity(changed_file_count);
  let mut direct_crates: BTreeSet<String> = BTreeSet::new();
  let mut build_test_seed_crates: BTreeSet<String> = BTreeSet::new();

  push_trace(
    &mut trace,
    &mut surface_refs,
    profile_reason_code(confidence.profile),
    None,
    None,
    None,
    &[] as &[String],
  );

  if confidence.bot_override {
    push_trace(
      &mut trace,
      &mut surface_refs,
      RC_BOT_PR_CONFIDENCE_OVERRIDE,
      None,
      None,
      None,
      &[] as &[String],
    );
  }

  for path in &changed_files {
    let file_kind = classify_file_kind(path);
    let custom_surfaces = custom_surfaces_for_path(path, &custom_patterns);
    let mut owners: Vec<String> = ctx.graph.files_to_crates(&[Path::new(path)]).into_iter().collect();
    owners.sort();

    for owner in &owners {
      direct_crates.insert(owner.clone());
      push_trace(
        &mut trace,
        &mut surface_refs,
        RC_FILE_OWNS_CRATE_DIRECT,
        Some(path),
        Some(owner),
        None,
        &[] as &[String],
      );
    }

    let owner_scope = owner_scope(path, &owners);

    let mut kind_surfaces = derive_surfaces_for_kind(&file_kind);

    let apply_owner_uncertain_fallback = file_kind.reason_code == RC_FILE_KIND_UNCLASSIFIED
      && !owners.is_empty()
      && confidence.profile != ConfidenceProfile::Fast
      && conservative_owner_fallback_enabled(ctx);

    let apply_strict_owner_expansion = confidence.profile == ConfidenceProfile::Strict && !owners.is_empty();

    if apply_owner_uncertain_fallback {
      ensure_surface(&mut kind_surfaces, "build");
      ensure_surface(&mut kind_surfaces, "test");

      let fallback_surfaces = vec!["build".to_string(), "test".to_string()];
      for owner in &owners {
        push_trace(
          &mut trace,
          &mut surface_refs,
          RC_OWNER_UNCERTAIN_FALLBACK,
          Some(path),
          Some(owner),
          None,
          &fallback_surfaces,
        );
      }
    }

    if apply_strict_owner_expansion {
      ensure_surface(&mut kind_surfaces, "build");
      ensure_surface(&mut kind_surfaces, "test");
      let strict_surfaces = vec!["build".to_string(), "test".to_string()];
      for owner in &owners {
        push_trace(
          &mut trace,
          &mut surface_refs,
          RC_CONFIDENCE_STRICT_OWNER_EXPANSION,
          Some(path),
          Some(owner),
          None,
          &strict_surfaces,
        );
      }
    }

    let baseline_transitive_seed = file_kind_seeds_build_test_transitive(&file_kind) || apply_owner_uncertain_fallback;
    let should_seed_build_test_transitive = match confidence.profile {
      ConfidenceProfile::Strict => !owners.is_empty() || baseline_transitive_seed,
      ConfidenceProfile::Balanced => baseline_transitive_seed,
      ConfidenceProfile::Fast => false,
    };

    if confidence.profile == ConfidenceProfile::Fast && baseline_transitive_seed && !owners.is_empty() {
      push_trace(
        &mut trace,
        &mut surface_refs,
        RC_CONFIDENCE_FAST_SKIP_TRANSITIVE,
        Some(path),
        None,
        None,
        &[] as &[String],
      );
    }

    if should_seed_build_test_transitive {
      build_test_seed_crates.extend(owners.iter().cloned());
    }

    push_trace(
      &mut trace,
      &mut surface_refs,
      file_kind.reason_code,
      Some(path),
      None,
      None,
      &kind_surfaces,
    );

    if !custom_surfaces.is_empty() {
      push_trace(
        &mut trace,
        &mut surface_refs,
        RC_FILE_KIND_CUSTOM,
        Some(path),
        None,
        None,
        &custom_surfaces,
      );
    }

    planned_files.push(PlannedFile {
      path: path.clone(),
      kind: file_kind.kind,
      sub_kind: file_kind.sub_kind,
      custom_surfaces,
      owners,
      owner_scope,
    });
  }

  let transitive_crates = compute_transitive_impact(ctx, &direct_crates)?;
  emit_transitive_build_test_trace(ctx, &build_test_seed_crates, &mut trace, &mut surface_refs)?;

  let surfaces = build_surfaces(&surface_refs, &custom_patterns);

  // Compute reproducibility metadata
  let git_merge_base = if refs.merge_base {
    Some(refs.resolved_base.clone())
  } else {
    None
  };

  let impact = PlanImpact {
    direct_crates: direct_crates.into_iter().collect(),
    transitive_crates,
  };
  let scope = build_execution_scope(
    &impact,
    &surfaces,
    ctx.cargo.metadata().workspace_packages().len(),
    &refs.resolved_base,
    &refs.resolved_head,
  );

  let output = PlanOutput {
    plan_contract_version: PLAN_CONTRACT_VERSION,
    inputs: PlanInputs {
      refs: PlanRefs {
        since: refs.since,
        from: refs.from,
        to: refs.to,
        merge_base: refs.merge_base,
        resolved_base: refs.resolved_base,
        resolved_head: refs.resolved_head,
      },
      workspace_root: ctx.workspace_root().display().to_string(),
      config_fingerprint: config_fingerprint(ctx.workspace_root()),
      toolchain_fingerprint: toolchain_fingerprint(ctx.workspace_root()),
      confidence_profile: confidence_profile_name(confidence.profile).to_string(),
      confidence_profile_source: confidence.source.to_string(),
    },
    files: planned_files,
    impact,
    scope,
    surfaces,
    trace,
    reproducibility: Reproducibility {
      cargo_rail_version: env!("CARGO_PKG_VERSION"),
      config_hash: config_fingerprint(ctx.workspace_root()),
      git_merge_base,
      git_shallow_clone: is_shallow_clone(ctx.workspace_root()),
    },
  };

  validate_surface_reason_invariants(&output)?;

  Ok(output)
}

fn conservative_owner_fallback_enabled(ctx: &WorkspaceContext) -> bool {
  ctx
    .config
    .as_ref()
    .map(|config| config.change_detection.conservative_unclassified_owner_fallback)
    .unwrap_or(true)
}

fn resolve_confidence_profile(ctx: &WorkspaceContext, opts: &PlanOptions) -> RailResult<EffectiveConfidenceProfile> {
  if let Some(raw) = opts.confidence_profile.as_deref() {
    let profile = parse_confidence_profile(raw)?;
    return Ok(EffectiveConfidenceProfile {
      profile,
      source: "cli",
      bot_override: false,
    });
  }

  let mut profile = ConfidenceProfile::default();
  let mut source = "default";
  let mut bot_override = false;

  if let Some(config) = &ctx.config {
    profile = config.change_detection.confidence_profile;
    source = "config";

    if let Some(bot_profile) = config.change_detection.bot_pr_confidence_profile
      && is_bot_authored_pull_request()
    {
      profile = bot_profile;
      source = "bot_pr_policy";
      bot_override = true;
    }
  }

  Ok(EffectiveConfidenceProfile {
    profile,
    source,
    bot_override,
  })
}

fn parse_confidence_profile(value: &str) -> RailResult<ConfidenceProfile> {
  match value {
    "strict" => Ok(ConfidenceProfile::Strict),
    "balanced" => Ok(ConfidenceProfile::Balanced),
    "fast" => Ok(ConfidenceProfile::Fast),
    _ => Err(RailError::with_help(
      format!("unknown confidence profile '{}'", value),
      "use --confidence-profile strict|balanced|fast",
    )),
  }
}

fn confidence_profile_name(profile: ConfidenceProfile) -> &'static str {
  match profile {
    ConfidenceProfile::Strict => "strict",
    ConfidenceProfile::Balanced => "balanced",
    ConfidenceProfile::Fast => "fast",
  }
}

fn profile_reason_code(profile: ConfidenceProfile) -> &'static str {
  match profile {
    ConfidenceProfile::Strict => RC_CONFIDENCE_PROFILE_STRICT,
    ConfidenceProfile::Balanced => RC_CONFIDENCE_PROFILE_BALANCED,
    ConfidenceProfile::Fast => RC_CONFIDENCE_PROFILE_FAST,
  }
}

fn is_bot_authored_pull_request() -> bool {
  let event = std::env::var("GITHUB_EVENT_NAME").ok();
  let is_pr_event = matches!(event.as_deref(), Some("pull_request") | Some("pull_request_target"));
  if !is_pr_event {
    return false;
  }

  std::env::var("GITHUB_ACTOR")
    .map(|actor| actor.ends_with("[bot]"))
    .unwrap_or(false)
}

fn validate_surface_reason_invariants(output: &PlanOutput) -> RailResult<()> {
  let trace_ids: BTreeSet<u32> = output.trace.iter().map(|reason| reason.id).collect();
  for (surface, decision) in &output.surfaces {
    if decision.enabled && decision.reasons.is_empty() {
      return Err(RailError::message(format!(
        "planner invariant violated: enabled surface '{}' has no trace reasons",
        surface
      )));
    }

    for reason in &decision.reasons {
      if !trace_ids.contains(reason) {
        return Err(RailError::message(format!(
          "planner invariant violated: surface '{}' references missing trace reason id {}",
          surface, reason
        )));
      }
    }
  }

  Ok(())
}

fn ensure_surface(surfaces: &mut Vec<String>, surface: &str) {
  if !surfaces.iter().any(|existing| existing == surface) {
    surfaces.push(String::from(surface));
  }
}

fn build_execution_scope(
  impact: &PlanImpact,
  surfaces: &BTreeMap<String, SurfaceDecision>,
  workspace_package_count: usize,
  resolved_base: &str,
  resolved_head: &str,
) -> ExecutionScope {
  let crates = impacted_crates_for_scope(impact);
  let package_scoped_surface_enabled = surfaces
    .iter()
    .any(|(name, decision)| decision.enabled && PACKAGE_SCOPED_SURFACES.contains(&name.as_str()));

  let (mode, crates) = if !package_scoped_surface_enabled {
    (ExecutionScopeMode::Empty, Vec::new())
  } else if crates.is_empty() || crates.len() == workspace_package_count {
    (ExecutionScopeMode::Workspace, Vec::new())
  } else {
    (ExecutionScopeMode::Crates, crates)
  };

  ExecutionScope {
    scope_contract_version: SCOPE_CONTRACT_VERSION,
    resolved_base: resolved_base.to_string(),
    resolved_head: resolved_head.to_string(),
    mode,
    crates,
    surfaces: scope_surfaces(surfaces),
  }
}

fn impacted_crates_for_scope(impact: &PlanImpact) -> Vec<String> {
  impact
    .direct_crates
    .iter()
    .chain(&impact.transitive_crates)
    .cloned()
    .collect::<BTreeSet<_>>()
    .into_iter()
    .collect()
}

fn scope_surfaces(surfaces: &BTreeMap<String, SurfaceDecision>) -> BTreeMap<String, bool> {
  surfaces
    .iter()
    .map(|(name, decision)| (name.clone(), decision.enabled))
    .collect()
}

/// Render concise planner explain text used by command surfaces that consume planning.
pub(crate) fn render_plan_explain(output: &PlanOutput) -> String {
  format_text(output, true)
}

fn reason_description(code: &str) -> &'static str {
  match code {
    RC_FILE_KIND_RUST_SRC => "Rust source file changed",
    RC_FILE_KIND_RUST_TEST => "Rust test file changed",
    RC_FILE_KIND_RUST_BENCH => "Rust benchmark file changed",
    RC_FILE_KIND_TOML_MANIFEST => "Cargo.toml changed",
    RC_FILE_KIND_TOML_WORKSPACE => "Workspace Cargo.toml changed",
    RC_FILE_KIND_TOML_TOOLING => "Tooling config changed",
    RC_FILE_KIND_CI => "CI or workflow file changed",
    RC_FILE_KIND_SCRIPT => "Script file changed",
    RC_FILE_KIND_DOCS => "Documentation changed",
    RC_FILE_KIND_CUSTOM => "Custom pattern matched",
    RC_FILE_KIND_UNCLASSIFIED => "Unclassified file changed",
    RC_FILE_OWNS_CRATE_DIRECT => "File directly owns a crate",
    RC_TRANSITIVE_DEPENDS_ON_DIRECT => "Transitive dependency of changed crate",
    RC_OWNER_UNCERTAIN_FALLBACK => "Conservative fallback for uncertain ownership",
    RC_CONFIDENCE_PROFILE_STRICT => "Strict confidence profile active",
    RC_CONFIDENCE_PROFILE_BALANCED => "Balanced confidence profile active",
    RC_CONFIDENCE_PROFILE_FAST => "Fast confidence profile active",
    RC_CONFIDENCE_STRICT_OWNER_EXPANSION => "Strict mode expands owned crates",
    RC_CONFIDENCE_FAST_SKIP_TRANSITIVE => "Fast mode skips transitive expansion",
    RC_BOT_PR_CONFIDENCE_OVERRIDE => "Bot PR confidence override applied",
    _ => "Planner reason",
  }
}

fn reason_lookup(output: &PlanOutput) -> BTreeMap<u32, &TraceReason> {
  output.trace.iter().map(|reason| (reason.id, reason)).collect()
}

fn summarize_reason_ids(reason_ids: &[u32], lookup: &BTreeMap<u32, &TraceReason>) -> String {
  let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
  for reason_id in reason_ids {
    if let Some(reason) = lookup.get(reason_id) {
      *counts.entry(reason.code.as_ref()).or_default() += 1;
    }
  }

  let mut ranked: Vec<(&str, usize)> = counts.into_iter().collect();
  ranked.sort_by(|(left_code, left_count), (right_code, right_count)| {
    right_count.cmp(left_count).then_with(|| left_code.cmp(right_code))
  });

  let parts: Vec<String> = ranked
    .into_iter()
    .take(3)
    .map(|(code, count)| {
      let description = reason_description(code);
      if count > 1 {
        format!("{} ({count}x)", description)
      } else {
        description.to_string()
      }
    })
    .collect();

  if parts.is_empty() {
    "No planner reasons".to_string()
  } else {
    parts.join("; ")
  }
}

fn collect_active_reason_ids(output: &PlanOutput) -> Vec<u32> {
  let mut ids = BTreeSet::new();
  for decision in output.surfaces.values() {
    if decision.enabled {
      ids.extend(decision.reasons.iter().copied());
    }
  }
  ids.into_iter().collect()
}

fn active_surface_names(output: &PlanOutput) -> Vec<&str> {
  output
    .scope
    .surfaces
    .iter()
    .filter(|(_, enabled)| **enabled)
    .map(|(name, _)| name.as_str())
    .collect()
}

fn summarize_surface_reason(output: &PlanOutput, surface: &str) -> Option<String> {
  let decision = output.surfaces.get(surface)?;
  if !decision.enabled {
    return None;
  }

  let lookup = reason_lookup(output);
  Some(summarize_reason_ids(&decision.reasons, &lookup))
}

fn resolve_refs(ctx: &WorkspaceContext, opts: &PlanOptions) -> RailResult<ResolvedRefs> {
  if let (Some(from), Some(to)) = (&opts.from, &opts.to) {
    return Ok(ResolvedRefs {
      since: opts.since.clone(),
      from: Some(from.clone()),
      to: Some(to.clone()),
      merge_base: opts.merge_base,
      resolved_base: from.clone(),
      resolved_head: to.clone(),
    });
  }

  let resolved_base = if opts.merge_base {
    let default_branch = detect_default_base_ref(ctx.git.git())?;
    ctx.git.git().get_merge_base(&default_branch, "HEAD")?
  } else if let Some(since) = &opts.since {
    since.clone()
  } else {
    detect_default_base_ref(ctx.git.git())?
  };

  Ok(ResolvedRefs {
    since: opts.since.clone(),
    from: opts.from.clone(),
    to: opts.to.clone(),
    merge_base: opts.merge_base,
    resolved_base,
    resolved_head: "WORKTREE".to_string(),
  })
}

fn collect_changed_files(ctx: &WorkspaceContext, refs: &ResolvedRefs) -> RailResult<Vec<String>> {
  let raw = if let (Some(from), Some(to)) = (refs.from.as_deref(), refs.to.as_deref()) {
    ctx.git.git().get_changed_files_between(from, Some(to))?
  } else {
    ctx.git.git().get_changed_files_between(&refs.resolved_base, None)?
  };

  let mut files: Vec<String> = raw
    .into_iter()
    .filter_map(|(git_path, _)| ctx.to_workspace_path(&git_path))
    .map(|p| crate::utils::path_to_git_format(&p))
    .collect();

  files.sort();
  files.dedup();
  Ok(files)
}

fn compile_custom_patterns(ctx: &WorkspaceContext) -> Vec<(String, Pattern)> {
  let mut patterns = Vec::new();

  let Some(config) = &ctx.config else {
    return patterns;
  };

  let mut names: Vec<String> = config.change_detection.custom.keys().cloned().collect();
  names.sort();

  for name in names {
    let Some(globs) = config.change_detection.custom.get(&name) else {
      continue;
    };
    for glob in globs {
      if let Ok(pattern) = Pattern::new(glob) {
        patterns.push((name.clone(), pattern));
      }
    }
  }

  patterns
}

fn classify_file_kind(path: &str) -> FileKind {
  if path.ends_with(".rs") {
    if path.starts_with("benches/") || path.contains("/benches/") {
      return FileKind {
        kind: "rust".to_string(),
        sub_kind: Some("bench".to_string()),
        reason_code: RC_FILE_KIND_RUST_BENCH,
      };
    }

    if path.starts_with("tests/") || path.contains("/tests/") {
      return FileKind {
        kind: "rust".to_string(),
        sub_kind: Some("test".to_string()),
        reason_code: RC_FILE_KIND_RUST_TEST,
      };
    }

    return FileKind {
      kind: "rust".to_string(),
      sub_kind: Some("src".to_string()),
      reason_code: RC_FILE_KIND_RUST_SRC,
    };
  }

  if path == "rust-toolchain.toml"
    || path == "rust-toolchain"
    || path.ends_with(".cargo/config")
    || path.ends_with(".cargo/config.toml")
  {
    return FileKind {
      kind: "toml".to_string(),
      sub_kind: Some("tooling".to_string()),
      reason_code: RC_FILE_KIND_TOML_TOOLING,
    };
  }

  if path.ends_with(".toml") {
    if path == "Cargo.toml" {
      return FileKind {
        kind: "toml".to_string(),
        sub_kind: Some("workspace".to_string()),
        reason_code: RC_FILE_KIND_TOML_WORKSPACE,
      };
    }

    if path.ends_with("Cargo.toml") {
      return FileKind {
        kind: "toml".to_string(),
        sub_kind: Some("manifest".to_string()),
        reason_code: RC_FILE_KIND_TOML_MANIFEST,
      };
    }

    return FileKind {
      kind: "toml".to_string(),
      sub_kind: Some("tooling".to_string()),
      reason_code: RC_FILE_KIND_TOML_TOOLING,
    };
  }

  if path.starts_with(".github/") || path.ends_with(".yml") || path.ends_with(".yaml") {
    return FileKind {
      kind: "ci".to_string(),
      sub_kind: None,
      reason_code: RC_FILE_KIND_CI,
    };
  }

  if is_script(path) {
    return FileKind {
      kind: "script".to_string(),
      sub_kind: None,
      reason_code: RC_FILE_KIND_SCRIPT,
    };
  }

  if is_docs(path) {
    return FileKind {
      kind: "docs".to_string(),
      sub_kind: None,
      reason_code: RC_FILE_KIND_DOCS,
    };
  }

  if is_repo_config(path) {
    return FileKind {
      kind: "config".to_string(),
      sub_kind: Some("repo".to_string()),
      reason_code: RC_FILE_KIND_REPO_CONFIG,
    };
  }

  FileKind {
    kind: "script".to_string(),
    sub_kind: None,
    reason_code: RC_FILE_KIND_UNCLASSIFIED,
  }
}

fn custom_surfaces_for_path(path: &str, custom_patterns: &[(String, Pattern)]) -> Vec<String> {
  let mut surfaces = Vec::new();

  for (name, pattern) in custom_patterns {
    if pattern.matches(path) {
      ensure_surface(&mut surfaces, &format!("custom:{}", name));
    }
  }

  surfaces
}

fn is_script(path: &str) -> bool {
  path.ends_with(".sh")
    || path.ends_with(".bash")
    || path.ends_with(".zsh")
    || path.ends_with(".ps1")
    || path.ends_with(".py")
    || path.ends_with(".rb")
    || path.ends_with(".pl")
    || path == "justfile"
    || path == "Justfile"
    || path == "Makefile"
    || path == "makefile"
    || path == "GNUmakefile"
}

fn is_docs(path: &str) -> bool {
  path.ends_with(".md")
    || path.ends_with(".txt")
    || path.ends_with(".adoc")
    || path.ends_with(".rst")
    || path.ends_with("LICENSE")
    || path.ends_with("README")
}

/// Repository configuration files that don't affect Rust build.
///
/// These are root-level dotfiles and config files that configure
/// tooling, editors, or repository behavior but don't require
/// rebuilding or retesting Rust code.
fn is_repo_config(path: &str) -> bool {
  // Only match root-level files (no directory separator)
  if path.contains('/') {
    return false;
  }

  // Common dotfiles
  matches!(
    path,
    ".gitignore"
      | ".gitattributes"
      | ".editorconfig"
      | ".dockerignore"
      | ".prettierrc"
      | ".prettierignore"
      | ".eslintrc"
      | ".eslintignore"
      | ".npmrc"
      | ".nvmrc"
      | ".node-version"
      | ".python-version"
      | ".ruby-version"
      | ".tool-versions"
  )
}

fn owner_scope(path: &str, owners: &[String]) -> String {
  if !owners.is_empty() {
    return "crate".to_string();
  }

  if path.starts_with(".github/")
    || path.starts_with("docs/")
    || path.starts_with("scripts/")
    || path.starts_with(".config/")
    || path.starts_with(".cargo/")
    || !path.contains('/')
  {
    "workspace".to_string()
  } else {
    "unowned".to_string()
  }
}

fn derive_surfaces_for_kind(kind: &FileKind) -> Vec<String> {
  // Static slices for common surface combinations
  static BUILD_TEST: &[&str] = &["build", "test"];
  static TEST_ONLY: &[&str] = &["test"];
  static BENCH_ONLY: &[&str] = &["bench"];
  static INFRA_BUILD_TEST: &[&str] = &["infra", "build", "test"];
  static INFRA_ONLY: &[&str] = &["infra"];
  static DOCS_ONLY: &[&str] = &["docs"];

  match (kind.kind.as_str(), kind.sub_kind.as_deref()) {
    ("rust", Some("src")) => BUILD_TEST.iter().map(|&s| String::from(s)).collect(),
    ("rust", Some("test")) => TEST_ONLY.iter().map(|&s| String::from(s)).collect(),
    ("rust", Some("bench")) => BENCH_ONLY.iter().map(|&s| String::from(s)).collect(),
    ("toml", Some("manifest")) => BUILD_TEST.iter().map(|&s| String::from(s)).collect(),
    ("toml", Some("workspace")) | ("toml", Some("tooling")) => {
      INFRA_BUILD_TEST.iter().map(|&s| String::from(s)).collect()
    }
    ("ci", _) | ("script", _) => INFRA_ONLY.iter().map(|&s| String::from(s)).collect(),
    ("docs", _) | ("config", Some("repo")) => DOCS_ONLY.iter().map(|&s| String::from(s)).collect(),
    (custom, _) if custom.starts_with("custom:") => vec![String::from(custom)],
    _ => DOCS_ONLY.iter().map(|&s| String::from(s)).collect(),
  }
}

fn compute_transitive_impact(ctx: &WorkspaceContext, direct_crates: &BTreeSet<String>) -> RailResult<Vec<String>> {
  let direct_set: HashSet<String> = direct_crates.iter().cloned().collect();
  let mut transitive: Vec<String> = ctx
    .graph
    .transitive_dependents_of_set(&direct_set)?
    .into_iter()
    .collect();
  transitive.sort();

  Ok(transitive)
}

fn emit_transitive_build_test_trace(
  ctx: &WorkspaceContext,
  build_test_seed_crates: &BTreeSet<String>,
  trace: &mut Vec<TraceReason>,
  surface_refs: &mut BTreeMap<String, BTreeSet<u32>>,
) -> RailResult<()> {
  // Use owned Strings once, reuse via slice reference
  static BUILD_TEST_SURFACES: &[&str] = &["build", "test"];
  let surfaces: Vec<String> = BUILD_TEST_SURFACES.iter().map(|&s| String::from(s)).collect();
  for direct in build_test_seed_crates {
    let deps = ctx.graph.transitive_dependents(direct)?;
    for dependent in deps {
      push_trace(
        trace,
        surface_refs,
        RC_TRANSITIVE_DEPENDS_ON_DIRECT,
        None,
        Some(&dependent),
        Some(direct),
        &surfaces,
      );
    }
  }

  Ok(())
}

fn file_kind_seeds_build_test_transitive(kind: &FileKind) -> bool {
  matches!(
    (kind.kind.as_str(), kind.sub_kind.as_deref()),
    ("rust", Some("src")) | ("toml", Some("manifest")) | ("toml", Some("workspace")) | ("toml", Some("tooling"))
  )
}

fn build_surfaces(
  surface_refs: &BTreeMap<String, BTreeSet<u32>>,
  custom_patterns: &[(String, Pattern)],
) -> BTreeMap<String, SurfaceDecision> {
  // Use static slice and map to owned strings
  static BUILTIN_SURFACES: &[&str] = &["build", "test", "bench", "docs", "infra"];
  let mut surface_names: Vec<String> = BUILTIN_SURFACES.iter().map(|&s| String::from(s)).collect();

  let mut custom_names: BTreeSet<String> = BTreeSet::new();
  for (name, _) in custom_patterns {
    custom_names.insert(format!("custom:{}", name));
  }

  surface_names.extend(custom_names);

  let mut result = BTreeMap::new();
  for surface in surface_names {
    let reasons: Vec<u32> = surface_refs
      .get(&surface)
      .map(|set| set.iter().copied().collect())
      .unwrap_or_default();

    result.insert(
      surface,
      SurfaceDecision {
        enabled: !reasons.is_empty(),
        reasons,
      },
    );
  }

  result
}

fn push_trace(
  trace: &mut Vec<TraceReason>,
  surface_refs: &mut BTreeMap<String, BTreeSet<u32>>,
  code: &'static str,
  file: Option<&str>,
  crate_name: Option<&str>,
  depends_on: Option<&str>,
  surfaces: &[String],
) -> u32 {
  let id = (trace.len() + 1) as u32;

  for surface in surfaces {
    surface_refs.entry(surface.clone()).or_default().insert(id);
  }

  trace.push(TraceReason {
    id,
    code,
    file: file.map(ToString::to_string),
    crate_name: crate_name.map(ToString::to_string),
    depends_on: depends_on.map(ToString::to_string),
    surface: surfaces.first().cloned(),
  });

  id
}

fn format_text(output: &PlanOutput, explain: bool) -> String {
  use std::fmt::Write as _;

  let estimated_capacity = 256 + (output.impact.direct_crates.len() * 24) + (output.scope.crates.len() * 24);
  let mut out = String::with_capacity(estimated_capacity);
  let active_surfaces = active_surface_names(output);
  let lookup = reason_lookup(output);
  let top_reasons = summarize_reason_ids(&collect_active_reason_ids(output), &lookup);

  out.push_str("plan\n\n");
  let _ = writeln!(out, "base: {}", output.scope.resolved_base);
  let _ = writeln!(out, "changed files: {}", output.files.len());
  let _ = writeln!(
    out,
    "surfaces: {}",
    if active_surfaces.is_empty() {
      "none".to_string()
    } else {
      active_surfaces.join(", ")
    }
  );
  match output.scope.mode {
    ExecutionScopeMode::Workspace => {
      let _ = writeln!(out, "scope: workspace");
    }
    ExecutionScopeMode::Crates => {
      let _ = writeln!(out, "scope: crates ({})", output.scope.crates.len());
    }
    ExecutionScopeMode::Empty => {
      let _ = writeln!(out, "scope: empty");
    }
  }
  if !output.impact.direct_crates.is_empty() {
    let _ = writeln!(
      out,
      "direct crates ({}): {}",
      output.impact.direct_crates.len(),
      format_preview_list(&output.impact.direct_crates, 12)
    );
  }
  if matches!(output.scope.mode, ExecutionScopeMode::Crates) && !output.scope.crates.is_empty() {
    let _ = writeln!(
      out,
      "execution crates ({}): {}",
      output.scope.crates.len(),
      format_preview_list(&output.scope.crates, 12)
    );
  }
  let _ = writeln!(out, "why: {}", top_reasons);

  if explain {
    out.push('\n');
    out.push_str("explain:\n");
    for surface in active_surfaces {
      if let Some(summary) = summarize_surface_reason(output, surface) {
        let _ = writeln!(out, "  {}: {}", surface, summary);
      }
    }
  }

  out
}

fn format_github(output: &PlanOutput, debug: bool) -> RailResult<String> {
  use std::fmt::Write as _;

  let scope_json = to_json(&output.scope)?;
  let plan_json = if debug { Some(to_json(output)?) } else { None };
  let mut out = String::with_capacity(160 + scope_json.len() + plan_json.as_ref().map_or(0, String::len));
  let _ = writeln!(out, "build={}", surface_enabled(output, "build"));
  let _ = writeln!(out, "test={}", surface_enabled(output, "test"));
  let _ = writeln!(out, "bench={}", surface_enabled(output, "bench"));
  let _ = writeln!(out, "docs={}", surface_enabled(output, "docs"));
  let _ = writeln!(out, "infra={}", surface_enabled(output, "infra"));
  let _ = writeln!(out, "base_ref={}", output.inputs.refs.resolved_base);
  let _ = writeln!(out, "scope_json={}", scope_json);
  if let Some(plan_json) = plan_json {
    let _ = writeln!(out, "plan_json={}", plan_json);
  }

  Ok(out)
}

fn surface_enabled(output: &PlanOutput, key: &str) -> bool {
  output.surfaces.get(key).map(|s| s.enabled).unwrap_or(false)
}

fn is_shallow_clone(workspace_root: &Path) -> bool {
  // Check for .git/shallow file which indicates a shallow clone
  let shallow_file = workspace_root.join(".git/shallow");
  shallow_file.exists()
}

fn write_output(content: &str, output_file: Option<&PathBuf>) -> RailResult<()> {
  match output_file {
    Some(path) => {
      let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(|e| RailError::message(format!("failed to open '{}': {}", path.display(), e)))?;
      writeln!(file, "{}", content)
        .map_err(|e| RailError::message(format!("failed to write '{}': {}", path.display(), e)))?;
      crate::progress!("output: {}", path.display());
    }
    None => {
      println!("{}", content);
    }
  }

  Ok(())
}
