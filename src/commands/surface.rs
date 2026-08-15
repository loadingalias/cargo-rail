//! Complete Rust declaration reachability and visibility analysis.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use cargo_metadata::{Package, TargetKind};
use serde::Serialize;

use crate::backup::{BackupManager, BackupMetadata};
use crate::cargo::{ManifestAnalyzer, TargetSpecificationIdentity};
use crate::commands::common::SurfaceOutputFormat;
use crate::compiler::facts::{
  CompilerFactDomain, CompilerFactPackage, CompilerFactTargetKind, ValidatedCompilerFactObject,
};
use crate::compiler::{CompilerAnalysisMetrics, CompilerCacheIdentity, CompilerDiagnosticsCollector};
use crate::config::{
  SurfaceConfig, SurfaceConsumerScope, SurfaceCrateVisibility, SurfaceExclude, SurfaceLintLevel, SurfaceOverride,
};
use crate::error::{RailError, RailResult};
use crate::mutation::{
  ExpectedMutation, MutationAction, MutationEffect, MutationPlan, MutationRisk, MutationTrace, build_plan,
  validate_changed_paths_with_allowed_paths, validate_pre_apply, write_receipt,
};
use crate::source::{ContentDigest, RepositoryPath};
use crate::surface::{
  SurfaceAnalysis, SurfaceFinding, SurfaceFindingKind, SurfaceGraph, SurfaceGraphMetrics, SurfaceItemAnalysis,
  SurfacePolicy, SurfaceProductKind, SurfaceProductRoot, retention_reason_code,
};
use crate::workspace::{CargoState, WorkspaceContext, WorkspaceSnapshot};

const SURFACE_CONTRACT_VERSION: u32 = 1;
const SURFACE_SCHEMA_JSON: &str = include_str!("../../schemas/surface-v1.schema.json");

/// Options for the `surface` domain command.
#[derive(Debug)]
pub struct SurfaceOptions {
  /// Run read-only analysis.
  pub check: bool,
  /// Apply exact visibility reductions.
  pub fix: bool,
  /// Render an exact mutation plan without writing.
  pub dry_run: bool,
  /// Create a bounded backup before mutation.
  pub backup: bool,
  /// Report projection.
  pub format: SurfaceOutputFormat,
  /// Optional output file.
  pub output: Option<PathBuf>,
  /// Include reason chains in human output.
  pub explain: bool,
}

#[derive(Debug, Serialize)]
struct SurfaceReport {
  surface_contract_version: u32,
  snapshot: SurfaceSnapshotReport,
  config: SurfaceConfigReport,
  toolchain: SurfaceToolchainReport,
  products: Vec<SurfaceProductReport>,
  targets: Vec<String>,
  features: Vec<SurfaceFeatureView>,
  completeness: SurfaceCompleteness,
  findings: Vec<SurfaceReportFinding>,
  reasons: Vec<SurfaceReason>,
  fragments: Vec<SurfaceFragmentReport>,
  cache: SurfaceCacheReport,
  metrics: SurfaceMetricsReport,
  mutation: Option<SurfaceMutationReport>,
}

#[derive(Debug, Serialize)]
struct SurfaceSnapshotReport {
  identity: String,
  configuration_fingerprint: String,
}

#[derive(Debug, Serialize)]
struct SurfaceConfigReport {
  identity: String,
  consumer_scope: SurfaceConsumerScope,
  crate_visibility: SurfaceCrateVisibility,
  preserve_uniform_fields: bool,
}

#[derive(Debug, Serialize)]
struct SurfaceToolchainReport {
  fingerprint: String,
  rustc: String,
  host: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct SurfaceProductReport {
  package: String,
  target: String,
  kind: SurfaceProductKind,
  implicit: bool,
  reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct SurfaceFeatureView {
  package: String,
  target: String,
  platform: String,
  features: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SurfaceCompleteness {
  complete: bool,
  fragments: u64,
  items: u64,
  edges: u64,
  entry_points: u64,
  retentions: u64,
  retention_reasons: BTreeMap<String, u64>,
}

#[derive(Debug, Serialize)]
struct SurfaceReportFinding {
  #[serde(flatten)]
  finding: SurfaceFinding,
  level: SurfaceLintLevel,
  #[serde(skip_serializing_if = "Option::is_none")]
  policy_reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct SurfaceReason {
  code: &'static str,
  description: &'static str,
}

#[derive(Debug, Serialize)]
struct SurfaceFragmentReport {
  identity: String,
  unit_identity: String,
  package: String,
  target: String,
  target_kind: CompilerFactTargetKind,
  domain: CompilerFactDomain,
  platform: String,
  complete: bool,
  items: u64,
  edges: u64,
}

#[derive(Debug, Serialize)]
struct SurfaceCacheReport {
  compiler_fact_objects: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SurfaceMetricsReport {
  acquisition: SurfaceAcquisitionMetricsReport,
  graph: SurfaceGraphMetricsReport,
}

#[derive(Debug, Serialize)]
struct SurfaceAcquisitionMetricsReport {
  analysis_views: usize,
  cargo_views_executed: usize,
  compiler_invocations: usize,
  diagnostic_cache_hits: usize,
  diagnostic_cache_misses: usize,
  fact_cache_hits: usize,
  fact_cache_misses: usize,
  fact_cache_store_failures: usize,
  fact_cache_bypass_reasons: BTreeMap<String, usize>,
  fresh_fragment_bytes: u64,
  retained_fact_object_bytes: u64,
}

#[derive(Debug, Serialize)]
struct SurfaceGraphMetricsReport {
  nodes: usize,
  edges: usize,
  traversals: usize,
  edge_visits: usize,
}

#[derive(Debug, Serialize)]
struct SurfaceMutationReport {
  phase: &'static str,
  plan: MutationPlan,
  #[serde(skip_serializing_if = "Option::is_none")]
  receipt: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  backup: Option<String>,
}

struct SurfaceRun {
  targets: Vec<String>,
  facts: Vec<ValidatedCompilerFactObject>,
  findings: Vec<SurfaceReportFinding>,
  metrics: SurfaceRunMetrics,
}

struct SurfaceRunMetrics {
  acquisition: CompilerAnalysisMetrics,
  graph: SurfaceGraphMetrics,
}

#[derive(Debug, Clone, Serialize)]
struct SurfaceVisibilityEdit {
  finding_identity: String,
  declaration_identity: String,
  declaration_start: u64,
  declaration_end: u64,
  visibility_start: u64,
  visibility_end: u64,
  edit_start: u64,
  edit_end: u64,
  original_visibility: String,
  replacement: String,
}

struct SurfaceFileUpdate {
  path: RepositoryPath,
  before: Vec<u8>,
  after: Vec<u8>,
}

struct AppliedSurfaceMutation {
  plan: MutationPlan,
  backup: Option<String>,
}

/// Print the current surface-report JSON Schema without workspace acquisition.
pub fn print_surface_schema() {
  print!("{SURFACE_SCHEMA_JSON}");
}

/// Analyze the complete configured Rust source surface.
pub fn run_surface(ctx: &WorkspaceContext, options: SurfaceOptions) -> RailResult<()> {
  if options.format.is_json_like() {
    crate::output::set_json_mode(true);
  }
  if options.check == options.fix || options.check && (options.dry_run || options.backup) {
    return Err(RailError::with_help(
      "surface requires exactly one operation",
      "use '--check' for read-only analysis or '--fix' for exact visibility repair",
    ));
  }

  let config = ctx.config().map(|config| config.surface.clone()).unwrap_or_default();
  config.validate().map_err(RailError::Config)?;
  let initial = analyze_surface(ctx, &config)?;

  if options.check {
    let report = build_report(
      ctx.snapshot()?,
      &config,
      &initial.targets,
      &initial.facts,
      &initial.metrics,
      initial.findings,
      None,
    )?;
    return emit_report(report, &options, "check");
  }

  let (plan, updates) = plan_visibility_mutation(ctx, &initial.findings)?;
  if options.dry_run {
    let report = build_report(
      ctx.snapshot()?,
      &config,
      &initial.targets,
      &initial.facts,
      &initial.metrics,
      initial.findings,
      Some(SurfaceMutationReport {
        phase: "planned",
        plan,
        receipt: None,
        backup: None,
      }),
    )?;
    return emit_report(report, &options, "fix_dry_run");
  }

  let applied = apply_visibility_mutation(ctx, plan, &updates, options.backup)?;
  let verification = (|| {
    let verified_context = ctx.recapture_after_mutation()?;
    let verified_config = verified_context
      .config()
      .map(|config| config.surface.clone())
      .unwrap_or_default();
    if verified_config != config {
      return Err(RailError::message(
        "surface configuration changed across post-mutation verification",
      ));
    }
    let verified = analyze_surface(&verified_context, &verified_config)?;
    verified_context.validate_snapshot_unchanged()?;
    verify_surface_updates(&ctx.git()?.git().worktree_root, &updates)?;
    Ok((verified_context, verified_config, verified))
  })();
  let (verified_context, verified_config, verified) = match verification {
    Ok(verified) => verified,
    Err(error) => return recover_failed_verification(ctx, &applied, &updates, error),
  };
  let mutation = finalize_surface_mutation(ctx, applied, &updates)?;
  let report = build_report(
    verified_context.snapshot()?,
    &verified_config,
    &verified.targets,
    &verified.facts,
    &verified.metrics,
    verified.findings,
    Some(mutation),
  )?;
  emit_report(report, &options, "fix")
}

fn analyze_surface(ctx: &WorkspaceContext, config: &SurfaceConfig) -> RailResult<SurfaceRun> {
  let snapshot = ctx.snapshot()?;
  validate_workspace_policy(ctx, snapshot, config)?;
  let workspace_packages = ctx.cargo().workspace_members();
  let manifests = ManifestAnalyzer::parse_snapshot(snapshot, &workspace_packages)?;
  let cache_identity = CompilerCacheIdentity::capture(snapshot)?;
  let targets = selected_target_views(config);
  let target_refs = targets.iter().map(String::as_str).collect::<Vec<_>>();
  let typed_packages = workspace_packages
    .iter()
    .map(|package| package.name.to_string())
    .collect::<BTreeSet<_>>();
  let doctest_packages = workspace_packages
    .iter()
    .filter(|package| package.targets.iter().any(|target| target.doctest))
    .map(|package| package.name.to_string())
    .collect::<BTreeSet<_>>();
  let collector =
    CompilerDiagnosticsCollector::with_identity(ctx.workspace_root(), &manifests, target_refs, &cache_identity);
  let evidence = collector.collect_with_typed_items(snapshot, &[], &typed_packages, &doctest_packages)?;
  if evidence.compiler_facts.is_empty() {
    return Err(RailError::message(
      "surface analysis produced no authenticated compiler facts",
    ));
  }

  let graph = SurfaceGraph::from_compiler_facts(&evidence.compiler_facts)?;
  let closed_world_packages = closed_world_packages(&workspace_packages, config.consumer_scope);
  let products = configured_product_roots(config);
  let policy = SurfacePolicy::new(
    closed_world_packages,
    config.crate_visibility == SurfaceCrateVisibility::Allow,
  )
  .with_products(products)
  .preserving_uniform_fields(config.preserve_uniform_fields);
  let analysis = graph.analyze(&policy)?;
  let findings = apply_diagnostic_policy(&analysis, config)?;
  Ok(SurfaceRun {
    targets,
    facts: evidence.compiler_facts,
    findings,
    metrics: SurfaceRunMetrics {
      acquisition: evidence.metrics,
      graph: analysis.metrics,
    },
  })
}

fn emit_report(report: SurfaceReport, options: &SurfaceOptions, mode: &'static str) -> RailResult<()> {
  let exit_code = i32::from(!report.findings.is_empty());
  let rendered = render_report(&report, options.format, options.explain, mode, exit_code)?;
  write_output(&rendered, options.output.as_deref())?;

  if exit_code == 0 {
    Ok(())
  } else {
    Err(RailError::ExitWithCode { code: exit_code })
  }
}

fn plan_visibility_mutation(
  ctx: &WorkspaceContext,
  findings: &[SurfaceReportFinding],
) -> RailResult<(MutationPlan, Vec<SurfaceFileUpdate>)> {
  let mut by_path = BTreeMap::<String, Vec<&SurfaceFinding>>::new();
  for report_finding in findings {
    let finding = &report_finding.finding;
    if finding.kind == SurfaceFindingKind::DeadPublic {
      continue;
    }
    if finding.source_generated {
      return Err(RailError::message(format!(
        "surface fix '{}' targets generated source '{}'",
        finding.identity, finding.source
      )));
    }
    by_path.entry(finding.source.clone()).or_default().push(finding);
  }

  let snapshot = ctx.snapshot()?;
  let git_root = &ctx.git()?.git().worktree_root;
  let mut actions = Vec::with_capacity(by_path.len());
  let mut updates = Vec::with_capacity(by_path.len());
  let mut trace = Vec::new();
  for (path, findings) in by_path {
    let repository_path = RepositoryPath::new(Path::new(&path))?;
    let before = snapshot.read_source_file(&repository_path)?.ok_or_else(|| {
      RailError::message(format!(
        "surface fix source '{}' is not one exact captured regular file",
        path
      ))
    })?;
    let (after, edits) = apply_visibility_edits(&before, &findings)?;
    let before_digest = format!("sha256:{}", ContentDigest::sha256(&before));
    let after_digest = format!("sha256:{}", ContentDigest::sha256(&after));
    let payload = serde_json::json!({
      "path": path,
      "before_digest": before_digest,
      "after_digest": after_digest,
      "edits": &edits,
    });
    let expected = ExpectedMutation::capture(git_root, repository_path.as_path().to_path_buf(), MutationEffect::Write);
    actions.push(
      MutationAction::new(
        "SURFACE_REDUCE_VISIBILITY",
        repository_path.to_string(),
        Some(format!("apply {} exact visibility edit(s)", edits.len())),
      )
      .with_payload(payload)
      .with_mutations(vec![expected]),
    );
    trace.push(MutationTrace::new(
      "SURFACE_EXACT_VISIBILITY_SPANS",
      format!(
        "{} binds {} non-overlapping visibility span(s) to captured source bytes",
        repository_path,
        edits.len()
      ),
    ));
    updates.push(SurfaceFileUpdate {
      path: repository_path,
      before,
      after,
    });
  }

  let risks = if updates.is_empty() {
    Vec::new()
  } else {
    vec![MutationRisk::new(
      "SOURCE_VISIBILITY_REWRITE",
      "medium",
      "Exact Rust visibility bytes will be replaced and every configured compiler view revalidated.",
    )]
  };
  let plan = build_plan(ctx, "surface", actions, risks, trace)?;
  Ok((plan, updates))
}

fn apply_visibility_edits(
  before: &[u8],
  findings: &[&SurfaceFinding],
) -> RailResult<(Vec<u8>, Vec<SurfaceVisibilityEdit>)> {
  let mut edits = findings
    .iter()
    .map(|finding| {
      let start = usize::try_from(finding.visibility_start)
        .map_err(|_| RailError::message("surface visibility start exceeds this platform"))?;
      let end = usize::try_from(finding.visibility_end)
        .map_err(|_| RailError::message("surface visibility end exceeds this platform"))?;
      if start >= end
        || end > before.len()
        || finding.visibility_start < finding.declaration_start
        || finding.visibility_end > finding.declaration_end
      {
        return Err(RailError::message(format!(
          "surface finding '{}' has an invalid visibility byte range",
          finding.identity
        )));
      }
      let original = std::str::from_utf8(&before[start..end]).map_err(|_| {
        RailError::message(format!(
          "surface finding '{}' visibility range is not valid UTF-8",
          finding.identity
        ))
      })?;
      let compact = original
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect::<String>();
      let replacement = match (finding.kind, finding.replacement) {
        (SurfaceFindingKind::DeadPublic, _) => {
          return Err(RailError::message(
            "dead-public findings are report-only and cannot enter a visibility mutation plan",
          ));
        }
        (SurfaceFindingKind::UnnecessaryPublic, Some("pub(crate)")) if compact == "pub" => "pub(crate)",
        (SurfaceFindingKind::UnnecessaryPublic, Some("pub(super)")) if compact == "pub" => "pub(super)",
        (SurfaceFindingKind::UnnecessaryPublic, Some("private")) if compact == "pub" => "",
        (SurfaceFindingKind::UnnecessaryRestrictedVisibility, Some("private")) if restricted_visibility(&compact) => "",
        (SurfaceFindingKind::UnnecessaryCrateVisibility, Some("pub(super)")) if compact == "pub(crate)" => "pub(super)",
        (SurfaceFindingKind::UnnecessaryCrateVisibility, Some("private")) if compact == "pub(crate)" => "",
        _ => {
          return Err(RailError::message(format!(
            "surface finding '{}' expected a different original visibility at {}..{}",
            finding.identity, finding.visibility_start, finding.visibility_end
          )));
        }
      };
      let mut edit_end = end;
      if replacement.is_empty() {
        while before.get(edit_end).is_some_and(u8::is_ascii_whitespace) {
          edit_end = edit_end
            .checked_add(1)
            .ok_or_else(|| RailError::message("surface edit end exceeds this platform"))?;
        }
      }
      Ok(SurfaceVisibilityEdit {
        finding_identity: finding.identity.clone(),
        declaration_identity: finding.item_identity.clone(),
        declaration_start: finding.declaration_start,
        declaration_end: finding.declaration_end,
        visibility_start: finding.visibility_start,
        visibility_end: finding.visibility_end,
        edit_start: finding.visibility_start,
        edit_end: u64::try_from(edit_end).map_err(|_| RailError::message("surface edit end exceeds u64"))?,
        original_visibility: original.to_string(),
        replacement: replacement.to_string(),
      })
    })
    .collect::<RailResult<Vec<_>>>()?;
  edits.sort_by_key(|edit| (edit.edit_start, edit.edit_end, edit.finding_identity.clone()));
  for pair in edits.windows(2) {
    if pair[0].edit_end > pair[1].edit_start
      || pair[0].edit_start == pair[1].edit_start && pair[0].edit_end == pair[1].edit_end
    {
      return Err(RailError::message(format!(
        "surface visibility edits '{}' and '{}' overlap or share one source span",
        pair[0].finding_identity, pair[1].finding_identity
      )));
    }
  }

  let mut after = before.to_vec();
  for edit in edits.iter().rev() {
    let start =
      usize::try_from(edit.edit_start).map_err(|_| RailError::message("surface edit start exceeds this platform"))?;
    let end =
      usize::try_from(edit.edit_end).map_err(|_| RailError::message("surface edit end exceeds this platform"))?;
    after.splice(start..end, edit.replacement.bytes());
  }
  Ok((after, edits))
}

fn restricted_visibility(visibility: &str) -> bool {
  visibility.starts_with("pub(") && visibility.ends_with(')') && visibility != "pub(crate)"
}

fn apply_visibility_mutation(
  ctx: &WorkspaceContext,
  plan: MutationPlan,
  updates: &[SurfaceFileUpdate],
  backup_requested: bool,
) -> RailResult<AppliedSurfaceMutation> {
  validate_pre_apply(ctx, &plan)?;
  ctx.validate_snapshot_unchanged()?;
  let git_root = &ctx.git()?.git().worktree_root;
  revalidate_surface_updates(git_root, updates)?;

  let backup_manager = BackupManager::new(ctx.workspace_root());
  let should_backup = backup_requested || !updates.is_empty() && !backup_manager.has_backups();
  let backup = if should_backup {
    let paths = updates
      .iter()
      .map(|update| {
        crate::utils::path_relative_to(ctx.workspace_root(), &git_root.join(update.path.as_path())).map_err(Into::into)
      })
      .collect::<RailResult<Vec<_>>>()?;
    let mut metadata = BackupMetadata::new("cargo rail surface")
      .with_description(format!("original source for surface operation {}", plan.operation_id));
    if let Some(config) = ctx.config() {
      metadata = metadata.with_config(config.as_ref().clone());
    }
    Some(backup_manager.create_backup(&paths, metadata, 3)?)
  } else {
    None
  };

  validate_pre_apply(ctx, &plan)?;
  revalidate_surface_updates(git_root, updates)?;
  for (index, update) in updates.iter().enumerate() {
    let absolute = git_root.join(update.path.as_path());
    if let Err(error) = crate::utils::write_file_atomic(&absolute, &update.after) {
      let rollback_error = rollback_surface_updates(git_root, &updates[..=index]);
      return match rollback_error {
        Ok(()) => Err(error.context("surface visibility application failed; applied files were restored")),
        Err(rollback) => Err(RailError::with_help(
          format!("surface visibility application failed: {error}; automatic recovery failed: {rollback}"),
          backup.as_ref().map_or_else(
            || "restore the affected files from version control".to_string(),
            |backup| format!("restore backup '{backup}' before retrying"),
          ),
        )),
      };
    }
  }

  if let Err(error) = verify_surface_updates(git_root, updates)
    .and_then(|()| validate_changed_paths_with_allowed_paths(ctx, &plan, &plan.pre_apply.changed_paths))
  {
    let rollback = rollback_surface_updates(git_root, updates);
    return Err(match rollback {
      Ok(()) => error.context("surface post-write validation failed; source files were restored"),
      Err(rollback) => RailError::with_help(
        format!("surface post-write validation failed: {error}; automatic recovery failed: {rollback}"),
        backup.as_ref().map_or_else(
          || "restore the affected files from version control".to_string(),
          |backup| format!("restore backup '{backup}' before retrying"),
        ),
      ),
    });
  }

  Ok(AppliedSurfaceMutation { plan, backup })
}

fn finalize_surface_mutation(
  ctx: &WorkspaceContext,
  applied: AppliedSurfaceMutation,
  updates: &[SurfaceFileUpdate],
) -> RailResult<SurfaceMutationReport> {
  let receipt = write_receipt(
    ctx.workspace_root(),
    "surface",
    "apply",
    "applied",
    applied.plan.clone(),
    vec![MutationTrace::new(
      "SURFACE_VISIBILITY_VERIFIED",
      format!(
        "applied {} authorized source file mutation(s) and revalidated every configured compiler view",
        updates.len()
      ),
    )],
  );
  let receipt = match receipt {
    Ok(receipt) => receipt,
    Err(error) => {
      let rollback = rollback_surface_updates(&ctx.git()?.git().worktree_root, updates);
      return Err(match rollback {
        Ok(()) => error.context("surface receipt publication failed; source files were restored"),
        Err(rollback) => RailError::with_help(
          format!("surface receipt publication failed: {error}; automatic recovery failed: {rollback}"),
          applied.backup.as_ref().map_or_else(
            || "restore the affected files from version control".to_string(),
            |backup| format!("restore backup '{backup}' before retrying"),
          ),
        ),
      });
    }
  };
  Ok(SurfaceMutationReport {
    phase: "applied",
    plan: applied.plan,
    receipt: Some(display_repository_path(&ctx.git()?.git().worktree_root, &receipt)),
    backup: applied.backup,
  })
}

fn recover_failed_verification(
  ctx: &WorkspaceContext,
  applied: &AppliedSurfaceMutation,
  updates: &[SurfaceFileUpdate],
  verification_error: RailError,
) -> RailResult<()> {
  let git_root = &ctx.git()?.git().worktree_root;
  let rollback = rollback_surface_updates(git_root, updates);
  let rollback_message = rollback.as_ref().map_or_else(
    |error| format!("automatic rollback failed: {error}"),
    |()| "automatic rollback succeeded".to_string(),
  );
  let receipt = write_receipt(
    ctx.workspace_root(),
    "surface",
    "verify",
    "failed",
    applied.plan.clone(),
    vec![MutationTrace::new(
      "SURFACE_POST_APPLY_VERIFICATION_FAILED",
      format!("verification failed; {rollback_message}: {verification_error}"),
    )],
  );
  match (rollback, receipt) {
    (Ok(()), Ok(receipt)) => Err(RailError::with_help(
      format!("surface post-apply verification failed; source files were restored: {verification_error}"),
      format!("failure receipt: {}", display_repository_path(git_root, &receipt)),
    )),
    (rollback, receipt) => Err(RailError::with_help(
      format!(
        "surface post-apply verification failed: {verification_error}; rollback: {}; failure receipt: {}",
        rollback
          .err()
          .map_or_else(|| "succeeded".to_string(), |error| error.to_string()),
        receipt
          .err()
          .map_or_else(|| "written".to_string(), |error| error.to_string())
      ),
      applied.backup.as_ref().map_or_else(
        || "restore the affected files from version control before retrying".to_string(),
        |backup| format!("restore backup '{backup}' before retrying"),
      ),
    )),
  }
}

fn revalidate_surface_updates(root: &Path, updates: &[SurfaceFileUpdate]) -> RailResult<()> {
  for update in updates {
    let absolute = root.join(update.path.as_path());
    let metadata = std::fs::symlink_metadata(&absolute)
      .map_err(|error| RailError::message(format!("failed to inspect surface source '{}': {error}", update.path)))?;
    if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) {
      return Err(RailError::message(format!(
        "surface source '{}' is not a regular non-symlink file",
        update.path
      )));
    }
    let relative = crate::utils::path_relative_to(root, &absolute)?;
    if relative != update.path.as_path() || std::fs::read(&absolute)? != update.before {
      return Err(RailError::message(format!(
        "surface source '{}' drifted before visibility application",
        update.path
      )));
    }
  }
  Ok(())
}

fn verify_surface_updates(root: &Path, updates: &[SurfaceFileUpdate]) -> RailResult<()> {
  for update in updates {
    let absolute = root.join(update.path.as_path());
    if std::fs::read(&absolute)? != update.after {
      return Err(RailError::message(format!(
        "surface source '{}' does not contain the planned bytes after application",
        update.path
      )));
    }
  }
  Ok(())
}

fn rollback_surface_updates(root: &Path, updates: &[SurfaceFileUpdate]) -> RailResult<()> {
  let mut errors = Vec::new();
  for update in updates.iter().rev() {
    let absolute = root.join(update.path.as_path());
    match std::fs::read(&absolute) {
      Ok(current) if current == update.before => {}
      Ok(current) if current == update.after => {
        if let Err(error) = crate::utils::write_file_atomic(&absolute, &update.before) {
          errors.push(format!("{}: {error}", update.path));
        }
      }
      Ok(_) => errors.push(format!(
        "{} changed after cargo-rail wrote it; preserving the newer bytes",
        update.path
      )),
      Err(error) => errors.push(format!("{}: {error}", update.path)),
    }
  }
  if errors.is_empty() {
    Ok(())
  } else {
    Err(RailError::message(errors.join("; ")))
  }
}

fn display_repository_path(root: &Path, path: &Path) -> String {
  path
    .strip_prefix(root)
    .map(crate::utils::path_to_git_format)
    .unwrap_or_else(|_| path.display().to_string())
}

fn validate_workspace_policy(
  ctx: &WorkspaceContext,
  snapshot: &WorkspaceSnapshot,
  config: &SurfaceConfig,
) -> RailResult<()> {
  let packages = ctx.cargo().workspace_members();
  let names = packages
    .iter()
    .map(|package| package.name.as_str())
    .collect::<BTreeSet<_>>();
  for (field, package) in config
    .product
    .iter()
    .map(|product| ("surface.product", product.package.as_str()))
    .chain(
      config
        .r#override
        .iter()
        .map(|policy| ("surface.override", policy.package.as_str())),
    )
    .chain(
      config
        .exclude
        .iter()
        .map(|policy| ("surface.exclude", policy.package.as_str())),
    )
  {
    if !names.contains(package) {
      return Err(RailError::message(format!(
        "{field} names unknown workspace package '{package}'"
      )));
    }
  }

  for product in &config.product {
    let package = ctx
      .cargo()
      .get_package(&product.package)
      .ok_or_else(|| RailError::message(format!("surface product package '{}' disappeared", product.package)))?;
    let (name, kind) = product
      .bin
      .as_deref()
      .map(|name| (name, SurfaceProductKind::Binary))
      .or_else(|| product.lib.as_deref().map(|name| (name, SurfaceProductKind::Library)))
      .ok_or_else(|| RailError::message("surface product has no validated target selector"))?;
    if !package.targets.iter().any(|target| {
      target.name == name
        && match kind {
          SurfaceProductKind::Binary => target.kind.contains(&TargetKind::Bin),
          SurfaceProductKind::Library => target.kind.iter().any(is_library_target),
        }
    }) {
      return Err(RailError::message(format!(
        "surface product '{}:{}' does not name an exact Cargo {} target",
        product.package,
        name,
        match kind {
          SurfaceProductKind::Binary => "binary",
          SurfaceProductKind::Library => "library",
        }
      )));
    }
  }

  for exclusion in &config.exclude {
    if let Some(file) = &exclusion.file {
      let path = RepositoryPath::new(Path::new(file))?;
      if !snapshot
        .source()
        .tree()
        .entries()
        .iter()
        .any(|entry| entry.path == path)
      {
        return Err(RailError::message(format!(
          "surface exclusion file '{}' is absent from the captured workspace source",
          file
        )));
      }
    }
  }

  let available_targets = snapshot
    .targets()
    .iter()
    .map(|target| match target.specification() {
      TargetSpecificationIdentity::BuiltIn(name) => name.as_str(),
      TargetSpecificationIdentity::Custom(specification) => specification.name(),
    })
    .collect::<BTreeSet<_>>();
  for target in config.targets.iter().filter(|target| target.as_str() != "host") {
    if !available_targets.contains(target.as_str()) {
      return Err(RailError::message(format!(
        "surface target '{target}' is absent from the captured target views"
      )));
    }
  }
  Ok(())
}

fn is_library_target(kind: &TargetKind) -> bool {
  matches!(
    kind,
    TargetKind::Lib | TargetKind::RLib | TargetKind::DyLib | TargetKind::CDyLib | TargetKind::StaticLib
  )
}

fn selected_target_views(config: &SurfaceConfig) -> Vec<String> {
  config
    .targets
    .iter()
    .map(|target| if target == "host" { "default" } else { target }.to_string())
    .collect()
}

fn closed_world_packages(packages: &[&Package], scope: SurfaceConsumerScope) -> BTreeSet<CompilerFactPackage> {
  if scope != SurfaceConsumerScope::Workspace {
    return BTreeSet::new();
  }
  packages
    .iter()
    .filter(|package| !CargoState::is_package_publishable(package))
    .map(|package| CompilerFactPackage {
      name: package.name.to_string(),
      version: package.version.to_string(),
      source: package.source.as_ref().map(|source| source.repr.clone()),
    })
    .collect()
}

fn configured_product_roots(config: &SurfaceConfig) -> BTreeSet<SurfaceProductRoot> {
  config
    .product
    .iter()
    .filter_map(|product| {
      product
        .bin
        .as_ref()
        .map(|target| (target, SurfaceProductKind::Binary))
        .or_else(|| product.lib.as_ref().map(|target| (target, SurfaceProductKind::Library)))
        .map(|(target, kind)| SurfaceProductRoot {
          package: product.package.clone(),
          target: target.clone(),
          kind,
        })
    })
    .collect()
}

fn apply_diagnostic_policy(
  analysis: &SurfaceAnalysis,
  config: &SurfaceConfig,
) -> RailResult<Vec<SurfaceReportFinding>> {
  for policy in config
    .r#override
    .iter()
    .filter(|policy| policy.level == SurfaceLintLevel::Expect)
  {
    if !analysis
      .findings
      .iter()
      .any(|finding| override_matches(finding, policy))
    {
      return Err(RailError::message(format!(
        "expected surface finding '{}:{}:{}:{}' disappeared",
        policy.lint, policy.package, policy.item, policy.kind
      )));
    }
  }
  for policy in config
    .exclude
    .iter()
    .filter(|policy| policy.level == SurfaceLintLevel::Expect)
  {
    if !analysis.items.iter().any(|item| exclusion_matches_item(item, policy)) {
      return Err(RailError::message(format!(
        "expected surface exclusion scope for package '{}' disappeared",
        policy.package
      )));
    }
  }

  let mut enabled = Vec::new();
  for finding in &analysis.findings {
    let exact = config
      .r#override
      .iter()
      .filter(|policy| override_matches(finding, policy))
      .collect::<Vec<_>>();
    if exact.len() > 1 {
      return Err(RailError::message(format!(
        "surface finding '{}' matches multiple item overrides",
        finding.identity
      )));
    }
    let (level, reason) = if let Some(policy) = exact.first() {
      (policy.level, Some(policy.reason.clone()))
    } else {
      let matches = config
        .exclude
        .iter()
        .filter(|policy| exclusion_matches_finding(finding, policy))
        .collect::<Vec<_>>();
      let levels = matches.iter().map(|policy| policy.level).collect::<BTreeSet<_>>();
      if levels.len() > 1 {
        return Err(RailError::message(format!(
          "surface finding '{}' matches exclusions with conflicting levels",
          finding.identity
        )));
      }
      matches.first().map_or((SurfaceLintLevel::Warn, None), |policy| {
        (policy.level, Some(policy.reason.clone()))
      })
    };
    if matches!(level, SurfaceLintLevel::Warn | SurfaceLintLevel::Deny) {
      enabled.push(SurfaceReportFinding {
        finding: finding.clone(),
        level,
        policy_reason: reason,
      });
    }
  }
  Ok(enabled)
}

fn override_matches(finding: &SurfaceFinding, policy: &SurfaceOverride) -> bool {
  finding.kind.as_str() == policy.lint
    && finding.packages.iter().any(|package| package == &policy.package)
    && finding.item_kind == policy.kind
    && finding
      .diagnostic_paths
      .iter()
      .any(|path| exact_diagnostic_path(path, &policy.item))
}

fn exclusion_matches_finding(finding: &SurfaceFinding, policy: &SurfaceExclude) -> bool {
  finding.packages.iter().any(|package| package == &policy.package)
    && exclusion_matches(&finding.source, &finding.diagnostic_paths, policy)
}

fn exclusion_matches_item(item: &SurfaceItemAnalysis, policy: &SurfaceExclude) -> bool {
  item.packages.iter().any(|package| package == &policy.package)
    && exclusion_matches(&item.source, &item.diagnostic_paths, policy)
}

fn exclusion_matches(source: &str, diagnostic_paths: &[String], policy: &SurfaceExclude) -> bool {
  policy.file.as_deref().is_some_and(|file| file == source)
    || policy.module.as_deref().is_some_and(|module| {
      diagnostic_paths
        .iter()
        .any(|path| diagnostic_module_contains(path, module))
    })
}

fn exact_diagnostic_path(path: &str, expected: &str) -> bool {
  path == expected || path.split_once("::").is_some_and(|(_, suffix)| suffix == expected)
}

fn diagnostic_module_contains(path: &str, module: &str) -> bool {
  let normalized = path.split_once("::").map_or(path, |(_, suffix)| suffix);
  normalized == module
    || normalized
      .strip_prefix(module)
      .is_some_and(|suffix| suffix.starts_with("::"))
}

fn build_report(
  snapshot: &WorkspaceSnapshot,
  config: &SurfaceConfig,
  targets: &[String],
  facts: &[ValidatedCompilerFactObject],
  metrics: &SurfaceRunMetrics,
  findings: Vec<SurfaceReportFinding>,
  mutation: Option<SurfaceMutationReport>,
) -> RailResult<SurfaceReport> {
  let mut fragments = facts
    .iter()
    .map(|fact| {
      let object = fact.object();
      SurfaceFragmentReport {
        identity: fact.identity().to_string(),
        unit_identity: object.unit.identity.clone(),
        package: object.unit.package.name.clone(),
        target: object.unit.cargo_target.clone(),
        target_kind: object.unit.target_kind.clone(),
        domain: object.unit.domain,
        platform: object.unit.platform.clone(),
        complete: object.completion.complete,
        items: object.completion.items,
        edges: object.completion.edges,
      }
    })
    .collect::<Vec<_>>();
  fragments.sort_by(|left, right| left.identity.cmp(&right.identity));
  let object_ids = fragments.iter().map(|fragment| fragment.identity.clone()).collect();

  let features = facts
    .iter()
    .map(|fact| {
      let object = fact.object();
      SurfaceFeatureView {
        package: object.unit.package.name.clone(),
        target: object.unit.cargo_target.clone(),
        platform: object.unit.platform.clone(),
        features: object.unit.features.clone(),
      }
    })
    .collect::<BTreeSet<_>>()
    .into_iter()
    .collect();
  let products = report_products(config, facts);
  let config_bytes = serde_json::to_vec(config)?;
  let reasons = used_reasons(&findings);

  Ok(SurfaceReport {
    surface_contract_version: SURFACE_CONTRACT_VERSION,
    snapshot: SurfaceSnapshotReport {
      identity: snapshot.id().to_string(),
      configuration_fingerprint: snapshot.configuration_fingerprint().to_string(),
    },
    config: SurfaceConfigReport {
      identity: format!("sha256:{}", ContentDigest::sha256(&config_bytes)),
      consumer_scope: config.consumer_scope,
      crate_visibility: config.crate_visibility,
      preserve_uniform_fields: config.preserve_uniform_fields,
    },
    toolchain: SurfaceToolchainReport {
      fingerprint: snapshot.toolchain_fingerprint().to_string(),
      rustc: snapshot.toolchain().rustc_verbose_version().to_string(),
      host: snapshot.toolchain().host_target().to_string(),
    },
    products,
    targets: targets.to_vec(),
    features,
    completeness: completeness(facts)?,
    findings,
    reasons,
    fragments,
    cache: SurfaceCacheReport {
      compiler_fact_objects: object_ids,
    },
    metrics: SurfaceMetricsReport {
      acquisition: SurfaceAcquisitionMetricsReport {
        analysis_views: metrics.acquisition.analysis_views,
        cargo_views_executed: metrics.acquisition.cargo_views_executed,
        compiler_invocations: metrics.acquisition.compiler_invocations,
        diagnostic_cache_hits: metrics.acquisition.diagnostic_cache_hits,
        diagnostic_cache_misses: metrics.acquisition.diagnostic_cache_misses,
        fact_cache_hits: metrics.acquisition.fact_cache_hits,
        fact_cache_misses: metrics.acquisition.fact_cache_misses,
        fact_cache_store_failures: metrics.acquisition.fact_cache_store_failures,
        fact_cache_bypass_reasons: metrics.acquisition.fact_cache_bypass_reasons.clone(),
        fresh_fragment_bytes: metrics.acquisition.fresh_fragment_bytes,
        retained_fact_object_bytes: metrics.acquisition.retained_fact_object_bytes,
      },
      graph: SurfaceGraphMetricsReport {
        nodes: metrics.graph.nodes,
        edges: metrics.graph.edges,
        traversals: metrics.graph.traversals,
        edge_visits: metrics.graph.edge_visits,
      },
    },
    mutation,
  })
}

fn report_products(config: &SurfaceConfig, facts: &[ValidatedCompilerFactObject]) -> Vec<SurfaceProductReport> {
  if !config.product.is_empty() {
    return config
      .product
      .iter()
      .filter_map(|product| {
        product
          .bin
          .as_ref()
          .map(|target| (target, SurfaceProductKind::Binary))
          .or_else(|| product.lib.as_ref().map(|target| (target, SurfaceProductKind::Library)))
          .map(|(target, kind)| SurfaceProductReport {
            package: product.package.clone(),
            target: target.clone(),
            kind,
            implicit: false,
            reason: product.reason.clone(),
          })
      })
      .collect();
  }
  facts
    .iter()
    .filter_map(|fact| {
      let object = fact.object();
      (object.unit.domain == CompilerFactDomain::Production
        && object.unit.target_kind == CompilerFactTargetKind::Binary)
        .then(|| SurfaceProductReport {
          package: object.unit.package.name.clone(),
          target: object.unit.cargo_target.clone(),
          kind: SurfaceProductKind::Binary,
          implicit: true,
          reason: "workspace binary implicit root".to_string(),
        })
    })
    .collect::<BTreeSet<_>>()
    .into_iter()
    .collect()
}

fn completeness(facts: &[ValidatedCompilerFactObject]) -> RailResult<SurfaceCompleteness> {
  let sum = |value: fn(&ValidatedCompilerFactObject) -> u64| {
    facts.iter().try_fold(0_u64, |total, fact| {
      total
        .checked_add(value(fact))
        .ok_or_else(|| RailError::message("surface completeness count overflow"))
    })
  };
  let mut retention_reasons = BTreeMap::<String, u64>::new();
  for fact in facts {
    let object = fact.object();
    for retention in &object.retentions {
      let count = retention_reasons
        .entry(retention_reason_code(object, &retention.reason))
        .or_default();
      *count = count
        .checked_add(1)
        .ok_or_else(|| RailError::message("surface retention reason count overflow"))?;
    }
  }
  Ok(SurfaceCompleteness {
    complete: facts.iter().all(|fact| fact.object().completion.complete),
    fragments: u64::try_from(facts.len()).map_err(|_| RailError::message("surface fragment count overflow"))?,
    items: sum(|fact| fact.object().completion.items)?,
    edges: sum(|fact| fact.object().completion.edges)?,
    entry_points: sum(|fact| fact.object().completion.entry_points)?,
    retentions: sum(|fact| fact.object().completion.retentions)?,
    retention_reasons,
  })
}

fn used_reasons(findings: &[SurfaceReportFinding]) -> Vec<SurfaceReason> {
  const REASONS: &[(&str, &str)] = &[
    (
      "all-compiled-uses-within-defining-module",
      "Every compiled use fits inside the defining module.",
    ),
    (
      "all-compiled-uses-within-parent-module",
      "Every compiled use fits inside the parent module.",
    ),
    (
      "non-production-live",
      "A test, example, benchmark, doctest, build script, or development consumer reaches the declaration.",
    ),
    (
      "not-non-production-live",
      "No non-production root reaches the declaration.",
    ),
    (
      "not-production-live",
      "No configured production root reaches the declaration.",
    ),
    (
      "not-required-public",
      "No cross-crate or public-interface path requires public visibility.",
    ),
    (
      "production-live",
      "A configured production root reaches the declaration.",
    ),
  ];
  let used = findings
    .iter()
    .flat_map(|finding| finding.finding.reasons.iter().copied())
    .collect::<BTreeSet<_>>();
  REASONS
    .iter()
    .filter(|(code, _)| used.contains(code))
    .map(|(code, description)| SurfaceReason { code, description })
    .collect()
}

fn render_report(
  report: &SurfaceReport,
  format: SurfaceOutputFormat,
  explain: bool,
  mode: &'static str,
  exit_code: i32,
) -> RailResult<String> {
  match format {
    SurfaceOutputFormat::Text => Ok(render_text(report, explain)),
    SurfaceOutputFormat::Json => {
      let payload = serde_json::to_value(report)?;
      let result = if exit_code == 0 { "clean" } else { "findings" };
      let envelope = crate::output::machine_json_envelope("surface", mode, result, exit_code, payload);
      serde_json::to_string_pretty(&envelope).map_err(Into::into)
    }
    SurfaceOutputFormat::GitHub => {
      let payload = serde_json::to_value(report)?;
      let result = if exit_code == 0 { "clean" } else { "findings" };
      let envelope = crate::output::machine_json_envelope("surface", mode, result, exit_code, payload);
      let report_json = serde_json::to_string(&envelope)?;
      Ok(format!(
        "surface={}\nfinding_count={}\nsurface_report_json={}\n",
        exit_code != 0,
        report.findings.len(),
        report_json
      ))
    }
  }
}

fn render_text(report: &SurfaceReport, explain: bool) -> String {
  let mut output = String::new();
  if report.findings.is_empty() {
    output.push_str("surface: clean\n");
  } else {
    let _ = writeln!(output, "surface: {} finding(s)", report.findings.len());
  }
  let _ = writeln!(output, "snapshot: {}", report.snapshot.identity);
  let _ = writeln!(output, "compiler fragments: {}", report.completeness.fragments);
  if explain && !report.completeness.retention_reasons.is_empty() {
    let reasons = report
      .completeness
      .retention_reasons
      .iter()
      .map(|(reason, count)| format!("{reason}={count}"))
      .collect::<Vec<_>>()
      .join(", ");
    let _ = writeln!(output, "conservative retentions: {reasons}");
  }
  for report_finding in &report.findings {
    let finding = &report_finding.finding;
    let package = finding.packages.first().map(String::as_str).unwrap_or("unknown");
    let path = finding
      .diagnostic_paths
      .first()
      .map(String::as_str)
      .unwrap_or(&finding.name);
    let _ = writeln!(
      output,
      "{} [{}] {} {} at {}:{}",
      finding.kind.as_str(),
      lint_level_name(report_finding.level),
      package,
      path,
      finding.source,
      finding.visibility_start
    );
    if explain {
      let _ = writeln!(output, "  reasons: {}", finding.reasons.join(", "));
      if let Some(reason) = &report_finding.policy_reason {
        let _ = writeln!(output, "  policy: {reason}");
      }
    }
  }
  output
}

fn lint_level_name(level: SurfaceLintLevel) -> &'static str {
  match level {
    SurfaceLintLevel::Allow => "allow",
    SurfaceLintLevel::Warn => "warn",
    SurfaceLintLevel::Deny => "deny",
    SurfaceLintLevel::Expect => "expect",
  }
}

fn write_output(content: &str, output: Option<&Path>) -> RailResult<()> {
  match output {
    Some(path) => {
      let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(|error| RailError::message(format!("failed to open '{}': {error}", path.display())))?;
      writeln!(file, "{content}")
        .map_err(|error| RailError::message(format!("failed to write '{}': {error}", path.display())))?;
      crate::progress!("output: {}", path.display());
    }
    None => println!("{content}"),
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::surface::SurfaceFindingKind;

  fn finding() -> SurfaceFinding {
    SurfaceFinding {
      identity: "surface-finding-v1-sha256-test".to_string(),
      item_identity: "surface-item-v1-sha256-test".to_string(),
      kind: SurfaceFindingKind::UnnecessaryPublic,
      name: "legacy_entry".to_string(),
      item_kind: "function",
      packages: vec!["app-core".to_string()],
      diagnostic_paths: vec!["app_core::migration::legacy_entry".to_string()],
      source: "crates/app-core/src/migration.rs".to_string(),
      source_generated: false,
      declaration_start: 20,
      declaration_end: 50,
      visibility_start: 20,
      visibility_end: 23,
      replacement: Some("pub(crate)"),
      production_live: true,
      non_production_live: false,
      required_public: false,
      reasons: vec!["production-live", "not-required-public"],
    }
  }

  fn analysis() -> SurfaceAnalysis {
    SurfaceAnalysis {
      items: vec![SurfaceItemAnalysis {
        identity: "surface-item-v1-sha256-test".to_string(),
        item_kind: "function",
        packages: vec!["app-core".to_string()],
        diagnostic_paths: vec!["app_core::migration::legacy_entry".to_string()],
        source: "crates/app-core/src/migration.rs".to_string(),
        source_generated: false,
        production_live: true,
        non_production_live: false,
        required_public: false,
        retained: false,
      }],
      findings: vec![finding()],
      metrics: SurfaceGraphMetrics::default(),
    }
  }

  #[test]
  fn exact_expect_override_suppresses_only_the_named_finding() {
    let mut config = SurfaceConfig::default();
    config.r#override.push(SurfaceOverride {
      lint: "unnecessary-public".to_string(),
      package: "app-core".to_string(),
      item: "migration::legacy_entry".to_string(),
      kind: "function".to_string(),
      level: SurfaceLintLevel::Expect,
      reason: "migration compatibility".to_string(),
    });
    assert!(
      apply_diagnostic_policy(&analysis(), &config)
        .expect("policy should apply")
        .is_empty()
    );

    config.r#override[0].item = "migration::gone".to_string();
    assert!(
      apply_diagnostic_policy(&analysis(), &config)
        .expect_err("missing expected finding should fail")
        .to_string()
        .contains("disappeared")
    );
  }

  #[test]
  fn overlapping_qualified_and_unqualified_overrides_are_rejected() {
    let mut config = SurfaceConfig::default();
    for item in ["app_core::migration::legacy_entry", "migration::legacy_entry"] {
      config.r#override.push(SurfaceOverride {
        lint: "unnecessary-public".to_string(),
        package: "app-core".to_string(),
        item: item.to_string(),
        kind: "function".to_string(),
        level: SurfaceLintLevel::Allow,
        reason: "one exact policy must own the finding".to_string(),
      });
    }

    assert!(
      apply_diagnostic_policy(&analysis(), &config)
        .expect_err("overlapping overrides are ambiguous")
        .to_string()
        .contains("multiple item overrides")
    );
  }

  #[test]
  fn expected_exclusion_tracks_scope_not_only_current_findings() {
    let mut config = SurfaceConfig::default();
    config.exclude.push(SurfaceExclude {
      package: "app-core".to_string(),
      module: Some("migration".to_string()),
      file: None,
      level: SurfaceLintLevel::Expect,
      reason: "generated migration surface".to_string(),
    });
    assert!(
      apply_diagnostic_policy(&analysis(), &config)
        .expect("scope should exist")
        .is_empty()
    );

    let mut no_findings = analysis();
    no_findings.findings.clear();
    assert!(
      apply_diagnostic_policy(&no_findings, &config)
        .expect("the excluded scope still exists")
        .is_empty()
    );
  }

  #[test]
  fn visibility_edits_replace_only_exact_non_overlapping_ranges() {
    let source = b"pub fn first() {}\nlet untouched = 1;\npub(crate) fn second() {}\n";
    let mut first = finding();
    first.identity = "first".to_string();
    first.item_identity = "first-item".to_string();
    first.declaration_start = 0;
    first.declaration_end = 17;
    first.visibility_start = 0;
    first.visibility_end = 3;

    let second_start = source
      .windows(b"pub(crate)".len())
      .position(|window| window == b"pub(crate)")
      .expect("second visibility should exist");
    let mut second = finding();
    second.identity = "second".to_string();
    second.item_identity = "second-item".to_string();
    second.kind = SurfaceFindingKind::UnnecessaryCrateVisibility;
    second.declaration_start = second_start as u64;
    second.declaration_end = source.len() as u64;
    second.visibility_start = second_start as u64;
    second.visibility_end = (second_start + b"pub(crate)".len()) as u64;
    second.replacement = Some("pub(super)");

    let (updated, edits) = apply_visibility_edits(source, &[&first, &second]).expect("edits should be exact");
    assert_eq!(
      std::str::from_utf8(&updated).expect("updated source should remain UTF-8"),
      "pub(crate) fn first() {}\nlet untouched = 1;\npub(super) fn second() {}\n"
    );
    assert_eq!(edits.len(), 2);
  }

  #[test]
  fn visibility_edits_reject_shared_spans_and_unexpected_source_bytes() {
    let source = b"pub fn item() {}\n";
    let mut first = finding();
    first.identity = "first".to_string();
    first.declaration_start = 0;
    first.declaration_end = source.len() as u64;
    first.visibility_start = 0;
    first.visibility_end = 3;
    let mut second = first.clone();
    second.identity = "second".to_string();
    assert!(
      apply_visibility_edits(source, &[&first, &second])
        .expect_err("shared spans are ambiguous")
        .to_string()
        .contains("overlap")
    );

    first.visibility_end = 6;
    assert!(
      apply_visibility_edits(source, &[&first])
        .expect_err("unexpected original visibility must fail")
        .to_string()
        .contains("expected a different original visibility")
    );
  }

  #[test]
  fn private_edit_owns_one_separator_and_accepts_spaced_restricted_syntax() {
    let source = b"  pub ( super ) fn item() {}\n";
    let visibility_start = source
      .windows(b"pub ( super )".len())
      .position(|window| window == b"pub ( super )")
      .expect("restricted visibility should exist");
    let mut item = finding();
    item.kind = SurfaceFindingKind::UnnecessaryRestrictedVisibility;
    item.declaration_start = visibility_start as u64;
    item.declaration_end = source.len() as u64;
    item.visibility_start = visibility_start as u64;
    item.visibility_end = (visibility_start + b"pub ( super )".len()) as u64;
    item.replacement = Some("private");

    let (updated, edits) = apply_visibility_edits(source, &[&item]).expect("private edit should be exact");
    assert_eq!(updated, b"  fn item() {}\n");
    assert_eq!(edits[0].visibility_end + 1, edits[0].edit_end);
  }

  #[test]
  fn rollback_restores_only_bytes_still_owned_by_the_mutation() {
    let root = tempfile::tempdir().expect("surface rollback root");
    let path = RepositoryPath::new(Path::new("src/lib.rs")).expect("repository path");
    std::fs::create_dir_all(root.path().join("src")).expect("source directory");
    let update = SurfaceFileUpdate {
      path,
      before: b"pub fn item() {}\n".to_vec(),
      after: b"fn item() {}\n".to_vec(),
    };

    std::fs::write(root.path().join("src/lib.rs"), &update.after).expect("applied bytes");
    rollback_surface_updates(root.path(), std::slice::from_ref(&update)).expect("owned rollback");
    assert_eq!(
      std::fs::read(root.path().join("src/lib.rs")).expect("restored bytes"),
      update.before
    );

    let concurrent = b"fn item() { user_edit(); }\n";
    std::fs::write(root.path().join("src/lib.rs"), concurrent).expect("concurrent bytes");
    assert!(
      rollback_surface_updates(root.path(), &[update])
        .expect_err("concurrent bytes must not be overwritten")
        .to_string()
        .contains("preserving the newer bytes")
    );
    assert_eq!(
      std::fs::read(root.path().join("src/lib.rs")).expect("preserved bytes"),
      concurrent
    );
  }
}
