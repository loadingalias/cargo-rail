//! `cargo rail plan` - deterministic file-first change planner.

use crate::change_detection::classify::{
    FileProfile, RC_FILE_KIND_CI, RC_FILE_KIND_CUSTOM, RC_FILE_KIND_DOCS, RC_FILE_KIND_INFRA_PATTERN,
    RC_FILE_KIND_REPO_CONFIG, RC_FILE_KIND_RUST_BENCH, RC_FILE_KIND_RUST_BUILD_SCRIPT, RC_FILE_KIND_RUST_EXAMPLE,
    RC_FILE_KIND_RUST_SRC, RC_FILE_KIND_RUST_TEST, RC_FILE_KIND_SCRIPT, RC_FILE_KIND_TOML_LOCKFILE,
    RC_FILE_KIND_TOML_MANIFEST, RC_FILE_KIND_TOML_TOOLING, RC_FILE_KIND_TOML_WORKSPACE, RC_FILE_KIND_UNCLASSIFIED,
    classify_path, compile_custom_patterns, compile_infrastructure_patterns, custom_surface_names,
    custom_surfaces_for_path, matches_infrastructure_patterns,
};
use crate::change_detection::semantic::{SemanticFileChange, SemanticScope};
use crate::commands::common::{PlanOutputFormat, format_preview_list};
use crate::config::{ConfidenceProfile, UnknownFilePolicy};
use crate::error::{RailError, RailResult};
use crate::git::detect_default_base_ref;
use crate::graph::{
    DEPENDENCY_UNIVERSE_MODE, DependencyUniverse, ImpactDomain, ImpactFallback, ImpactPropagation, ImpactStep,
};
use crate::utils::{config_fingerprint, toolchain_fingerprint};
use crate::workspace::WorkspaceContext;
use cargo_metadata::PackageId;
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

#[derive(Debug)]
enum ResolvedRefs {
    Objects {
        from: String,
        to: String,
    },
    Worktree {
        since: Option<String>,
        merge_base: bool,
        base: String,
    },
}

impl ResolvedRefs {
    fn resolved_base(&self) -> &str {
        match self {
            Self::Objects { from, .. } => from,
            Self::Worktree { base, .. } => base,
        }
    }

    fn resolved_head(&self) -> &str {
        match self {
            Self::Objects { to, .. } => to,
            Self::Worktree { .. } => "WORKTREE",
        }
    }

    fn git_merge_base(&self) -> Option<String> {
        match self {
            Self::Worktree {
                merge_base: true, base, ..
            } => Some(base.clone()),
            Self::Objects { .. } | Self::Worktree { .. } => None,
        }
    }

    fn into_plan_refs(self) -> PlanRefs {
        match self {
            Self::Objects { from, to } => PlanRefs {
                since: None,
                resolved_base: from.clone(),
                resolved_head: to.clone(),
                from: Some(from),
                to: Some(to),
                merge_base: false,
            },
            Self::Worktree {
                since,
                merge_base,
                base,
            } => PlanRefs {
                since,
                from: None,
                to: None,
                merge_base,
                resolved_base: base,
                resolved_head: "WORKTREE".to_string(),
            },
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct PlanOutput {
    pub(crate) plan_contract_version: u32,
    pub(crate) inputs: PlanInputs,
    pub(crate) resolution_universe: ResolutionUniverse,
    pub(crate) files: Vec<PlannedFile>,
    pub(crate) impact: PlanImpact,
    pub(crate) scope: ExecutionScope,
    pub(crate) surfaces: BTreeMap<String, SurfaceDecision>,
    pub(crate) trace: Vec<TraceReason>,
    pub(crate) reproducibility: Reproducibility,
}

#[derive(Debug, Serialize)]
pub(crate) struct ResolutionUniverse {
    pub(crate) mode: &'static str,
    pub(crate) identity: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct PlanInputs {
    pub(crate) refs: PlanRefs,
    pub(crate) snapshot_id: Option<String>,
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
    pub(crate) build_transitive_crates: Vec<String>,
    pub(crate) development_transitive_crates: Vec<String>,
}

/// Execution-focused projection of the planner contract.
///
/// This is the stable handoff for Cargo, nextest, Just, and CI consumers.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExecutionScope {
    pub(crate) scope_contract_version: u32,
    pub(crate) resolved_base: String,
    pub(crate) resolved_head: String,
    pub(crate) mode: ExecutionScopeMode,
    pub(crate) crates: Vec<String>,
    pub(crate) cargo_args: Vec<String>,
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
    pub(crate) scope: SurfaceScope,
}

/// Package selection for one planner surface.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SurfaceScope {
    pub(crate) mode: ExecutionScopeMode,
    pub(crate) crates: Vec<String>,
    pub(crate) cargo_args: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct TraceReason {
    pub(crate) id: u32,
    pub(crate) code: &'static str,
    pub(crate) description: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) semantic_input: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) file: Option<String>,
    #[serde(rename = "crate", skip_serializing_if = "Option::is_none")]
    pub(crate) crate_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) package_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) depends_on: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) depends_on_package_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) edge_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) alias: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) target_predicate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) optional: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) uses_default_features: Option<bool>,
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) proc_macro: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) host: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) features: Vec<String>,
    pub(crate) selected_surfaces: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) fallback_reasons: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Reproducibility {
    pub(crate) cargo_rail_version: &'static str,
    pub(crate) config_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) git_merge_base: Option<String>,
    pub(crate) git_shallow_clone: bool,
}

const RC_FILE_OWNS_CRATE_DIRECT: &str = "FILE_OWNS_CRATE_DIRECT";
const RC_TRANSITIVE_DEPENDS_ON_DIRECT: &str = "TRANSITIVE_DEPENDS_ON_DIRECT";
const RC_OWNER_UNCERTAIN_FALLBACK: &str = "OWNER_UNCERTAIN_FALLBACK";
const RC_CONFIDENCE_PROFILE_STRICT: &str = "CONFIDENCE_PROFILE_STRICT";
const RC_CONFIDENCE_PROFILE_BALANCED: &str = "CONFIDENCE_PROFILE_BALANCED";
const RC_CONFIDENCE_PROFILE_FAST: &str = "CONFIDENCE_PROFILE_FAST";
const RC_CONFIDENCE_STRICT_OWNER_EXPANSION: &str = "CONFIDENCE_STRICT_OWNER_EXPANSION";
const RC_CONFIDENCE_FAST_SKIP_TRANSITIVE: &str = "CONFIDENCE_FAST_SKIP_TRANSITIVE";
const RC_SEMANTIC_INPUT_UNCHANGED: &str = "SEMANTIC_INPUT_UNCHANGED";
const RC_SEMANTIC_INPUT_PACKAGES: &str = "SEMANTIC_INPUT_PACKAGES";
const RC_SEMANTIC_INPUT_FALLBACK: &str = "SEMANTIC_INPUT_FALLBACK";
const PLAN_CONTRACT_VERSION: u32 = 7;
const SCOPE_CONTRACT_VERSION: u32 = 4;
const PLAN_SCHEMA_JSON: &str = include_str!("../../schemas/plan-v7.schema.json");
const PACKAGE_SCOPED_SURFACES: &[&str] = &["build", "test", "bench"];

#[derive(Debug, Clone, Copy)]
struct EffectiveConfidenceProfile {
    profile: ConfidenceProfile,
    source: &'static str,
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

/// Print the JSON Schema for the current planner contract.
pub fn print_plan_schema() {
    print!("{}", PLAN_SCHEMA_JSON);
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
    let snapshot = match &refs {
        ResolvedRefs::Worktree { .. } => Some(ctx.snapshot()?),
        ResolvedRefs::Objects { .. } => None,
    };
    let historical_resolution_unavailable = snapshot.is_none();
    let dependency_universe = DependencyUniverse::from_metadata(ctx.cargo().metadata(), ctx.git()?.repo_root())?;
    let changed_files = collect_changed_files(ctx, &refs)?;
    let semantic_head = match &refs {
        ResolvedRefs::Objects { to, .. } => Some(to.as_str()),
        ResolvedRefs::Worktree { .. } => None,
    };
    let semantic_changes =
        crate::change_detection::semantic::analyze(ctx, &changed_files, refs.resolved_base(), semantic_head)?;
    let confidence = resolve_confidence_profile(ctx, opts)?;
    let change_detection_config = ctx.config().map(|config| &config.change_detection);
    let infrastructure_patterns = compile_infrastructure_patterns(change_detection_config);
    let custom_patterns = compile_custom_patterns(change_detection_config);
    let configured_custom_surfaces = custom_surface_names(&custom_patterns);

    let changed_file_count = changed_files.len();
    let mut trace = Vec::with_capacity(changed_file_count * 4); // Multiple trace entries per file
    let mut surface_refs: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();

    let mut planned_files = Vec::with_capacity(changed_file_count);
    let mut direct_crates: BTreeSet<String> = BTreeSet::new();
    let mut build_test_seed_package_ids: HashSet<PackageId> = HashSet::new();
    let mut surface_packages = BTreeMap::<String, BTreeSet<String>>::new();
    let mut workspace_surfaces = BTreeSet::<String>::new();
    let unknown_policy = unknown_file_policy(ctx);

    push_trace(
        &mut trace,
        &mut surface_refs,
        profile_reason_code(confidence.profile),
        None,
        None,
        None,
        &[] as &[String],
    );

    for path in &changed_files {
        let file_kind = classify_path(Path::new(path));
        let semantic_change = semantic_changes.get(path);
        let custom_surfaces = custom_surfaces_for_path(path, &custom_patterns);
        let owner = ctx.graph().file_to_package(Path::new(path));
        let semantic_workspace_packages = semantic_change
            .and_then(|change| match &change.scope {
                SemanticScope::Packages(packages) => Some(packages),
                SemanticScope::None | SemanticScope::Workspace => None,
            })
            .into_iter()
            .flatten()
            .filter_map(|package_id| ctx.graph().package(package_id))
            .filter(|package| package.is_workspace_member)
            .collect::<Vec<_>>();
        let owners: Vec<String> = if semantic_workspace_packages.is_empty() {
            owner.iter().map(|package| package.name.clone()).collect()
        } else {
            semantic_workspace_packages
                .iter()
                .map(|package| package.name.clone())
                .collect()
        };

        if let Some(owner) = owner {
            direct_crates.insert(owner.name.clone());
            push_trace(
                &mut trace,
                &mut surface_refs,
                RC_FILE_OWNS_CRATE_DIRECT,
                Some(path),
                Some(&owner.name),
                None,
                &[] as &[String],
            );
            if let Some(reason) = trace.last_mut() {
                reason.package_id = Some(owner.id.to_string());
            }
        }
        direct_crates.extend(semantic_workspace_packages.iter().map(|package| package.name.clone()));

        let owner_scope = if owner.is_none() && !semantic_workspace_packages.is_empty() {
            "semantic".to_string()
        } else {
            owner_scope(path, &owners)
        };

        let mut kind_surfaces: Vec<String> = file_kind
            .default_surfaces()
            .iter()
            .map(|surface| (*surface).to_string())
            .collect();
        apply_semantic_surface_policy(semantic_change, &mut kind_surfaces);
        let has_builtin_infra_surface = kind_surfaces.iter().any(|surface| surface == "infra");
        let extra_infra_surface =
            !has_builtin_infra_surface && matches_infrastructure_patterns(path, &infrastructure_patterns);

        let unknown_owner_build_test = file_kind == FileProfile::Unknown
            && !owners.is_empty()
            && confidence.profile != ConfidenceProfile::Fast
            && matches!(
                unknown_policy,
                UnknownFilePolicy::OwnedBuildTest | UnknownFilePolicy::Strict
            );

        if file_kind == FileProfile::Unknown {
            for surface in unknown_surfaces_for_path(unknown_policy, &owners) {
                ensure_surface(&mut kind_surfaces, surface);
            }
        }

        let apply_strict_owner_expansion = confidence.profile == ConfidenceProfile::Strict && !owners.is_empty();

        if unknown_owner_build_test {
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

        let baseline_transitive_seed = (file_kind.seeds_build_test_transitive()
            && !matches!(semantic_change.map(|change| &change.scope), Some(SemanticScope::None)))
            || unknown_owner_build_test;
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
            match semantic_change.map(|change| &change.scope) {
                Some(SemanticScope::Packages(packages)) => build_test_seed_package_ids.extend(packages.iter().cloned()),
                Some(SemanticScope::None | SemanticScope::Workspace) => {}
                None => {
                    if let Some(package) = owner {
                        build_test_seed_package_ids.insert(package.id.clone());
                    }
                }
            }
        }

        match semantic_change.map(|change| &change.scope) {
            Some(SemanticScope::Packages(_)) => {
                for package in &semantic_workspace_packages {
                    record_surface_scope(
                        &kind_surfaces,
                        Some(&package.name),
                        &mut surface_packages,
                        &mut workspace_surfaces,
                    );
                }
            }
            Some(SemanticScope::None) => {}
            Some(SemanticScope::Workspace) => {
                record_surface_scope(&kind_surfaces, None, &mut surface_packages, &mut workspace_surfaces)
            }
            None => record_surface_scope(
                &kind_surfaces,
                if historical_resolution_unavailable {
                    None
                } else {
                    owner.map(|package| package.name.as_str())
                },
                &mut surface_packages,
                &mut workspace_surfaces,
            ),
        }

        push_trace(
            &mut trace,
            &mut surface_refs,
            file_kind.reason_code(),
            Some(path),
            None,
            None,
            if semantic_change.is_some() {
                &[] as &[String]
            } else {
                &kind_surfaces
            },
        );
        if historical_resolution_unavailable
            && kind_surfaces
                .iter()
                .any(|surface| PACKAGE_SCOPED_SURFACES.contains(&surface.as_str()))
            && let Some(reason) = trace.last_mut()
        {
            reason.fallback_reasons.push("historical_resolution_unavailable");
        }

        if let Some(change) = semantic_change {
            push_semantic_trace(
                &mut trace,
                &mut surface_refs,
                path,
                change,
                &semantic_workspace_packages,
                &kind_surfaces,
            );
        }

        if extra_infra_surface {
            let infra_surfaces = vec!["infra".to_string()];
            push_trace(
                &mut trace,
                &mut surface_refs,
                RC_FILE_KIND_INFRA_PATTERN,
                Some(path),
                None,
                None,
                &infra_surfaces,
            );
        }

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
            kind: file_kind.planned_kind().to_string(),
            sub_kind: file_kind.planned_sub_kind().map(str::to_string),
            custom_surfaces,
            owners,
            owner_scope,
        });
    }

    let mut semantic_impact = dependency_universe.propagate(&build_test_seed_package_ids)?;
    if historical_resolution_unavailable {
        for step in &mut semantic_impact.steps {
            if !step
                .fallbacks
                .contains(&ImpactFallback::HistoricalResolutionUnavailable)
            {
                step.fallbacks.push(ImpactFallback::HistoricalResolutionUnavailable);
            }
            step.fallbacks.sort();
        }
    }
    let build_transitive_crates = package_names(ctx, &semantic_impact.build)?;
    let development_transitive_crates = package_names(ctx, &semantic_impact.development)?;
    emit_semantic_impact_trace(
        ctx,
        &semantic_impact,
        &mut trace,
        &mut surface_refs,
        &mut surface_packages,
    )?;

    let workspace_members = ctx.graph().workspace_members();
    let surfaces = build_surfaces(
        &surface_refs,
        &configured_custom_surfaces,
        &surface_packages,
        &workspace_surfaces,
        workspace_members,
    );

    // Compute reproducibility metadata
    let git_merge_base = refs.git_merge_base();
    let snapshot_id = snapshot.as_ref().map(|snapshot| snapshot.id().to_string());
    let (configuration_identity, toolchain_identity) = if let Some(snapshot) = &snapshot {
        (
            snapshot.configuration_fingerprint().to_string(),
            snapshot.toolchain_fingerprint().to_string(),
        )
    } else {
        (
            config_fingerprint(ctx.workspace_root()),
            toolchain_fingerprint(ctx.workspace_root()),
        )
    };

    let impact = PlanImpact {
        direct_crates: direct_crates.into_iter().collect(),
        build_transitive_crates,
        development_transitive_crates,
    };
    let scope = build_execution_scope(
        &surfaces,
        workspace_members.len(),
        refs.resolved_base(),
        refs.resolved_head(),
    );

    let output = PlanOutput {
        plan_contract_version: PLAN_CONTRACT_VERSION,
        inputs: PlanInputs {
            refs: refs.into_plan_refs(),
            snapshot_id,
            workspace_root: ctx.workspace_root().display().to_string(),
            config_fingerprint: configuration_identity.clone(),
            toolchain_fingerprint: toolchain_identity,
            confidence_profile: confidence_profile_name(confidence.profile).to_string(),
            confidence_profile_source: confidence.source.to_string(),
        },
        resolution_universe: ResolutionUniverse {
            mode: DEPENDENCY_UNIVERSE_MODE,
            identity: dependency_universe.identity().to_string(),
        },
        files: planned_files,
        impact,
        scope,
        surfaces,
        trace,
        reproducibility: Reproducibility {
            cargo_rail_version: env!("CARGO_PKG_VERSION"),
            config_hash: configuration_identity,
            git_merge_base,
            git_shallow_clone: is_shallow_clone(ctx.workspace_root()),
        },
    };

    validate_surface_reason_invariants(&output)?;

    Ok(output)
}

fn unknown_file_policy(ctx: &WorkspaceContext) -> UnknownFilePolicy {
    ctx.config()
        .as_ref()
        .map(|config| config.change_detection.unknown_file_policy)
        .unwrap_or_default()
}

fn unknown_surfaces_for_path(policy: UnknownFilePolicy, owners: &[String]) -> &'static [&'static str] {
    const BUILD_TEST: &[&str] = &["build", "test"];
    const INFRA: &[&str] = &["infra"];
    const DOCS: &[&str] = &["docs"];

    match policy {
        UnknownFilePolicy::Docs => DOCS,
        UnknownFilePolicy::OwnedBuildTest => {
            if owners.is_empty() {
                DOCS
            } else {
                BUILD_TEST
            }
        }
        UnknownFilePolicy::WorkspaceInfra => {
            if owners.is_empty() {
                INFRA
            } else {
                DOCS
            }
        }
        UnknownFilePolicy::Strict => {
            if owners.is_empty() {
                INFRA
            } else {
                BUILD_TEST
            }
        }
    }
}

fn resolve_confidence_profile(ctx: &WorkspaceContext, opts: &PlanOptions) -> RailResult<EffectiveConfidenceProfile> {
    if let Some(raw) = opts.confidence_profile.as_deref() {
        let profile = parse_confidence_profile(raw)?;
        return Ok(EffectiveConfidenceProfile { profile, source: "cli" });
    }

    let mut profile = ConfidenceProfile::default();
    let mut source = "default";

    if let Some(config) = ctx.config() {
        profile = config.change_detection.confidence_profile;
        source = "config";
    }

    Ok(EffectiveConfidenceProfile { profile, source })
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
    surfaces: &BTreeMap<String, SurfaceDecision>,
    workspace_package_count: usize,
    resolved_base: &str,
    resolved_head: &str,
) -> ExecutionScope {
    let package_scopes = PACKAGE_SCOPED_SURFACES
        .iter()
        .filter_map(|surface| surfaces.get(*surface))
        .filter(|decision| decision.enabled)
        .map(|decision| &decision.scope)
        .collect::<Vec<_>>();
    let workspace_selected = package_scopes
        .iter()
        .any(|scope| matches!(scope.mode, ExecutionScopeMode::Workspace));
    let crates = package_scopes
        .iter()
        .flat_map(|scope| scope.crates.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let (mode, crates) = if package_scopes.is_empty() {
        (ExecutionScopeMode::Empty, Vec::new())
    } else if workspace_selected || crates.len() == workspace_package_count {
        (ExecutionScopeMode::Workspace, Vec::new())
    } else {
        (ExecutionScopeMode::Crates, crates)
    };
    let cargo_args = cargo_args_for_scope(mode, &crates);

    ExecutionScope {
        scope_contract_version: SCOPE_CONTRACT_VERSION,
        resolved_base: resolved_base.to_string(),
        resolved_head: resolved_head.to_string(),
        mode,
        crates,
        cargo_args,
    }
}

fn cargo_args_for_scope(mode: ExecutionScopeMode, crates: &[String]) -> Vec<String> {
    match mode {
        ExecutionScopeMode::Empty => Vec::new(),
        ExecutionScopeMode::Workspace => vec!["--workspace".to_string()],
        ExecutionScopeMode::Crates => crates
            .iter()
            .flat_map(|crate_name| ["-p".to_string(), crate_name.clone()])
            .collect(),
    }
}

fn reason_description(code: &str) -> &'static str {
    match code {
        RC_FILE_KIND_RUST_SRC => "Rust source file changed",
        RC_FILE_KIND_RUST_TEST => "Rust test file changed",
        RC_FILE_KIND_RUST_BENCH => "Rust benchmark file changed",
        RC_FILE_KIND_RUST_EXAMPLE => "Rust example file changed",
        RC_FILE_KIND_RUST_BUILD_SCRIPT => "Cargo build script changed",
        RC_FILE_KIND_TOML_MANIFEST => "Cargo.toml changed",
        RC_FILE_KIND_TOML_WORKSPACE => "Workspace Cargo.toml changed",
        RC_FILE_KIND_TOML_TOOLING => "Tooling config changed",
        RC_FILE_KIND_TOML_LOCKFILE => "Cargo.lock changed",
        RC_FILE_KIND_CI => "CI or workflow file changed",
        RC_FILE_KIND_SCRIPT => "Script file changed",
        RC_FILE_KIND_DOCS => "Documentation changed",
        RC_FILE_KIND_REPO_CONFIG => "Repository config changed",
        RC_FILE_KIND_INFRA_PATTERN => "Configured infrastructure pattern matched",
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
        RC_SEMANTIC_INPUT_UNCHANGED => "Cargo input bytes changed without changing effective build semantics",
        RC_SEMANTIC_INPUT_PACKAGES => "Cargo semantic change localized to resolved packages",
        RC_SEMANTIC_INPUT_FALLBACK => "Cargo semantic change requires conservative workspace impact",
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
        .surfaces
        .iter()
        .filter(|(_, decision)| decision.enabled)
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
    match (&opts.from, &opts.to) {
        (Some(from), Some(to)) => {
            if opts.since.is_some() || opts.merge_base {
                return Err(RailError::message(
                    "--from/--to cannot be combined with --since or --merge-base",
                ));
            }
            return Ok(ResolvedRefs::Objects {
                from: from.clone(),
                to: to.clone(),
            });
        }
        (Some(_), None) => return Err(RailError::message("--from requires --to")),
        (None, Some(_)) => return Err(RailError::message("--to requires --from")),
        (None, None) => {}
    }

    let resolved_base = if opts.merge_base {
        let default_branch = detect_default_base_ref(ctx.git()?.git())?;
        ctx.git()?.git().get_merge_base(&default_branch, "HEAD")?
    } else if let Some(since) = &opts.since {
        since.clone()
    } else {
        detect_default_base_ref(ctx.git()?.git())?
    };

    Ok(ResolvedRefs::Worktree {
        since: opts.since.clone(),
        merge_base: opts.merge_base,
        base: resolved_base,
    })
}

fn collect_changed_files(ctx: &WorkspaceContext, refs: &ResolvedRefs) -> RailResult<Vec<String>> {
    let raw_paths: Vec<PathBuf> = match refs {
        ResolvedRefs::Objects { from, to } => ctx.exclude_generated_source_paths(
            ctx.git()?
                .git()
                .get_changed_files_between(from, Some(to))?
                .into_iter()
                .map(|(path, _)| path)
                .collect(),
        )?,
        ResolvedRefs::Worktree { base, .. } => {
            let changes = if let Some(capture) = ctx.source_capture() {
                capture.changes_from(ctx.git()?.git(), base)?
            } else {
                ctx.capture_worktree_source()?.changes_from(ctx.git()?.git(), base)?
            };
            changes
                .entries()
                .iter()
                .map(|change| change.path.as_path().to_path_buf())
                .collect()
        }
    };

    let mut files: Vec<String> = raw_paths
        .into_iter()
        .filter_map(|git_path| ctx.to_workspace_path(&git_path))
        .map(|p| crate::utils::path_to_git_format(&p))
        .collect();

    files.sort();
    files.dedup();
    Ok(files)
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

fn package_names(ctx: &WorkspaceContext, package_ids: &BTreeSet<PackageId>) -> RailResult<Vec<String>> {
    package_ids
        .iter()
        .map(|package_id| {
            ctx.graph()
                .package(package_id)
                .map(|package| package.name.clone())
                .ok_or_else(|| {
                    RailError::message(format!(
                        "impacted package '{}' is missing from the resolved graph",
                        package_id
                    ))
                })
        })
        .collect()
}

fn emit_semantic_impact_trace(
    ctx: &WorkspaceContext,
    impact: &ImpactPropagation,
    trace: &mut Vec<TraceReason>,
    surface_refs: &mut BTreeMap<String, BTreeSet<u32>>,
    surface_packages: &mut BTreeMap<String, BTreeSet<String>>,
) -> RailResult<()> {
    for step in &impact.steps {
        if step.domain == ImpactDomain::Development && impact.build.contains(&step.dependent) {
            continue;
        }
        let dependent = ctx.graph().package(&step.dependent).ok_or_else(|| {
            RailError::message(format!(
                "impacted package '{}' is missing from the resolved graph",
                step.dependent
            ))
        })?;
        let dependency = ctx.graph().package(&step.dependency).ok_or_else(|| {
            RailError::message(format!(
                "dependency package '{}' is missing from the resolved graph",
                step.dependency
            ))
        })?;
        let surfaces = match step.domain {
            ImpactDomain::Build => ["bench", "build", "test"].as_slice(),
            ImpactDomain::Development => ["bench", "test"].as_slice(),
        };
        for surface in surfaces {
            surface_packages
                .entry((*surface).to_string())
                .or_default()
                .insert(dependent.name.clone());
        }
        push_impact_trace(
            trace,
            surface_refs,
            step,
            dependent.name.as_str(),
            dependency.name.as_str(),
            surfaces,
        );
    }
    Ok(())
}

fn build_surfaces(
    surface_refs: &BTreeMap<String, BTreeSet<u32>>,
    custom_surfaces: &[String],
    surface_packages: &BTreeMap<String, BTreeSet<String>>,
    workspace_surfaces: &BTreeSet<String>,
    workspace_members: &[String],
) -> BTreeMap<String, SurfaceDecision> {
    // Use static slice and map to owned strings
    static BUILTIN_SURFACES: &[&str] = &["build", "test", "bench", "docs", "infra", "surface"];
    let mut surface_names: Vec<String> = BUILTIN_SURFACES.iter().map(|&s| String::from(s)).collect();
    surface_names.extend(custom_surfaces.iter().cloned());

    let mut result = BTreeMap::new();
    for surface in surface_names {
        let reasons: Vec<u32> = surface_refs
            .get(&surface)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default();

        let enabled = !reasons.is_empty();
        let crates = surface_packages
            .get(&surface)
            .map(|packages| packages.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let package_scoped = PACKAGE_SCOPED_SURFACES.contains(&surface.as_str());
        let mode = if !enabled || !package_scoped {
            ExecutionScopeMode::Empty
        } else if workspace_surfaces.contains(&surface) || crates.is_empty() || crates.len() == workspace_members.len()
        {
            ExecutionScopeMode::Workspace
        } else {
            ExecutionScopeMode::Crates
        };
        let crates = if matches!(mode, ExecutionScopeMode::Crates) {
            crates
        } else {
            Vec::new()
        };
        let cargo_args = cargo_args_for_scope(mode, &crates);
        result.insert(
            surface,
            SurfaceDecision {
                enabled,
                reasons,
                scope: SurfaceScope {
                    mode,
                    crates,
                    cargo_args,
                },
            },
        );
    }

    result
}

fn record_surface_scope(
    surfaces: &[String],
    package: Option<&str>,
    surface_packages: &mut BTreeMap<String, BTreeSet<String>>,
    workspace_surfaces: &mut BTreeSet<String>,
) {
    for surface in surfaces
        .iter()
        .filter(|surface| PACKAGE_SCOPED_SURFACES.contains(&surface.as_str()))
    {
        if let Some(package) = package {
            surface_packages
                .entry(surface.clone())
                .or_default()
                .insert(package.to_string());
        } else {
            workspace_surfaces.insert(surface.clone());
        }
    }
}

fn apply_semantic_surface_policy(change: Option<&SemanticFileChange>, surfaces: &mut Vec<String>) {
    if matches!(change.map(|change| &change.scope), Some(SemanticScope::None)) {
        surfaces.retain(|surface| !PACKAGE_SCOPED_SURFACES.contains(&surface.as_str()) && surface != "surface");
    }
}

fn next_trace_id(trace: &[TraceReason]) -> u32 {
    u32::try_from(trace.len().saturating_add(1)).unwrap_or(u32::MAX)
}

fn push_semantic_trace(
    trace: &mut Vec<TraceReason>,
    surface_refs: &mut BTreeMap<String, BTreeSet<u32>>,
    path: &str,
    change: &SemanticFileChange,
    workspace_packages: &[&crate::graph::core::PackageNode],
    surfaces: &[String],
) {
    let code = match change.scope {
        SemanticScope::None => RC_SEMANTIC_INPUT_UNCHANGED,
        SemanticScope::Packages(_) => RC_SEMANTIC_INPUT_PACKAGES,
        SemanticScope::Workspace => RC_SEMANTIC_INPUT_FALLBACK,
    };
    let selected_surfaces = if matches!(change.scope, SemanticScope::Packages(_)) && workspace_packages.is_empty() {
        surfaces
            .iter()
            .filter(|surface| !PACKAGE_SCOPED_SURFACES.contains(&surface.as_str()))
            .cloned()
            .collect::<Vec<_>>()
    } else {
        surfaces.to_vec()
    };
    let packages = if workspace_packages.is_empty() {
        vec![None]
    } else {
        workspace_packages.iter().copied().map(Some).collect()
    };
    for package in packages {
        let id = next_trace_id(trace);
        for surface in &selected_surfaces {
            surface_refs.entry(surface.clone()).or_default().insert(id);
        }
        trace.push(TraceReason {
            id,
            code,
            description: reason_description(code),
            semantic_input: Some(change.input),
            file: Some(path.to_string()),
            crate_name: package.map(|package| package.name.clone()),
            package_id: package.map(|package| package.id.to_string()),
            depends_on: None,
            depends_on_package_id: None,
            edge_kind: None,
            alias: None,
            target_predicate: None,
            optional: None,
            uses_default_features: None,
            proc_macro: false,
            host: false,
            features: Vec::new(),
            selected_surfaces: selected_surfaces.clone(),
            fallback_reasons: change.fallback.into_iter().collect(),
        });
    }
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
    let id = next_trace_id(trace);

    for surface in surfaces {
        surface_refs.entry(surface.clone()).or_default().insert(id);
    }

    trace.push(TraceReason {
        id,
        code,
        description: reason_description(code),
        semantic_input: None,
        file: file.map(ToString::to_string),
        crate_name: crate_name.map(ToString::to_string),
        package_id: None,
        depends_on: depends_on.map(ToString::to_string),
        depends_on_package_id: None,
        edge_kind: None,
        alias: None,
        target_predicate: None,
        optional: None,
        uses_default_features: None,
        proc_macro: false,
        host: false,
        features: Vec::new(),
        selected_surfaces: surfaces.to_vec(),
        fallback_reasons: Vec::new(),
    });

    id
}

fn push_impact_trace(
    trace: &mut Vec<TraceReason>,
    surface_refs: &mut BTreeMap<String, BTreeSet<u32>>,
    step: &ImpactStep,
    dependent: &str,
    dependency: &str,
    surfaces: &[&str],
) {
    let id = next_trace_id(trace);
    for surface in surfaces {
        surface_refs.entry((*surface).to_string()).or_default().insert(id);
    }
    trace.push(TraceReason {
        id,
        code: RC_TRANSITIVE_DEPENDS_ON_DIRECT,
        description: reason_description(RC_TRANSITIVE_DEPENDS_ON_DIRECT),
        semantic_input: None,
        file: None,
        crate_name: Some(dependent.to_string()),
        package_id: Some(step.dependent.to_string()),
        depends_on: Some(dependency.to_string()),
        depends_on_package_id: Some(step.dependency.to_string()),
        edge_kind: Some(dependency_kind_name(step.kind).to_string()),
        alias: Some(step.alias.clone()),
        target_predicate: step.target.clone(),
        optional: Some(step.optional),
        uses_default_features: Some(step.uses_default_features),
        proc_macro: step.proc_macro,
        host: step.host,
        features: step.features.clone(),
        selected_surfaces: surfaces.iter().map(|surface| (*surface).to_string()).collect(),
        fallback_reasons: step
            .fallbacks
            .iter()
            .map(|fallback| fallback_reason(*fallback))
            .collect(),
    });
}

fn dependency_kind_name(kind: cargo_metadata::DependencyKind) -> &'static str {
    match kind {
        cargo_metadata::DependencyKind::Normal => "normal",
        cargo_metadata::DependencyKind::Development => "development",
        cargo_metadata::DependencyKind::Build => "build",
        cargo_metadata::DependencyKind::Unknown => "unknown",
    }
}

fn fallback_reason(fallback: ImpactFallback) -> &'static str {
    match fallback {
        ImpactFallback::HistoricalResolutionUnavailable => "historical_resolution_unavailable",
        ImpactFallback::UnknownDependencyKind => "dependency_kind_unknown",
    }
}

fn is_false(value: &bool) -> bool {
    !value
}

fn format_text(output: &PlanOutput, explain: bool) -> String {
    let estimated_capacity = 256 + (output.impact.direct_crates.len() * 24) + (output.scope.crates.len() * 24);
    let mut out = String::with_capacity(estimated_capacity);
    let active_surfaces = active_surface_names(output);
    let lookup = reason_lookup(output);
    let top_reasons = summarize_reason_ids(&collect_active_reason_ids(output), &lookup);

    out.push_str("plan\n\n");
    out.push_str("base: ");
    out.push_str(&output.scope.resolved_base);
    out.push_str("\nchanged files: ");
    out.push_str(&output.files.len().to_string());
    out.push_str("\nsurfaces: ");
    if active_surfaces.is_empty() {
        out.push_str("none");
    } else {
        out.push_str(&active_surfaces.join(", "));
    }
    out.push('\n');
    match output.scope.mode {
        ExecutionScopeMode::Workspace => out.push_str("scope: workspace\n"),
        ExecutionScopeMode::Crates => {
            out.push_str("scope: crates (");
            out.push_str(&output.scope.crates.len().to_string());
            out.push_str(")\n");
        }
        ExecutionScopeMode::Empty => out.push_str("scope: empty\n"),
    }
    if !output.impact.direct_crates.is_empty() {
        out.push_str("direct crates (");
        out.push_str(&output.impact.direct_crates.len().to_string());
        out.push_str("): ");
        out.push_str(&format_preview_list(&output.impact.direct_crates, 12));
        out.push('\n');
    }
    if matches!(output.scope.mode, ExecutionScopeMode::Crates) && !output.scope.crates.is_empty() {
        out.push_str("execution crates (");
        out.push_str(&output.scope.crates.len().to_string());
        out.push_str("): ");
        out.push_str(&format_preview_list(&output.scope.crates, 12));
        out.push('\n');
    }
    out.push_str("why: ");
    out.push_str(&top_reasons);
    out.push('\n');

    if explain {
        out.push('\n');
        out.push_str("explain:\n");
        for surface in active_surfaces {
            if let Some(summary) = summarize_surface_reason(output, surface) {
                out.push_str("  ");
                out.push_str(surface);
                out.push_str(": ");
                out.push_str(&summary);
                out.push('\n');
            }
        }
        let evidence = output
            .trace
            .iter()
            .filter(|reason| {
                reason.semantic_input.is_some()
                    || reason.edge_kind.is_some()
                    || reason.package_id.is_some()
                    || !reason.fallback_reasons.is_empty()
            })
            .collect::<Vec<_>>();
        if !evidence.is_empty() {
            out.push_str("  evidence:\n");
            for reason in evidence {
                out.push_str("    [");
                out.push_str(&reason.id.to_string());
                out.push(']');
                if let Some(input) = reason.semantic_input {
                    out.push_str(" input=");
                    out.push_str(input);
                }
                if let Some(package_id) = &reason.package_id {
                    out.push_str(" package=");
                    out.push_str(package_id);
                }
                if let Some(depends_on) = &reason.depends_on_package_id {
                    out.push_str(" dependency=");
                    out.push_str(depends_on);
                }
                if let Some(kind) = &reason.edge_kind {
                    out.push_str(" kind=");
                    out.push_str(kind);
                }
                if let Some(alias) = &reason.alias {
                    out.push_str(" alias=");
                    out.push_str(alias);
                }
                if let Some(target) = &reason.target_predicate {
                    out.push_str(" target=");
                    out.push_str(target);
                }
                if let Some(optional) = reason.optional {
                    out.push_str(" optional=");
                    out.push_str(if optional { "true" } else { "false" });
                }
                if let Some(default_features) = reason.uses_default_features {
                    out.push_str(" default-features=");
                    out.push_str(if default_features { "true" } else { "false" });
                }
                if !reason.features.is_empty() {
                    out.push_str(" features=");
                    out.push_str(&reason.features.join(","));
                }
                if reason.proc_macro {
                    out.push_str(" proc-macro=true");
                }
                if reason.host {
                    out.push_str(" host=true");
                }
                if !reason.selected_surfaces.is_empty() {
                    out.push_str(" surfaces=");
                    out.push_str(&reason.selected_surfaces.join(","));
                }
                if !reason.fallback_reasons.is_empty() {
                    out.push_str(" fallback=");
                    out.push_str(&reason.fallback_reasons.join(","));
                }
                out.push('\n');
            }
        }
    }

    out
}

fn format_github(output: &PlanOutput, debug: bool) -> RailResult<String> {
    let scope_json = to_json(&output.scope)?;
    let plan_json = if debug { Some(to_json(output)?) } else { None };
    let mut out = String::with_capacity(160 + scope_json.len() + plan_json.as_ref().map_or(0, String::len));
    for (key, enabled) in [
        ("build", surface_enabled(output, "build")),
        ("test", surface_enabled(output, "test")),
        ("bench", surface_enabled(output, "bench")),
        ("docs", surface_enabled(output, "docs")),
        ("infra", surface_enabled(output, "infra")),
        ("surface", surface_enabled(output, "surface")),
    ] {
        out.push_str(key);
        out.push('=');
        out.push_str(if enabled { "true" } else { "false" });
        out.push('\n');
    }
    out.push_str("base_ref=");
    out.push_str(&output.inputs.refs.resolved_base);
    out.push_str("\ncargo_args=");
    out.push_str(&output.scope.cargo_args.join(" "));
    out.push_str("\nscope_json=");
    out.push_str(&scope_json);
    out.push('\n');
    if let Some(plan_json) = plan_json {
        out.push_str("plan_json=");
        out.push_str(&plan_json);
        out.push('\n');
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
