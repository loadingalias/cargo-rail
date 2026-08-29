//! Complete Rust declaration reachability and visibility analysis.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use cargo_metadata::{Package, TargetKind};
use serde::Serialize;

use crate::backup::{BackupManager, BackupMetadata};
use crate::cargo::{ManifestAnalyzer, TargetSpecificationIdentity};
use crate::commands::common::SurfaceOutputFormat;
use crate::compiler::collector::{
    CompilerAcquisitionProduct, CompilerAcquisitionRequest, validate_compiler_acquisition_resume,
};
use crate::compiler::driver::{CompilerFactDriverAuthority, CompilerFactDriverReadiness};
use crate::compiler::facts::{CompilerFactDomain, CompilerFactTargetKind, ValidatedCompilerFactObject};
use crate::compiler::{CompilerAnalysisMetrics, CompilerCacheIdentity, CompilerDiagnosticsCollector, FeatureSelection};
use crate::config::{
    SurfaceConfig, SurfaceConsumerScope, SurfaceCrateVisibility, SurfaceDoctestCoverage, SurfaceExclude,
    SurfaceLintLevel, SurfaceOverride,
};
use crate::error::{RailError, RailResult};
use crate::mutation::{
    ExpectedMutation, MutationAction, MutationEffect, MutationPlan, MutationRisk, MutationTrace, build_plan,
    validate_changed_paths_with_allowed_paths, validate_pre_apply, write_receipt,
};
use crate::source::{ContentDigest, RepositoryPath};
use crate::surface::{
    SurfaceAnalysis, SurfaceCompilerCrate, SurfaceFinding, SurfaceFindingKind, SurfaceGraph, SurfaceGraphMetrics,
    SurfaceItemAnalysis, SurfacePolicy, SurfaceProductKind, SurfaceProductRoot, SurfaceProductSelection,
    SurfaceRetentionSummary, retention_reason_code, target_selector_matches,
};
use crate::workspace::{CargoState, WorkspaceContext, WorkspaceSnapshot};

const SURFACE_CONTRACT_VERSION: u32 = 3;
const SURFACE_PREPARATION_CONTRACT_VERSION: u32 = 1;
const SURFACE_SCHEMA_JSON: &str = include_str!("../../schemas/surface-v3.schema.json");

/// Options for the `surface` domain command.
#[derive(Debug)]
pub struct SurfaceOptions {
    /// Prepare the exact-toolchain producer without analysis.
    pub prepare: bool,
    /// Run read-only analysis.
    pub check: bool,
    /// Apply exact visibility reductions.
    pub fix: bool,
    /// Prior partial acquisition manifest to revalidate before exact reuse.
    pub resume: Option<PathBuf>,
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
    /// Exact enabled finding classes to retain; empty retains every class.
    pub only: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SurfacePreparationReport {
    surface_preparation_contract_version: u32,
    snapshot: SurfaceSnapshotReport,
    toolchain: SurfaceToolchainReport,
    driver: SurfaceDriverReadinessReport,
}

#[derive(Debug, Serialize)]
struct SurfaceDriverReadinessReport {
    protocol: u32,
    identity: String,
    content_digest: String,
    compiler_library_digest: String,
    rustc_release: String,
    rustc_commit: String,
    rustc_host: String,
}

#[derive(Debug, Serialize)]
struct SurfaceReport {
    surface_contract_version: u32,
    snapshot: SurfaceSnapshotReport,
    config: SurfaceConfigReport,
    toolchain: SurfaceToolchainReport,
    authority: SurfaceAuthorityReport,
    products: Vec<SurfaceProductReport>,
    targets: Vec<String>,
    features: Vec<SurfaceFeatureView>,
    completeness: SurfaceCompleteness,
    retention: SurfaceRetentionSummary,
    findings: Vec<SurfaceReportFinding>,
    configuration_diagnostics: Vec<SurfaceConfigurationDiagnostic>,
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
    enabled: bool,
    consumer_scope: SurfaceConsumerScope,
    crate_visibility: SurfaceCrateVisibility,
    preserve_uniform_fields: bool,
    only: Vec<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    target_selector: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SurfaceAuthorityReport {
    audited_targets: Vec<SurfaceCompilerCrate>,
    open_targets: Vec<SurfaceCompilerCrate>,
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
    #[serde(skip)]
    line: Option<usize>,
    #[serde(skip)]
    column: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SurfaceConfigurationDiagnostic {
    code: &'static str,
    location: String,
    message: String,
    reason: String,
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
    configuration_diagnostics: Vec<SurfaceConfigurationDiagnostic>,
    authority: SurfaceAuthorityReport,
    retention: SurfaceRetentionSummary,
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
    let preparation_conflicts = options.prepare
        && (options.check
            || options.fix
            || options.resume.is_some()
            || options.dry_run
            || options.backup
            || options.explain
            || !options.only.is_empty());
    if preparation_conflicts || options.check && options.fix || !options.fix && (options.dry_run || options.backup) {
        return Err(RailError::with_help(
            "surface received incompatible operation flags",
            "use '--prepare' alone to prove readiness, '--check' for CI, or '--fix' for exact visibility repair",
        ));
    }

    if options.prepare {
        return prepare_surface(ctx, &options);
    }

    let config = ctx.config().map(|config| config.surface.clone()).unwrap_or_default();
    config.validate().map_err(RailError::Config)?;
    if let Some(manifest) = &options.resume {
        validate_compiler_acquisition_resume(
            ctx.workspace_root(),
            manifest,
            ctx.snapshot()?.configuration_fingerprint(),
        )?;
    }
    let mut initial = analyze_surface(ctx, &config, options.explain)?;
    ctx.validate_snapshot_unchanged()?;
    filter_findings(&mut initial.findings, &options.only);

    if !options.fix {
        let report = build_report(ctx.snapshot()?, &config, &options.only, initial, None)?;
        return emit_report(
            report,
            &options,
            if options.check { "check" } else { "inspect" },
            options.check,
        );
    }

    let (plan, updates) = plan_visibility_mutation(ctx, &initial.findings)?;
    if options.dry_run || !initial.configuration_diagnostics.is_empty() {
        let report = build_report(
            ctx.snapshot()?,
            &config,
            &options.only,
            initial,
            Some(SurfaceMutationReport {
                phase: "planned",
                plan,
                receipt: None,
                backup: None,
            }),
        )?;
        return emit_report(
            report,
            &options,
            if options.dry_run { "fix_dry_run" } else { "fix" },
            true,
        );
    }

    let applied = apply_visibility_mutation(ctx, plan, &updates, options.backup)?;
    let verification = (|| {
        surface_test_fault("recompilation")?;
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
        let mut verified = analyze_surface(&verified_context, &verified_config, options.explain)?;
        filter_findings(&mut verified.findings, &options.only);
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
        &options.only,
        verified,
        Some(mutation),
    )?;
    emit_report(report, &options, "fix", true)
}

fn prepare_surface(ctx: &WorkspaceContext, options: &SurfaceOptions) -> RailResult<()> {
    let snapshot = ctx.snapshot()?;
    let readiness = CompilerFactDriverAuthority::prepare_surface(snapshot)?;
    ctx.validate_snapshot_unchanged()?;
    let report = SurfacePreparationReport {
        surface_preparation_contract_version: SURFACE_PREPARATION_CONTRACT_VERSION,
        snapshot: SurfaceSnapshotReport {
            identity: snapshot.id().to_string(),
            configuration_fingerprint: snapshot.configuration_fingerprint().to_string(),
        },
        toolchain: SurfaceToolchainReport {
            fingerprint: snapshot.toolchain_fingerprint().to_string(),
            rustc: snapshot.toolchain().direct_rustc_verbose_version().to_string(),
            host: snapshot.toolchain().host_target().to_string(),
        },
        driver: surface_driver_readiness_report(readiness),
    };
    let rendered = render_preparation_report(&report, options.format)?;
    write_output(&rendered, options.output.as_deref())
}

fn surface_driver_readiness_report(readiness: CompilerFactDriverReadiness) -> SurfaceDriverReadinessReport {
    SurfaceDriverReadinessReport {
        protocol: readiness.protocol,
        identity: readiness.driver_identity,
        content_digest: readiness.driver_digest,
        compiler_library_digest: readiness.compiler_library_digest,
        rustc_release: readiness.rustc_release,
        rustc_commit: readiness.rustc_commit,
        rustc_host: readiness.rustc_host,
    }
}

fn render_preparation_report(report: &SurfacePreparationReport, format: SurfaceOutputFormat) -> RailResult<String> {
    match format {
        SurfaceOutputFormat::Text => Ok(format!(
            "surface: ready\nrustc: {} ({}, {})\ndriver: {}\nprotocol: {}",
            report.driver.rustc_release,
            report.driver.rustc_commit,
            report.driver.rustc_host,
            report.driver.identity,
            report.driver.protocol,
        )),
        SurfaceOutputFormat::Json => {
            let envelope =
                crate::output::machine_json_envelope("surface", "prepare", "ready", 0, serde_json::to_value(report)?);
            serde_json::to_string_pretty(&envelope).map_err(Into::into)
        }
        SurfaceOutputFormat::GitHub => {
            let envelope =
                crate::output::machine_json_envelope("surface", "prepare", "ready", 0, serde_json::to_value(report)?);
            Ok(format!(
                "surface_ready=true\nsurface_preparation_json={}\n",
                serde_json::to_string(&envelope)?,
            ))
        }
    }
}

fn filter_findings(findings: &mut Vec<SurfaceReportFinding>, only: &[String]) {
    if !only.is_empty() {
        let selected = only.iter().map(String::as_str).collect::<BTreeSet<_>>();
        findings.retain(|finding| selected.contains(finding.finding.kind.as_str()));
    }
}

fn analyze_surface(ctx: &WorkspaceContext, config: &SurfaceConfig, counterfactuals: bool) -> RailResult<SurfaceRun> {
    let snapshot = ctx.snapshot()?;
    validate_workspace_policy(ctx, snapshot, config)?;
    let workspace_packages = ctx.cargo().workspace_members();
    let manifests = ManifestAnalyzer::parse_snapshot(snapshot, &workspace_packages)?;
    let cache_identity = CompilerCacheIdentity::capture(snapshot)?;
    let workspace_targets = ctx.config().map(|config| config.targets.as_slice()).unwrap_or_default();
    let targets = selected_target_views(config, workspace_targets);
    let target_refs = targets.iter().map(String::as_str).collect::<Vec<_>>();
    let typed_packages = workspace_packages
        .iter()
        .map(|package| package.name.to_string())
        .collect::<BTreeSet<_>>();
    let doctest_packages = if !config.doctest.is_empty() {
        config
            .doctest
            .iter()
            .map(|entry| entry.package.clone())
            .collect::<BTreeSet<_>>()
    } else if config.doctest_coverage == SurfaceDoctestCoverage::Automatic {
        workspace_packages
            .iter()
            .filter(|package| package.targets.iter().any(|target| target.doctest))
            .map(|package| package.name.to_string())
            .collect::<BTreeSet<_>>()
    } else {
        BTreeSet::new()
    };
    let acquisition = CompilerAcquisitionRequest {
        snapshot_identity: snapshot.id().to_string(),
        configuration_fingerprint: snapshot.configuration_fingerprint().to_string(),
        products: acquisition_products(config, &workspace_packages),
    };
    let collector =
        CompilerDiagnosticsCollector::with_identity(ctx.workspace_root(), &manifests, target_refs, &cache_identity)
            .with_acquisition_manifest(acquisition);
    let feature_profiles = configured_feature_profiles(config);
    let evidence = if feature_profiles.is_empty() {
        collector.collect_with_typed_items(snapshot, &[], &typed_packages, &doctest_packages)?
    } else {
        collector.collect_with_typed_items_and_features(
            snapshot,
            &[],
            &typed_packages,
            &doctest_packages,
            &feature_profiles,
        )?
    };
    if evidence.compiler_facts.is_empty() {
        return Err(RailError::message(
            "surface analysis produced no authenticated compiler facts",
        ));
    }

    let graph = SurfaceGraph::from_compiler_facts(&evidence.compiler_facts)?;
    let closed_world_crates = closed_world_crates(&workspace_packages, &evidence.compiler_facts, config)?;
    let all_compiler_crates = evidence
        .compiler_facts
        .iter()
        .map(|fact| SurfaceCompilerCrate::from(fact.object()))
        .collect::<BTreeSet<_>>();
    let authority = SurfaceAuthorityReport {
        audited_targets: closed_world_crates.iter().cloned().collect(),
        open_targets: all_compiler_crates.difference(&closed_world_crates).cloned().collect(),
    };
    let products = configured_product_roots(config);
    let policy = SurfacePolicy::new(
        closed_world_crates,
        config.crate_visibility == SurfaceCrateVisibility::Allow,
    )
    .with_products(products)
    .preserving_uniform_fields(config.preserve_uniform_fields);
    let analysis = if counterfactuals {
        graph.analyze_with_counterfactuals(&policy)?
    } else {
        graph.analyze(&policy)?
    };
    let policy_result = apply_diagnostic_policy(&analysis, config)?;
    Ok(SurfaceRun {
        targets,
        facts: evidence.compiler_facts,
        findings: policy_result.findings,
        configuration_diagnostics: policy_result.configuration_diagnostics,
        authority,
        retention: analysis.retention,
        metrics: SurfaceRunMetrics {
            acquisition: evidence.metrics,
            graph: analysis.metrics,
        },
    })
}

fn acquisition_products(config: &SurfaceConfig, packages: &[&Package]) -> Vec<CompilerAcquisitionProduct> {
    if !config.product.is_empty() {
        return config
            .product
            .iter()
            .filter_map(|product| {
                product
                    .bin
                    .as_ref()
                    .map(|target| (target, "binary"))
                    .or_else(|| product.lib.as_ref().map(|target| (target, "library")))
                    .map(|(target, kind)| CompilerAcquisitionProduct {
                        package: product.package.clone(),
                        cargo_target: target.clone(),
                        kind: kind.to_string(),
                    })
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
    }
    packages
        .iter()
        .flat_map(|package| {
            package
                .targets
                .iter()
                .filter(|target| target.kind.contains(&TargetKind::Bin))
                .map(|target| CompilerAcquisitionProduct {
                    package: package.name.to_string(),
                    cargo_target: target.name.clone(),
                    kind: "binary".to_string(),
                })
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn configured_feature_profiles(config: &SurfaceConfig) -> Vec<FeatureSelection> {
    config
        .feature_profile
        .iter()
        .map(|profile| {
            if profile.all_features {
                FeatureSelection::AllFeatures
            } else if profile.no_default_features && profile.features.is_empty() {
                FeatureSelection::NoDefaultFeatures
            } else if profile.no_default_features {
                FeatureSelection::Selected(profile.features.clone())
            } else if profile.features.is_empty() {
                FeatureSelection::Default
            } else {
                FeatureSelection::DefaultWith(profile.features.clone())
            }
        })
        .collect()
}

fn emit_report(
    report: SurfaceReport,
    options: &SurfaceOptions,
    mode: &'static str,
    fail_on_findings: bool,
) -> RailResult<()> {
    let has_denied_findings = report
        .findings
        .iter()
        .any(|finding| finding.level == SurfaceLintLevel::Deny)
        || !report.configuration_diagnostics.is_empty();
    let exit_code = i32::from(fail_on_findings && has_denied_findings);
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
        let expected =
            ExpectedMutation::capture(git_root, repository_path.as_path().to_path_buf(), MutationEffect::Write);
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
            let visibility_start = finding.visibility_start.ok_or_else(|| {
                RailError::message(format!(
                    "surface finding '{}' has no writable visibility span",
                    finding.identity
                ))
            })?;
            let visibility_end = finding.visibility_end.ok_or_else(|| {
                RailError::message(format!(
                    "surface finding '{}' has no writable visibility span",
                    finding.identity
                ))
            })?;
            let start = usize::try_from(visibility_start)
                .map_err(|_| RailError::message("surface visibility start exceeds this platform"))?;
            let end = usize::try_from(visibility_end)
                .map_err(|_| RailError::message("surface visibility end exceeds this platform"))?;
            if start >= end
                || end > before.len()
                || visibility_start < finding.declaration_start
                || visibility_end > finding.declaration_end
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
                (SurfaceFindingKind::UnnecessaryRestrictedVisibility, Some("private"))
                    if restricted_visibility(&compact) || compact == "pub(crate)" =>
                {
                    ""
                }
                (SurfaceFindingKind::UnnecessaryCrateVisibility, Some("pub(super)")) if compact == "pub(crate)" => {
                    "pub(super)"
                }
                (SurfaceFindingKind::UnnecessaryCrateVisibility, Some("private")) if compact == "pub(crate)" => "",
                _ => {
                    return Err(RailError::message(format!(
                        "surface finding '{}' expected a different original visibility at {}..{}",
                        finding.identity, visibility_start, visibility_end
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
                visibility_start,
                visibility_end,
                edit_start: visibility_start,
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
        let start = usize::try_from(edit.edit_start)
            .map_err(|_| RailError::message("surface edit start exceeds this platform"))?;
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
                crate::utils::path_relative_to(ctx.workspace_root(), &git_root.join(update.path.as_path()))
                    .map_err(Into::into)
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
        let fault_point = if index == 0 { "first-write" } else { "write" };
        let write =
            surface_test_fault(fault_point).and_then(|()| crate::utils::write_file_atomic(&absolute, &update.after));
        if let Err(error) = write {
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
        if index == 0
            && updates.len() > 1
            && let Err(error) = surface_test_fault("partial-write")
        {
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

    if let Err(error) = surface_test_fault("post-write-validation")
        .and_then(|()| verify_surface_updates(git_root, updates))
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
    let receipt = surface_test_fault("receipt-write").and_then(|()| {
        write_receipt(
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
        )
    });
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

fn surface_test_fault(point: &str) -> RailResult<()> {
    if std::env::var("CARGO_RAIL_SURFACE_FAIL_AT").as_deref() == Ok(point) {
        Err(RailError::message(format!("injected surface failure at {point}")))
    } else {
        Ok(())
    }
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
        let metadata = std::fs::symlink_metadata(&absolute).map_err(|error| {
            RailError::message(format!("failed to inspect surface source '{}': {error}", update.path))
        })?;
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
    path.strip_prefix(root)
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
                .filter_map(|policy| policy.package.as_deref().map(|package| ("surface.override", package))),
        )
        .chain(
            config
                .exclude
                .iter()
                .filter_map(|policy| policy.package.as_deref().map(|package| ("surface.exclude", package))),
        )
        .chain(
            config
                .doctest
                .iter()
                .map(|entry| ("surface.doctest", entry.package.as_str())),
        )
    {
        if !names.contains(package) {
            return Err(RailError::message(format!(
                "{field} names unknown workspace package '{package}'"
            )));
        }
    }

    let library_crates = packages
        .iter()
        .flat_map(|package| {
            package
                .targets
                .iter()
                .filter(|target| target.kind.iter().any(is_library_target))
                .map(|target| (package.name.as_str(), target.name.as_str()))
        })
        .collect::<Vec<_>>();
    for boundary in &config.external {
        let matches = library_crates
            .iter()
            .filter(|(_, crate_name)| *crate_name == boundary.crate_name)
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err(RailError::message(format!(
                "surface external crate '{}' identifies {} workspace library targets",
                boundary.crate_name,
                matches.len()
            )));
        }
    }

    for doctest in &config.doctest {
        let package = ctx
            .cargo()
            .get_package(&doctest.package)
            .ok_or_else(|| RailError::message(format!("surface doctest package '{}' disappeared", doctest.package)))?;
        if !package.targets.iter().any(|target| target.doctest) {
            return Err(RailError::message(format!(
                "surface doctest package '{}' has no doctest-enabled Cargo target",
                doctest.package
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
    let workspace_targets = ctx.config().map(|config| config.targets.as_slice()).unwrap_or_default();
    for target in config
        .targets
        .effective(workspace_targets)
        .into_iter()
        .filter(|target| target != "host")
    {
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

fn selected_target_views(config: &SurfaceConfig, workspace_targets: &[String]) -> Vec<String> {
    config
        .targets
        .effective(workspace_targets)
        .into_iter()
        .map(|target| {
            if target == "host" {
                "default".to_string()
            } else {
                target
            }
        })
        .collect()
}

fn closed_world_crates(
    packages: &[&Package],
    facts: &[ValidatedCompilerFactObject],
    config: &SurfaceConfig,
) -> RailResult<BTreeSet<SurfaceCompilerCrate>> {
    let publishable = packages
        .iter()
        .map(|package| (package.name.as_str(), CargoState::is_package_publishable(package)))
        .collect::<BTreeMap<_, _>>();
    let products = configured_product_roots(config);
    let library_products_only = !products.is_empty()
        && products
            .iter()
            .all(|selection| selection.root.kind == SurfaceProductKind::Library);
    let selected_libraries = products
        .iter()
        .filter(|selection| selection.root.kind == SurfaceProductKind::Library)
        .map(|selection| (selection.root.package.as_str(), selection.root.target.as_str()))
        .collect::<BTreeSet<_>>();
    let selected_binaries = products
        .iter()
        .filter(|selection| selection.root.kind == SurfaceProductKind::Binary)
        .map(|selection| (selection.root.package.as_str(), selection.root.target.as_str()))
        .collect::<BTreeSet<_>>();
    let external = config
        .external
        .iter()
        .map(|boundary| boundary.crate_name.as_str())
        .collect::<BTreeSet<_>>();
    let mut closed = BTreeSet::new();
    for fact in facts {
        let object = fact.object();
        let Some(package_publishable) = publishable.get(object.unit.package.name.as_str()).copied() else {
            continue;
        };
        let compiler_crate = SurfaceCompilerCrate::from(object);
        if external.contains(compiler_crate.crate_name.as_str()) {
            continue;
        }
        let product = (
            compiler_crate.package.name.as_str(),
            compiler_crate.cargo_target.as_str(),
        );
        let authorized = if compiler_crate.target_kind == CompilerFactTargetKind::Binary {
            if products.is_empty() {
                true
            } else {
                selected_binaries.contains(&product)
            }
        } else if config.consumer_scope != SurfaceConsumerScope::Workspace {
            false
        } else if library_products_only {
            !package_publishable
                && compiler_crate.target_kind == CompilerFactTargetKind::Library
                && selected_libraries.contains(&product)
        } else {
            !package_publishable
                && matches!(
                    compiler_crate.target_kind,
                    CompilerFactTargetKind::Library
                        | CompilerFactTargetKind::ProcMacro
                        | CompilerFactTargetKind::BuildScript
                )
        };
        if authorized {
            closed.insert(compiler_crate);
        }
    }
    Ok(closed)
}

fn configured_product_roots(config: &SurfaceConfig) -> Vec<SurfaceProductSelection> {
    config
        .product
        .iter()
        .filter_map(|product| {
            product
                .bin
                .as_ref()
                .map(|target| (target, SurfaceProductKind::Binary))
                .or_else(|| product.lib.as_ref().map(|target| (target, SurfaceProductKind::Library)))
                .map(|(target, kind)| SurfaceProductSelection {
                    root: SurfaceProductRoot {
                        package: product.package.clone(),
                        target: target.clone(),
                        kind,
                    },
                    target: product.target.clone(),
                })
        })
        .collect()
}

struct SurfacePolicyResult {
    findings: Vec<SurfaceReportFinding>,
    configuration_diagnostics: Vec<SurfaceConfigurationDiagnostic>,
}

fn apply_diagnostic_policy(analysis: &SurfaceAnalysis, config: &SurfaceConfig) -> RailResult<SurfacePolicyResult> {
    let mut configuration_diagnostics = Vec::new();
    let mut active_overrides = Vec::new();
    for (index, policy) in config.r#override.iter().enumerate() {
        let mut matching_items = BTreeSet::new();
        for item in &analysis.items {
            if override_identifies_item(item, policy)? {
                matching_items.insert(item.identity.as_str());
            }
        }
        let location = format!("surface.override[{index}]");
        if matching_items.is_empty() {
            configuration_diagnostics.push(SurfaceConfigurationDiagnostic {
                code: "unknown-item",
                location,
                message: format!(
                    "surface override does not identify a compiled item '{}':{}",
                    policy.item, policy.lint
                ),
                reason: policy.reason.clone(),
            });
            continue;
        }
        if matching_items.len() > 1 {
            configuration_diagnostics.push(SurfaceConfigurationDiagnostic {
                code: "ambiguous-item",
                location,
                message: format!(
                    "surface override identifies {} compiled items; add an exact declaration kind",
                    matching_items.len()
                ),
                reason: policy.reason.clone(),
            });
            continue;
        }
        active_overrides.push((index, policy));
        if policy.level == SurfaceLintLevel::Expect {
            let mut fulfilled = false;
            for finding in &analysis.findings {
                if override_matches(finding, policy)? {
                    fulfilled = true;
                    break;
                }
            }
            if !fulfilled {
                configuration_diagnostics.push(SurfaceConfigurationDiagnostic {
                    code: "unfulfilled-expectation",
                    location,
                    message: format!(
                        "expected surface finding '{}:{}' no longer exists",
                        policy.lint, policy.item
                    ),
                    reason: policy.reason.clone(),
                });
            }
        }
    }

    for (index, policy) in config.exclude.iter().enumerate() {
        if policy.level != SurfaceLintLevel::Expect {
            continue;
        }
        let mut suppressed = false;
        for finding in &analysis.findings {
            if exclusion_matches_finding(finding, policy)? {
                suppressed = true;
                break;
            }
        }
        if !suppressed {
            configuration_diagnostics.push(SurfaceConfigurationDiagnostic {
                code: "unfulfilled-expectation",
                location: format!("surface.exclude[{index}]"),
                message: "expected surface exclusion no longer suppresses a finding".to_string(),
                reason: policy.reason.clone(),
            });
        }
    }

    let mut enabled = Vec::new();
    for finding in &analysis.findings {
        let mut exact = Vec::new();
        for (index, policy) in &active_overrides {
            if override_matches(finding, policy)? {
                exact.push((*index, *policy));
            }
        }
        if exact.len() > 1 {
            configuration_diagnostics.push(SurfaceConfigurationDiagnostic {
                code: "ambiguous-policy",
                location: exact
                    .iter()
                    .map(|(index, _)| format!("surface.override[{index}]"))
                    .collect::<Vec<_>>()
                    .join(","),
                message: format!("surface finding '{}' matches multiple item overrides", finding.identity),
                reason: exact
                    .iter()
                    .map(|(_, policy)| policy.reason.as_str())
                    .collect::<Vec<_>>()
                    .join("; "),
            });
            continue;
        }
        let (level, reason) = if let Some((_, policy)) = exact.first() {
            (policy.level, Some(policy.reason.clone()))
        } else {
            let mut matched_exclusion = None;
            for policy in &config.exclude {
                if exclusion_matches_finding(finding, policy)? {
                    matched_exclusion = Some(policy);
                }
            }
            matched_exclusion.map_or_else(
                || (default_lint_level(finding.kind, config), None),
                |policy| (policy.level, Some(policy.reason.clone())),
            )
        };
        if matches!(level, SurfaceLintLevel::Warn | SurfaceLintLevel::Deny) {
            enabled.push(SurfaceReportFinding {
                finding: finding.clone(),
                level,
                policy_reason: reason,
                line: None,
                column: None,
            });
        }
    }
    Ok(SurfacePolicyResult {
        findings: enabled,
        configuration_diagnostics,
    })
}

fn default_lint_level(kind: SurfaceFindingKind, config: &SurfaceConfig) -> SurfaceLintLevel {
    let mut level = if kind == SurfaceFindingKind::UnnecessaryCrateVisibility {
        SurfaceLintLevel::Allow
    } else {
        SurfaceLintLevel::Deny
    };
    for directive in &config.lint {
        let selected = directive.selector == kind.as_str()
            || directive.selector == "warnings" && kind != SurfaceFindingKind::UnnecessaryCrateVisibility;
        if selected {
            level = directive.level;
        }
    }
    level
}

fn override_identifies_item(item: &SurfaceItemAnalysis, policy: &SurfaceOverride) -> RailResult<bool> {
    Ok(policy_owner_matches(
        &item.packages,
        &item.compiler_crates,
        policy.package.as_deref(),
        policy.crate_name.as_deref(),
    ) && policy.kind.as_deref().is_none_or(|kind| item.item_kind == kind)
        && item
            .diagnostic_paths
            .iter()
            .any(|path| exact_diagnostic_path(path, &policy.item))
        && target_observations_match(policy.target.as_deref(), &item.target_observations)?)
}

fn override_matches(finding: &SurfaceFinding, policy: &SurfaceOverride) -> RailResult<bool> {
    Ok(finding.kind.as_str() == policy.lint
        && policy_owner_matches(
            &finding.packages,
            &finding.compiler_crates,
            policy.package.as_deref(),
            policy.crate_name.as_deref(),
        )
        && policy.kind.as_deref().is_none_or(|kind| finding.item_kind == kind)
        && finding
            .diagnostic_paths
            .iter()
            .any(|path| exact_diagnostic_path(path, &policy.item))
        && target_observations_match(policy.target.as_deref(), &finding.target_observations)?)
}

fn exclusion_matches_finding(finding: &SurfaceFinding, policy: &SurfaceExclude) -> RailResult<bool> {
    Ok(policy_owner_matches(
        &finding.packages,
        &finding.compiler_crates,
        policy.package.as_deref(),
        policy.crate_name.as_deref(),
    ) && exclusion_matches(&finding.source, &finding.diagnostic_paths, policy)
        && target_observations_match(policy.target.as_deref(), &finding.target_observations)?)
}

fn policy_owner_matches(
    packages: &[String],
    compiler_crates: &[SurfaceCompilerCrate],
    package: Option<&str>,
    crate_name: Option<&str>,
) -> bool {
    package.is_some_and(|expected| packages.iter().any(|package| package == expected))
        || crate_name.is_some_and(|expected| {
            compiler_crates
                .iter()
                .any(|compiler_crate| compiler_crate.crate_name == expected)
        })
}

fn target_observations_match(
    selector: Option<&str>,
    observations: &[crate::surface::SurfaceTargetObservation],
) -> RailResult<bool> {
    for observation in observations {
        if target_selector_matches(selector, observation)? {
            return Ok(true);
        }
    }
    Ok(false)
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
    only: &[String],
    run: SurfaceRun,
    mutation: Option<SurfaceMutationReport>,
) -> RailResult<SurfaceReport> {
    let SurfaceRun {
        targets,
        facts,
        findings,
        configuration_diagnostics,
        authority,
        retention,
        metrics,
    } = run;
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
    let products = report_products(config, &facts);
    let config_bytes = serde_json::to_vec(config)?;
    let findings = locate_and_sort_findings(snapshot, findings)?;
    let reasons = used_reasons(&findings);

    Ok(SurfaceReport {
        surface_contract_version: SURFACE_CONTRACT_VERSION,
        snapshot: SurfaceSnapshotReport {
            identity: snapshot.id().to_string(),
            configuration_fingerprint: snapshot.configuration_fingerprint().to_string(),
        },
        config: SurfaceConfigReport {
            identity: format!("sha256:{}", ContentDigest::sha256(&config_bytes)),
            enabled: config.enabled,
            consumer_scope: config.consumer_scope,
            crate_visibility: config.crate_visibility,
            preserve_uniform_fields: config.preserve_uniform_fields,
            only: only.iter().cloned().collect::<BTreeSet<_>>().into_iter().collect(),
        },
        toolchain: SurfaceToolchainReport {
            fingerprint: snapshot.toolchain_fingerprint().to_string(),
            rustc: snapshot.toolchain().rustc_verbose_version().to_string(),
            host: snapshot.toolchain().host_target().to_string(),
        },
        authority,
        products,
        targets,
        features,
        completeness: completeness(&facts)?,
        retention,
        findings,
        configuration_diagnostics,
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
                        target_selector: product.target.clone(),
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
                    target_selector: None,
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

fn locate_and_sort_findings(
    snapshot: &WorkspaceSnapshot,
    mut findings: Vec<SurfaceReportFinding>,
) -> RailResult<Vec<SurfaceReportFinding>> {
    let mut source_cache = BTreeMap::<String, Vec<u8>>::new();
    for report_finding in &mut findings {
        let finding = &report_finding.finding;
        if finding.source_generated {
            continue;
        }
        if !source_cache.contains_key(&finding.source) {
            let path = RepositoryPath::new(Path::new(&finding.source))?;
            let Some(bytes) = snapshot.read_source_file(&path)? else {
                continue;
            };
            source_cache.insert(finding.source.clone(), bytes);
        }
        let bytes = source_cache
            .get(&finding.source)
            .expect("captured Surface source was inserted");
        let offset = finding.visibility_start.unwrap_or(finding.declaration_start);
        let (line, column) = source_location(bytes, offset)?;
        report_finding.line = Some(line);
        report_finding.column = Some(column);
    }
    findings.sort_by(|left, right| {
        left.finding
            .source
            .cmp(&right.finding.source)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.column.cmp(&right.column))
            .then_with(|| lint_level_name(left.level).cmp(lint_level_name(right.level)))
            .then_with(|| left.finding.kind.as_str().cmp(right.finding.kind.as_str()))
            .then_with(|| left.finding.identity.cmp(&right.finding.identity))
    });
    Ok(findings)
}

fn source_location(bytes: &[u8], offset: u64) -> RailResult<(usize, usize)> {
    let offset = usize::try_from(offset)
        .ok()
        .filter(|offset| *offset <= bytes.len())
        .ok_or_else(|| RailError::message("surface diagnostic byte offset exceeds captured source"))?;
    let prefix = std::str::from_utf8(&bytes[..offset])
        .map_err(|_| RailError::message("surface diagnostic byte offset is not a UTF-8 boundary"))?;
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix
        .rsplit_once('\n')
        .map_or(prefix, |(_, tail)| tail)
        .chars()
        .count()
        + 1;
    Ok((line, column))
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
            let result = if report.findings.is_empty() && report.configuration_diagnostics.is_empty() {
                "clean"
            } else {
                "findings"
            };
            let envelope = crate::output::machine_json_envelope("surface", mode, result, exit_code, payload);
            serde_json::to_string_pretty(&envelope).map_err(Into::into)
        }
        SurfaceOutputFormat::GitHub => {
            let payload = serde_json::to_value(report)?;
            let result = if report.findings.is_empty() && report.configuration_diagnostics.is_empty() {
                "clean"
            } else {
                "findings"
            };
            let envelope = crate::output::machine_json_envelope("surface", mode, result, exit_code, payload);
            let report_json = serde_json::to_string(&envelope)?;
            Ok(format!(
                "surface={}\nfinding_count={}\nconfiguration_diagnostic_count={}\nsurface_report_json={}\n",
                result == "findings",
                report.findings.len(),
                report.configuration_diagnostics.len(),
                report_json
            ))
        }
    }
}

fn render_text(report: &SurfaceReport, explain: bool) -> String {
    let mut output = String::new();
    if report.findings.is_empty() && report.configuration_diagnostics.is_empty() {
        output.push_str("Surface is clean.\n");
    } else {
        output.push_str("Surface: ");
        output.push_str(&report.findings.len().to_string());
        output.push_str(" finding(s), ");
        output.push_str(&report.configuration_diagnostics.len().to_string());
        output.push_str(" configuration diagnostic(s)\n");
    }
    if crate::output::is_verbose() {
        output.push_str("Snapshot: ");
        output.push_str(&report.snapshot.identity);
        output.push_str("\nCompiler fragments: ");
        output.push_str(&report.completeness.fragments.to_string());
        output.push('\n');
    }
    if explain {
        render_retention_explanation(&mut output, &report.retention);
    }
    for report_finding in &report.findings {
        let finding = &report_finding.finding;
        let item = finding
            .diagnostic_paths
            .first()
            .map(String::as_str)
            .unwrap_or(&finding.name);
        output.push_str(&finding.source);
        output.push(':');
        output.push_str(
            &report_finding
                .line
                .map_or_else(|| "?".to_string(), |line| line.to_string()),
        );
        output.push(':');
        output.push_str(
            &report_finding
                .column
                .map_or_else(|| "?".to_string(), |column| column.to_string()),
        );
        output.push_str(": ");
        output.push_str(lint_level_name(report_finding.level));
        output.push('[');
        output.push_str(finding.kind.as_str());
        output.push_str("]: ");
        output.push_str(finding.item_kind);
        output.push(' ');
        output.push_str(item);
        if let Some(replacement) = finding.replacement {
            output.push_str("; proposed visibility: ");
            output.push_str(if replacement.is_empty() { "private" } else { replacement });
        }
        output.push('\n');
        if explain {
            output.push_str("  reasons: ");
            output.push_str(&finding.reasons.join(", "));
            output.push('\n');
            if let Some(reason) = &report_finding.policy_reason {
                output.push_str("  policy: ");
                output.push_str(reason);
                output.push('\n');
            }
        }
    }
    for diagnostic in &report.configuration_diagnostics {
        output.push_str("configuration ");
        output.push_str(diagnostic.code);
        output.push_str(" at ");
        output.push_str(&diagnostic.location);
        output.push_str(": ");
        output.push_str(&diagnostic.message);
        output.push('\n');
        if explain {
            output.push_str("  reason: ");
            output.push_str(&diagnostic.reason);
            output.push('\n');
        }
    }
    output
}

fn render_retention_explanation(output: &mut String, retention: &SurfaceRetentionSummary) {
    output.push_str("Conservative proof observations: ");
    output.push_str(&retention.conservative_observations.to_string());
    output.push_str(" retention row(s) across ");
    output.push_str(&retention.fragment_item_observations.to_string());
    output.push_str(" fragment item observation(s); ");
    output.push_str(&retention.unique_retained_items.to_string());
    output.push_str(" unique retained item(s) across ");
    output.push_str(&retention.merged_items.to_string());
    output.push_str(" merged graph item(s).\n");
    for reason in &retention.reasons {
        output.push_str("  ");
        output.push_str(&reason.code);
        output.push_str(": observations=");
        output.push_str(&reason.observations.to_string());
        output.push_str(", unique-items=");
        output.push_str(&reason.unique_items.to_string());
        output.push_str("; predicate: ");
        output.push_str(reason.predicate);
        output.push('\n');
        for representative in &reason.representatives {
            output.push_str("    example: ");
            output.push_str(&representative.source);
            output.push(':');
            output.push_str(&representative.declaration_start.to_string());
            output.push('-');
            output.push_str(&representative.declaration_end.to_string());
            output.push(' ');
            output.push_str(representative.item_kind);
            if let Some(path) = representative.diagnostic_paths.first() {
                output.push(' ');
                output.push_str(path);
            }
            output.push('\n');
        }
    }
    if let Some(counterfactual) = &retention.counterfactual {
        output.push_str("Counterfactual suppression (omit one reason; retain all alternatives): ");
        output.push_str(&counterfactual.suppressed_finding_observations.to_string());
        output.push_str(" finding observation(s), ");
        output.push_str(&counterfactual.traversals.to_string());
        output.push_str(" graph traversal(s), ");
        output.push_str(&counterfactual.edge_visits.to_string());
        output.push_str(" edge visit(s).\n");
        for reason in &counterfactual.reasons {
            output.push_str("  ");
            output.push_str(&reason.code);
            output.push_str(": suppressed-findings=");
            output.push_str(&reason.suppressed_findings.to_string());
            if !reason.finding_kinds.is_empty() {
                output.push_str(" (");
                output.push_str(
                    &reason
                        .finding_kinds
                        .iter()
                        .map(|(kind, count)| format!("{kind}={count}"))
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                output.push(')');
            }
            output.push('\n');
        }
    }
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
    use crate::compiler::fact_protocol::DRIVER_IDENTITY_PREFIX;
    use crate::compiler::facts::{CompilerFactPackage, CompilerFactRole};
    use crate::surface::{SurfaceFindingKind, SurfaceTargetObservation};

    fn preparation_report() -> SurfacePreparationReport {
        SurfacePreparationReport {
            surface_preparation_contract_version: SURFACE_PREPARATION_CONTRACT_VERSION,
            snapshot: SurfaceSnapshotReport {
                identity: "workspace-snapshot-v2-sha256:test".to_string(),
                configuration_fingerprint: "sha256:config".to_string(),
            },
            toolchain: SurfaceToolchainReport {
                fingerprint: "sha256:toolchain".to_string(),
                rustc: "rustc 1.99.0-nightly\ncommit-hash: 1111111111111111111111111111111111111111\nhost: aarch64-apple-darwin\nrelease: 1.99.0-nightly".to_string(),
                host: "aarch64-apple-darwin".to_string(),
            },
            driver: SurfaceDriverReadinessReport {
                protocol: 3,
                identity: format!("{DRIVER_IDENTITY_PREFIX}{}", "d".repeat(64)),
                content_digest: "sha256:driver".to_string(),
                compiler_library_digest: "sha256:library".to_string(),
                rustc_release: "1.99.0-nightly".to_string(),
                rustc_commit: "1111111111111111111111111111111111111111".to_string(),
                rustc_host: "aarch64-apple-darwin".to_string(),
            },
        }
    }

    #[test]
    fn preparation_projection_is_versioned_and_machine_safe() {
        let report = preparation_report();
        let json = render_preparation_report(&report, SurfaceOutputFormat::Json).expect("JSON preparation");
        let value: serde_json::Value = serde_json::from_str(&json).expect("preparation JSON");
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["command"], "surface");
        assert_eq!(value["mode"], "prepare");
        assert_eq!(value["result"], "ready");
        assert_eq!(value["exit_code"], 0);
        assert_eq!(value["surface_preparation_contract_version"], 1);
        assert_eq!(value["driver"]["protocol"], 3);
        assert_eq!(value["driver"]["rustc_release"], "1.99.0-nightly");

        let github = render_preparation_report(&report, SurfaceOutputFormat::GitHub).expect("GitHub preparation");
        assert!(github.starts_with("surface_ready=true\nsurface_preparation_json={"));
        assert!(
            !github.contains("\ncommit-hash:"),
            "embedded JSON must stay on one line"
        );
    }

    #[test]
    fn source_locations_count_unicode_scalars_instead_of_utf8_bytes() {
        let source = "fn α() { pub fn β() {} }\n";
        let offset = source.find("pub").expect("visibility offset") as u64;
        assert_eq!(source_location(source.as_bytes(), offset).unwrap(), (1, 10));
    }

    fn compiler_crate() -> SurfaceCompilerCrate {
        SurfaceCompilerCrate {
            package: CompilerFactPackage {
                name: "app-core".to_string(),
                version: "1.0.0".to_string(),
                source: None,
            },
            cargo_target: "app_core".to_string(),
            crate_name: "app_core".to_string(),
            target_kind: CompilerFactTargetKind::Library,
            role: CompilerFactRole::Target,
        }
    }

    fn target_observation() -> SurfaceTargetObservation {
        SurfaceTargetObservation {
            platform: "aarch64-apple-darwin".to_string(),
            cfg: vec!["target_os=\"macos\"".to_string()],
        }
    }

    fn finding() -> SurfaceFinding {
        SurfaceFinding {
            identity: "surface-finding-v1-sha256-test".to_string(),
            item_identity: "surface-item-v1-sha256-test".to_string(),
            kind: SurfaceFindingKind::UnnecessaryPublic,
            name: "legacy_entry".to_string(),
            item_kind: "function",
            packages: vec!["app-core".to_string()],
            compiler_crates: vec![compiler_crate()],
            target_observations: vec![target_observation()],
            diagnostic_paths: vec!["app_core::migration::legacy_entry".to_string()],
            source: "crates/app-core/src/migration.rs".to_string(),
            source_generated: false,
            declaration_start: 20,
            declaration_end: 50,
            visibility_start: Some(20),
            visibility_end: Some(23),
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
                compiler_crates: vec![compiler_crate()],
                target_observations: vec![target_observation()],
                diagnostic_paths: vec!["app_core::migration::legacy_entry".to_string()],
                source: "crates/app-core/src/migration.rs".to_string(),
                source_generated: false,
                production_live: true,
                non_production_live: false,
                required_public: false,
                retained: false,
            }],
            findings: vec![finding()],
            retention: SurfaceRetentionSummary {
                fragment_item_observations: 1,
                merged_items: 1,
                conservative_observations: 0,
                unique_retained_items: 0,
                representative_limit: 3,
                reasons: Vec::new(),
                counterfactual: None,
            },
            metrics: SurfaceGraphMetrics::default(),
        }
    }

    #[test]
    fn retention_explanation_renders_zero_evidence() {
        let retention = SurfaceRetentionSummary {
            fragment_item_observations: 4,
            merged_items: 2,
            conservative_observations: 0,
            unique_retained_items: 0,
            representative_limit: 3,
            reasons: Vec::new(),
            counterfactual: Some(crate::surface::SurfaceRetentionCounterfactualSummary {
                basis: "omit-one-reason-before-diagnostic-policy",
                traversals: 0,
                edge_visits: 0,
                suppressed_finding_observations: 0,
                reasons: Vec::new(),
            }),
        };
        let mut output = String::new();
        render_retention_explanation(&mut output, &retention);
        assert!(output.contains(
            "0 retention row(s) across 4 fragment item observation(s); 0 unique retained item(s) across 2 merged graph item(s)."
        ));
        assert!(output.contains(
            "Counterfactual suppression (omit one reason; retain all alternatives): 0 finding observation(s)"
        ));
    }

    #[test]
    fn exact_expect_override_suppresses_only_the_named_finding() {
        let mut config = SurfaceConfig::default();
        config.r#override.push(SurfaceOverride {
            lint: "unnecessary-public".to_string(),
            package: Some("app-core".to_string()),
            crate_name: None,
            item: "migration::legacy_entry".to_string(),
            kind: Some("function".to_string()),
            target: None,
            level: SurfaceLintLevel::Expect,
            reason: "migration compatibility".to_string(),
        });
        let result = apply_diagnostic_policy(&analysis(), &config).expect("policy should apply");
        assert!(result.findings.is_empty());
        assert!(result.configuration_diagnostics.is_empty());

        config.r#override[0].item = "migration::gone".to_string();
        let result = apply_diagnostic_policy(&analysis(), &config).expect("stale policy should be diagnosed");
        assert_eq!(result.configuration_diagnostics[0].code, "unknown-item");
    }

    #[test]
    fn overlapping_qualified_and_unqualified_overrides_are_rejected() {
        let mut config = SurfaceConfig::default();
        for item in ["app_core::migration::legacy_entry", "migration::legacy_entry"] {
            config.r#override.push(SurfaceOverride {
                lint: "unnecessary-public".to_string(),
                package: Some("app-core".to_string()),
                crate_name: None,
                item: item.to_string(),
                kind: Some("function".to_string()),
                target: None,
                level: SurfaceLintLevel::Allow,
                reason: "one exact policy must own the finding".to_string(),
            });
        }

        let result = apply_diagnostic_policy(&analysis(), &config).expect("overlap should be diagnosed");
        assert!(
            result
                .configuration_diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "ambiguous-policy")
        );
    }

    #[test]
    fn expected_exclusion_requires_a_suppressed_finding() {
        let mut config = SurfaceConfig::default();
        config.exclude.push(SurfaceExclude {
            package: Some("app-core".to_string()),
            crate_name: None,
            module: Some("migration".to_string()),
            file: None,
            target: None,
            level: SurfaceLintLevel::Expect,
            reason: "generated migration surface".to_string(),
        });
        let result = apply_diagnostic_policy(&analysis(), &config).expect("finding should satisfy exclusion");
        assert!(result.findings.is_empty());
        assert!(result.configuration_diagnostics.is_empty());

        let mut no_findings = analysis();
        no_findings.findings.clear();
        let result = apply_diagnostic_policy(&no_findings, &config).expect("stale exclusion should be diagnosed");
        assert_eq!(result.configuration_diagnostics[0].code, "unfulfilled-expectation");
    }

    #[test]
    fn ordered_lint_levels_keep_warning_runs_successful_and_crate_visibility_allowed_by_default() {
        let mut config = SurfaceConfig::default();
        config.lint.extend([
            crate::config::SurfaceLintDirective {
                selector: "warnings".to_string(),
                level: SurfaceLintLevel::Warn,
            },
            crate::config::SurfaceLintDirective {
                selector: "unnecessary-public".to_string(),
                level: SurfaceLintLevel::Deny,
            },
            crate::config::SurfaceLintDirective {
                selector: "warnings".to_string(),
                level: SurfaceLintLevel::Warn,
            },
        ]);
        let result = apply_diagnostic_policy(&analysis(), &config).expect("ordered policy");
        assert_eq!(result.findings[0].level, SurfaceLintLevel::Warn);

        let mut crate_visibility = analysis();
        crate_visibility.findings[0].kind = SurfaceFindingKind::UnnecessaryCrateVisibility;
        let default =
            apply_diagnostic_policy(&crate_visibility, &SurfaceConfig::default()).expect("crate visibility default");
        assert!(default.findings.is_empty());
        config.lint.push(crate::config::SurfaceLintDirective {
            selector: "unnecessary-crate-visibility".to_string(),
            level: SurfaceLintLevel::Deny,
        });
        assert_eq!(
            apply_diagnostic_policy(&crate_visibility, &config)
                .expect("explicit crate visibility")
                .findings[0]
                .level,
            SurfaceLintLevel::Deny
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
        first.visibility_start = Some(0);
        first.visibility_end = Some(3);

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
        second.visibility_start = Some(second_start as u64);
        second.visibility_end = Some((second_start + b"pub(crate)".len()) as u64);
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
        first.visibility_start = Some(0);
        first.visibility_end = Some(3);
        let mut second = first.clone();
        second.identity = "second".to_string();
        assert!(
            apply_visibility_edits(source, &[&first, &second])
                .expect_err("shared spans are ambiguous")
                .to_string()
                .contains("overlap")
        );

        first.visibility_end = Some(6);
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
        item.visibility_start = Some(visibility_start as u64);
        item.visibility_end = Some((visibility_start + b"pub ( super )".len()) as u64);
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
