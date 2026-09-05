//! Evidence-backed named-work decisions for the v9 planner contract.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;
use std::sync::Arc;

use cargo_metadata::{Metadata, MetadataCommand, Package, PackageId};
use glob::Pattern;
use rscrypto::Sha256;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::evidence::{
    EvidenceBindings, ObservedInput, PlanningEvidenceState, PortableBaseEdgeDomain, PortableBaseModel,
    required_capabilities,
};
use super::source_features::{SourceActivation, expanded_features, source_activation};
use super::{ConfigDelta, IndexedFileChange, PlanningIndex};
use crate::change_detection::semantic::{SemanticFileChange, SemanticScope};
use crate::config::{CargoPrerequisiteConfig, CargoRootConfig, CargoTargetRootConfig, PlanWorkConfig, PlanWorkScope};
use crate::error::{RailError, RailResult};
use crate::graph::{DependencyUniverse, ImpactPropagation};
use crate::workspace::WorkspaceContext;

const PLAN_CONTRACT_VERSION: u32 = 9;
const WORK_CATALOG_VERSION: u32 = 1;

const BUILTIN_CARGO_WORK: &[&str] = &[
    "cargo.build",
    "cargo.clippy",
    "cargo.doc",
    "cargo.doctest",
    "cargo.package",
    "cargo.test",
];

const BUILTIN_WORK: &[(&str, WorkSpecScope)] = &[
    ("cargo.build", WorkSpecScope::Cargo),
    ("cargo.clippy", WorkSpecScope::Cargo),
    ("cargo.doc", WorkSpecScope::Cargo),
    ("cargo.doctest", WorkSpecScope::Cargo),
    ("cargo.fmt", WorkSpecScope::Repository),
    ("cargo.package", WorkSpecScope::Cargo),
    ("cargo.test", WorkSpecScope::Cargo),
    ("dependency-policy", WorkSpecScope::Repository),
    ("release.semver", WorkSpecScope::Cargo),
    ("surface", WorkSpecScope::Repository),
];

/// Complete v9 named-work plan.
#[derive(Debug, Serialize)]
pub(crate) struct WorkPlan {
    pub(crate) plan_contract_version: u32,
    pub(crate) identity: String,
    pub(crate) inputs: WorkPlanInputs,
    pub(crate) changes: WorkChanges,
    pub(crate) work: BTreeMap<String, WorkDecision>,
    pub(crate) required: Vec<String>,
    pub(crate) evidence: BTreeMap<String, EvidenceRecord>,
    pub(crate) attribution: BTreeMap<String, WorkAttribution>,
}

/// Explanatory facts captured at the same boundary that selects executable work.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct WorkAttribution {
    pub(crate) inputs: BTreeSet<WorkInput>,
    pub(crate) selections: Vec<SelectionAttribution>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) struct WorkInput {
    pub(crate) kind: WorkInputKind,
    pub(crate) value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkInputKind {
    Path,
    Configuration,
    Package,
    Dependency,
    Work,
    Prerequisite,
}

impl WorkInput {
    fn new(kind: WorkInputKind, value: impl Into<String>) -> Self {
        Self {
            kind,
            value: value.into(),
        }
    }
    fn path(value: impl Into<String>) -> Self {
        Self::new(WorkInputKind::Path, value)
    }
    fn evidence_text(&self) -> String {
        let prefix = match self.kind {
            WorkInputKind::Path => "path",
            WorkInputKind::Configuration => "config",
            WorkInputKind::Package | WorkInputKind::Dependency => "cargo",
            WorkInputKind::Work => "cargo",
            WorkInputKind::Prerequisite => "prerequisite",
        };
        format!("{prefix}:{}", self.value)
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SelectionAttribution {
    /// A package key or variant ID from this work item's exact scope.
    pub(crate) subject: String,
    pub(crate) relation: ImpactRelation,
    /// One captured originating package when dependency propagation selected this subject.
    pub(crate) origin: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ImpactRelation {
    Direct,
    Dependency,
    Unattributed,
}

#[derive(Debug, Serialize)]
pub(crate) struct WorkPlanInputs {
    pub(crate) base: String,
    pub(crate) head: String,
    pub(crate) head_commit: String,
    pub(crate) capture: Option<String>,
    pub(crate) cargo: String,
    pub(crate) configuration: String,
    pub(crate) toolchain: String,
    pub(crate) target: String,
    pub(crate) platform: String,
    pub(crate) catalog: String,
    pub(crate) evidence: Vec<String>,
    pub(crate) r#override: PlanOverride,
}

pub(crate) struct WorkPlanAuthority {
    pub(crate) base: String,
    pub(crate) head: String,
    pub(crate) head_commit: String,
    pub(crate) capture: Option<String>,
    pub(crate) cargo_configuration_identity: String,
    pub(crate) toolchain_identity: String,
    pub(crate) target_identity: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PlanOverride {
    None,
    All,
}

#[derive(Debug, Serialize)]
pub(crate) struct WorkChanges {
    pub(crate) files: Vec<IndexedFileChange>,
    pub(crate) cargo: Vec<CargoStructuralDelta>,
    pub(crate) config: Vec<ConfigDelta>,
}

#[derive(Debug, Serialize)]
pub(crate) struct CargoStructuralDelta {
    pub(crate) package: String,
    pub(crate) target: Option<String>,
    pub(crate) kind: String,
}

/// A decision is tagged so skipped work cannot carry executable scope.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum WorkDecision {
    Skipped {
        evidence: Vec<String>,
    },
    Required {
        cause: WorkCause,
        scope: WorkScope,
        evidence: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkCause {
    ChangedInput,
    IncompleteEvidence,
    ForcedAll,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum WorkScope {
    Repository,
    Cargo { selection: CargoSelection },
    Variants { selection: VariantSelection },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum CargoSelection {
    Workspace {
        cargo_args: Vec<String>,
        targets: Vec<CargoTargetSelector>,
    },
    Packages {
        packages: Vec<PortablePackageSelector>,
        cargo_args: Vec<String>,
        targets: Vec<CargoTargetSelector>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) struct PortablePackageSelector {
    pub(crate) key: String,
    pub(crate) name: String,
    pub(crate) cargo_spec: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) struct CargoTargetSelector {
    pub(crate) package: String,
    pub(crate) name: String,
    pub(crate) kind: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum VariantSelection {
    All {
        evidence: String,
    },
    Selected {
        variants: Vec<SelectedVariant>,
        evidence: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SelectedVariant {
    pub(crate) id: String,
    pub(crate) dimensions: BTreeMap<String, ScalarDimension>,
}

struct SelectedVariants {
    report_inputs: BTreeSet<WorkInput>,
    relations: Vec<SelectionAttribution>,
    variants: Vec<SelectedVariant>,
    attributed_inputs: BTreeSet<String>,
    all_required_inputs_attributed: bool,
}

#[derive(Default)]
struct CargoRootMatch {
    selected: bool,
    attributed_paths: BTreeSet<String>,
    origins: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum ScalarDimension {
    String(String),
    Integer(i64),
    Boolean(bool),
}

#[derive(Debug, Serialize)]
pub(crate) struct EvidenceRecord {
    pub(crate) code: String,
    pub(crate) subject: String,
    pub(crate) description: String,
    pub(crate) input: Option<String>,
    pub(crate) complete: bool,
}

pub(crate) fn format_work_plan(
    plan: &WorkPlan,
    explain: bool,
    explain_work: Option<&str>,
    verbose: bool,
) -> RailResult<String> {
    if let Some(id) = explain_work
        && !plan.work.contains_key(id)
    {
        return Err(RailError::with_help(
            format!("unknown work ID '{id}'"),
            format!(
                "choose one of: {}",
                plan.work.keys().cloned().collect::<Vec<_>>().join(", ")
            ),
        ));
    }

    let mut output = String::new();
    if plan.required.is_empty() {
        output.push_str("No work required.\n");
    } else {
        output.push_str("Required work:\n");
        let mut indirect = 0;
        for id in &plan.required {
            let WorkDecision::Required { scope, cause, evidence } = &plan.work[id] else {
                continue;
            };
            let attribution = &plan.attribution[id];
            if !explain
                && explain_work.is_none()
                && *cause == WorkCause::ChangedInput
                && !attribution.inputs.is_empty()
                && attribution.inputs.iter().all(|input| {
                    matches!(
                        input.kind,
                        WorkInputKind::Dependency | WorkInputKind::Work | WorkInputKind::Prerequisite
                    )
                })
            {
                indirect += 1;
                continue;
            }
            let scope_text = if explain || explain_work.is_some() {
                human_scope(scope)
            } else {
                human_attributed_scope(scope, attribution)
            };
            output.push_str(&format!("  {id} [{scope_text}]\n"));
            match cause {
                WorkCause::IncompleteEvidence => {
                    for record in evidence
                        .iter()
                        .filter_map(|key| plan.evidence.get(key))
                        .filter(|record| !record.complete)
                    {
                        output.push_str(&format!("    Scope expanded: {}\n", record.description));
                    }
                }
                WorkCause::ForcedAll => output.push_str("    Required by --all.\n"),
                WorkCause::ChangedInput => {}
            }
        }
        if indirect > 0 {
            output.push_str(&format!(
                "Also required: {indirect} dependency work item(s). Use --explain for details.\n"
            ));
        }
    }

    if verbose {
        let changed = plan.changes.files.len() + plan.changes.cargo.len() + plan.changes.config.len();
        let skipped = plan.work.len().saturating_sub(plan.required.len());
        output.push_str(&format!("Changed inputs: {changed}; skipped work: {skipped}.\n"));
    }

    let selected = if let Some(id) = explain_work {
        vec![(id, &plan.work[id])]
    } else if explain {
        plan.required
            .iter()
            .filter_map(|id| plan.work.get(id).map(|decision| (id.as_str(), decision)))
            .collect()
    } else {
        Vec::new()
    };
    if !selected.is_empty() {
        output.push_str("\nDecision proof:\n");
        for (id, decision) in selected {
            let (state, references) = match decision {
                WorkDecision::Skipped { evidence } => ("skipped", evidence),
                WorkDecision::Required { cause, evidence, .. } => (
                    match cause {
                        WorkCause::ChangedInput => "required: changed input",
                        WorkCause::IncompleteEvidence => "required: incomplete evidence widened scope",
                        WorkCause::ForcedAll => "required: --all",
                    },
                    evidence,
                ),
            };
            output.push_str(&format!("  {id} ({state})\n"));
            for evidence in references.iter().filter_map(|reference| plan.evidence.get(reference)) {
                output.push_str(&format!("    {}", evidence.description));
                if let Some(input) = &evidence.input {
                    output.push_str(&format!(" [{input}]"));
                }
                output.push('\n');
            }
        }
    }
    Ok(output)
}

fn human_attributed_scope(scope: &WorkScope, attribution: &WorkAttribution) -> String {
    let names = match scope {
        WorkScope::Cargo {
            selection: CargoSelection::Packages { packages, .. },
        } => packages
            .iter()
            .map(|package| (package.key.as_str(), package.name.as_str()))
            .collect::<BTreeMap<_, _>>(),
        WorkScope::Variants {
            selection: VariantSelection::Selected { variants, .. },
        } => variants
            .iter()
            .map(|variant| (variant.id.as_str(), variant.id.as_str()))
            .collect(),
        _ => return human_scope(scope),
    };
    let mut shown = Vec::new();
    let mut dependent = 0;
    for selection in &attribution.selections {
        let name = names
            .get(selection.subject.as_str())
            .copied()
            .unwrap_or(&selection.subject);
        match selection.relation {
            ImpactRelation::Dependency => dependent += 1,
            ImpactRelation::Direct => shown.push(name.to_string()),
            ImpactRelation::Unattributed => shown.push(format!("{name} (attribution unavailable)")),
        }
    }
    if dependent > 0 {
        shown.push(format!("{dependent} dependent selection(s); --explain for details"));
    }
    shown.join(", ")
}

fn human_scope(scope: &WorkScope) -> String {
    match scope {
        WorkScope::Repository => "repository".to_string(),
        WorkScope::Cargo {
            selection: CargoSelection::Workspace { targets, .. },
        } => match targets.len() {
            0 => "Cargo workspace".to_string(),
            count => format!("Cargo workspace, {count} target(s)"),
        },
        WorkScope::Cargo {
            selection: CargoSelection::Packages { packages, targets, .. },
        } => {
            let names = packages
                .iter()
                .map(|package| package.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            if targets.is_empty() {
                format!("Cargo packages: {names}")
            } else {
                format!("Cargo packages: {names}; {} target(s)", targets.len())
            }
        }
        WorkScope::Variants {
            selection: VariantSelection::All { .. },
        } => "all variants".to_string(),
        WorkScope::Variants {
            selection: VariantSelection::Selected { variants, .. },
        } => format!(
            "variants: {}",
            variants
                .iter()
                .map(|variant| variant.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum WorkSpecScope {
    Repository,
    Cargo,
    Variants,
}

#[derive(Debug, Serialize)]
struct WorkSpecIdentity<'a> {
    id: &'a str,
    origin: &'a str,
    scope: WorkSpecScope,
    paths: &'a [String],
    config: &'a [String],
    cargo: &'a [String],
    cargo_prerequisites: &'a [CargoPrerequisiteConfig],
    variant_catalog: Option<&'a str>,
    variant_catalog_identity: Option<&'a str>,
}

#[derive(Debug)]
struct WorkSpec {
    id: String,
    origin: &'static str,
    scope: WorkSpecScope,
    paths: Vec<String>,
    config: Vec<String>,
    cargo: Vec<String>,
    cargo_prerequisites: Vec<CargoPrerequisiteConfig>,
    resolved_prerequisites: Vec<ResolvedCargoPrerequisite>,
    variant_catalog: Option<String>,
    catalog: Option<VariantCatalog>,
    variant_catalog_identity: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VariantCatalog {
    variant_catalog_version: u32,
    work: String,
    variants: Vec<VariantRow>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VariantRow {
    id: String,
    dimensions: BTreeMap<String, ScalarDimension>,
    #[serde(default)]
    config: Vec<String>,
    #[serde(default)]
    external_paths: Vec<String>,
    #[serde(default)]
    cargo_roots: Vec<VariantCargoRoot>,
    #[serde(skip)]
    resolved_cargo_roots: Vec<ResolvedCargoRoot>,
}

/// A package/target root in the primary workspace or one configured auxiliary workspace.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VariantCargoRoot {
    package: String,
    target: Option<CargoTargetRootConfig>,
    /// Exact no-default-feature roots. Absence preserves package-level v2 behavior.
    #[serde(default)]
    features: Option<Vec<String>>,
    /// Registered `release.auxiliary_cargo_manifests` entry, when not the primary workspace.
    #[serde(default)]
    manifest: Option<String>,
}

#[derive(Debug)]
struct ResolvedCargoPrerequisite {
    source_work: String,
    when: Vec<ResolvedCargoRoot>,
    require: Vec<ResolvedCargoRoot>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ResolvedCargoRoot {
    domain: String,
    package: PackageId,
    package_key: String,
    target: Option<ResolvedCargoTarget>,
    features: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ResolvedCargoTarget {
    name: String,
    kinds: Vec<String>,
}

#[derive(Default)]
struct StructuralCargoImpact {
    workspace: bool,
    deliverables_workspace: bool,
    seeds: HashSet<PackageId>,
    build: BTreeSet<PackageId>,
    development: BTreeSet<PackageId>,
    current_build_origins: BTreeMap<PackageId, String>,
    current_development_origins: BTreeMap<PackageId, String>,
    historical_build_origins: BTreeMap<PackageId, String>,
    historical_development_origins: BTreeMap<PackageId, String>,
    seed_paths: BTreeMap<PackageId, BTreeSet<String>>,
    unattributed_seeds: BTreeSet<PackageId>,
}

struct CargoDomain {
    metadata: Arc<Metadata>,
    universe: DependencyUniverse,
}

struct PlanningCargoModel {
    domains: BTreeMap<String, CargoDomain>,
}

const PRIMARY_CARGO_DOMAIN: &str = "workspace";

impl PlanningCargoModel {
    fn new(ctx: &WorkspaceContext) -> RailResult<Self> {
        let metadata = ctx.cargo().shared_metadata();
        let universe = DependencyUniverse::from_metadata(&metadata, ctx.planning_authority_source_root())?;
        Ok(Self {
            domains: BTreeMap::from([(PRIMARY_CARGO_DOMAIN.to_string(), CargoDomain { metadata, universe })]),
        })
    }

    fn load_auxiliary(&mut self, ctx: &WorkspaceContext, manifest: &str) -> RailResult<()> {
        if manifest == PRIMARY_CARGO_DOMAIN {
            return Err(RailError::message(format!(
                "variant Cargo root manifest '{manifest}' is not a repository-relative manifest path"
            )));
        }
        if self.domains.contains_key(manifest) {
            return Ok(());
        }
        crate::config::plan::validate_positive_path(manifest, "variant Cargo root manifest", false)
            .map_err(RailError::Config)?;
        let configured = ctx.config().is_some_and(|config| {
            config
                .release
                .auxiliary_cargo_manifests
                .iter()
                .any(|path| crate::utils::path_to_git_format(path) == manifest)
        });
        if !configured {
            return Err(RailError::message(format!(
                "variant Cargo root manifest '{manifest}' is not registered in release.auxiliary_cargo_manifests"
            )));
        }

        ctx.validate_planning_source_unchanged()?;
        let source_root = ctx.planning_authority_source_root();
        let manifest_path = source_root.join(manifest);
        let mut command = MetadataCommand::new();
        command
            .current_dir(source_root)
            .manifest_path(&manifest_path)
            .other_options(vec!["--locked".to_string()]);
        crate::instrumentation::record_cargo_metadata_load(false);
        let metadata = Arc::new(command.exec().map_err(|error| {
            RailError::message(format!(
                "failed to load configured auxiliary Cargo manifest '{manifest}': {error}"
            ))
        })?);
        for package in metadata.packages.iter().filter(|package| package.source.is_none()) {
            let package_manifest = crate::utils::canonicalize_existing(package.manifest_path.as_std_path()).map_err(
                |error| {
                    RailError::message(format!(
                        "failed to resolve local package manifest '{}' from auxiliary Cargo manifest '{manifest}': {error}",
                        package.manifest_path
                    ))
                },
            )?;
            if package_manifest.strip_prefix(source_root).is_err() {
                return Err(RailError::with_help(
                    format!(
                        "auxiliary Cargo manifest '{manifest}' resolves local package manifest '{}' outside the captured workspace",
                        package.manifest_path
                    ),
                    "keep auxiliary path dependencies inside the captured workspace",
                ));
            }
        }
        let universe = DependencyUniverse::from_metadata(&metadata, ctx.planning_authority_source_root())?;
        ctx.validate_planning_source_unchanged()?;
        self.domains
            .insert(manifest.to_string(), CargoDomain { metadata, universe });
        Ok(())
    }

    fn identity(&self) -> RailResult<String> {
        let domains = self
            .domains
            .iter()
            .map(|(name, domain)| (name, domain.universe.identity()))
            .collect::<Vec<_>>();
        identity("resolution-universe-v3", &canonical_bytes(&domains)?)
    }

    fn domain(&self, name: &str) -> RailResult<&CargoDomain> {
        self.domains
            .get(name)
            .ok_or_else(|| RailError::message(format!("Cargo domain '{name}' was not loaded")))
    }

    fn package<'a>(&'a self, root: &ResolvedCargoRoot) -> RailResult<&'a Package> {
        self.domain(&root.domain)?
            .metadata
            .packages
            .iter()
            .find(|package| package.id == root.package)
            .ok_or_else(|| RailError::message(format!("resolved Cargo package '{}' disappeared", root.package)))
    }

    fn package_in_domain<'a>(&'a self, domain: &str, package: &PackageId) -> Option<&'a Package> {
        self.domains
            .get(domain)?
            .metadata
            .packages
            .iter()
            .find(|candidate| &candidate.id == package)
    }

    fn active_features(&self, domain: &str, package: &PackageId) -> Option<BTreeSet<String>> {
        self.domains
            .get(domain)?
            .metadata
            .resolve
            .as_ref()?
            .nodes
            .iter()
            .find(|node| &node.id == package)
            .map(|node| node.features.iter().map(ToString::to_string).collect())
    }

    fn root_affected_by(&self, root: &ResolvedCargoRoot, seed: &PackageId) -> RailResult<bool> {
        let domain = self.domain(&root.domain)?;
        if !domain.universe.contains_package(seed) {
            return Ok(false);
        }
        if seed == &root.package {
            return Ok(true);
        }
        Ok(domain
            .universe
            .propagate(&HashSet::from([seed.clone()]))?
            .build
            .contains(&root.package))
    }

    fn propagate(&self, seeds: &HashSet<PackageId>) -> RailResult<ImpactPropagation> {
        let mut merged = ImpactPropagation::default();
        for domain in self.domains.values() {
            let known = seeds
                .iter()
                .filter(|seed| domain.universe.contains_package(seed))
                .cloned()
                .collect::<HashSet<_>>();
            if known.is_empty() {
                continue;
            }
            let propagation = domain.universe.propagate(&known)?;
            merged.build.extend(propagation.build);
            merged.development.extend(propagation.development);
            for (package, origin) in propagation.build_origins {
                let entry = merged.build_origins.entry(package).or_insert_with(|| origin.clone());
                if origin < *entry {
                    *entry = origin;
                }
            }
            for (package, origin) in propagation.development_origins {
                let entry = merged
                    .development_origins
                    .entry(package)
                    .or_insert_with(|| origin.clone());
                if origin < *entry {
                    *entry = origin;
                }
            }
        }
        merged.development.retain(|package| !merged.build.contains(package));
        merged
            .development_origins
            .retain(|package, _| !merged.build_origins.contains_key(package));
        Ok(merged)
    }

    fn owning_packages(&self, ctx: &WorkspaceContext, path: &Path) -> BTreeSet<PackageId> {
        let mut matches = Vec::new();
        for domain in self.domains.values() {
            for package in domain
                .metadata
                .packages
                .iter()
                .filter(|package| package.source.is_none())
            {
                let Some(root) = package.manifest_path.parent().and_then(|root| {
                    root.as_std_path()
                        .strip_prefix(ctx.planning_authority_source_root())
                        .ok()
                }) else {
                    continue;
                };
                if path == root.join("Cargo.toml")
                    || path
                        .strip_prefix(root)
                        .is_ok_and(|suffix| !suffix.as_os_str().is_empty())
                {
                    matches.push((root.components().count(), package.id.clone()));
                }
            }
        }
        let depth = matches.iter().map(|(depth, _)| *depth).max();
        matches
            .into_iter()
            .filter_map(|(candidate_depth, package)| (Some(candidate_depth) == depth).then_some(package))
            .collect()
    }

    fn package_key(&self, ctx: &WorkspaceContext, package: &PackageId) -> Option<String> {
        self.domains
            .values()
            .find_map(|domain| {
                domain
                    .metadata
                    .packages
                    .iter()
                    .find(|candidate| &candidate.id == package)
            })
            .map(|package| portable_package_key(ctx, package))
    }
}

#[derive(Default)]
struct PrerequisiteSourceImpact {
    roots: Vec<ResolvedCargoRoot>,
    inputs: BTreeSet<String>,
}

#[derive(Debug, Serialize)]
struct EvidenceIdentity<'a> {
    code: &'a str,
    subject: &'a str,
    input: Option<&'a str>,
    complete: bool,
}

struct Evaluator<'a> {
    ctx: &'a WorkspaceContext,
    cargo_model: &'a PlanningCargoModel,
    paths: &'a [&'a str],
    config_deltas: &'a [ConfigDelta],
    semantic_changes: &'a BTreeMap<String, SemanticFileChange>,
    observed: &'a PlanningEvidenceState,
    structural_impact: &'a StructuralCargoImpact,
    prerequisite_source_roots: &'a BTreeMap<String, PrerequisiteSourceImpact>,
    evidence: BTreeMap<String, EvidenceRecord>,
    decisions: BTreeMap<String, WorkDecision>,
    report_inputs: BTreeMap<String, BTreeSet<WorkInput>>,
    variant_relations: BTreeMap<String, Vec<SelectionAttribution>>,
}

pub(crate) fn build_work_plan(
    ctx: &WorkspaceContext,
    index: PlanningIndex,
    authority: WorkPlanAuthority,
    semantic_changes: &BTreeMap<String, SemanticFileChange>,
    evidence_path: Option<&Path>,
    force_all: bool,
) -> RailResult<WorkPlan> {
    let mut cargo_model = PlanningCargoModel::new(ctx)?;
    let specs = build_catalog(ctx, &mut cargo_model)?;
    let cargo_identity = cargo_model.identity()?;
    let catalog = catalog_identity(&specs)?;
    let observed = PlanningEvidenceState::load(
        evidence_path,
        EvidenceBindings {
            source_base: &authority.base,
            cargo_configuration_identity: &authority.cargo_configuration_identity,
            toolchain_identity: &authority.toolchain_identity,
            target_identity: &authority.target_identity,
        },
        &cargo_identity,
        ctx,
    );
    let (work, required, evidence, attribution) = {
        let paths = index.paths().collect::<Vec<_>>();
        let structural_impact =
            structural_cargo_impact(ctx, &cargo_model, semantic_changes, observed.base_model(), &paths)?;
        let prerequisite_source_roots = prerequisite_source_roots(&specs, &structural_impact);
        let mut evaluator = Evaluator {
            ctx,
            cargo_model: &cargo_model,
            paths: &paths,
            config_deltas: index.config_deltas(),
            semantic_changes,
            observed: &observed,
            structural_impact: &structural_impact,
            prerequisite_source_roots: &prerequisite_source_roots,
            evidence: BTreeMap::new(),
            decisions: BTreeMap::new(),
            report_inputs: BTreeMap::new(),
            variant_relations: BTreeMap::new(),
        };

        for spec in specs.iter().filter(|spec| spec.origin == "builtin") {
            evaluator.evaluate_builtin(spec)?;
        }
        for spec in specs.iter().filter(|spec| spec.origin == "repository") {
            evaluator.evaluate_declared(spec)?;
        }
        if force_all {
            evaluator.force_all(&specs)?;
        }

        let required = evaluator
            .decisions
            .iter()
            .filter_map(|(id, decision)| matches!(decision, WorkDecision::Required { .. }).then_some(id.clone()))
            .collect::<Vec<_>>();
        validate_decisions(&specs, &evaluator.decisions, &required, &evaluator.evidence)?;
        let attribution = evaluator.attribution()?;
        (evaluator.decisions, required, evaluator.evidence, attribution)
    };

    let PlanningIndex { file_changes, config } = index;

    let mut plan = WorkPlan {
        plan_contract_version: PLAN_CONTRACT_VERSION,
        identity: String::new(),
        inputs: WorkPlanInputs {
            base: authority.base,
            head: authority.head,
            head_commit: authority.head_commit,
            capture: authority.capture,
            cargo: cargo_identity,
            configuration: authority.cargo_configuration_identity,
            toolchain: authority.toolchain_identity,
            target: authority.target_identity,
            platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            catalog,
            evidence: observed.identity().map(str::to_string).into_iter().collect(),
            r#override: if force_all {
                PlanOverride::All
            } else {
                PlanOverride::None
            },
        },
        changes: WorkChanges {
            files: file_changes,
            cargo: cargo_structural_deltas(ctx, semantic_changes),
            config,
        },
        work,
        required,
        evidence,
        attribution,
    };
    plan.identity = plan_identity(&plan)?;
    Ok(plan)
}

fn build_catalog(ctx: &WorkspaceContext, cargo_model: &mut PlanningCargoModel) -> RailResult<Vec<WorkSpec>> {
    let mut specs = BUILTIN_WORK
        .iter()
        .map(|(id, scope)| WorkSpec {
            id: (*id).to_string(),
            origin: "builtin",
            scope: *scope,
            paths: Vec::new(),
            config: Vec::new(),
            cargo: Vec::new(),
            cargo_prerequisites: Vec::new(),
            resolved_prerequisites: Vec::new(),
            variant_catalog: None,
            catalog: None,
            variant_catalog_identity: None,
        })
        .collect::<Vec<_>>();

    if let Some(config) = ctx.config() {
        for (id, work) in &config.plan.work {
            if BUILTIN_WORK.binary_search_by_key(&id.as_str(), |(id, _)| *id).is_ok() {
                return Err(RailError::message(format!(
                    "repository work ID '{id}' collides with a code-owned built-in"
                )));
            }
            specs.push(declared_spec(ctx, cargo_model, id, work)?);
        }
    }
    specs.sort_unstable_by(|left, right| left.id.cmp(&right.id));
    Ok(specs)
}

fn declared_spec(
    ctx: &WorkspaceContext,
    cargo_model: &mut PlanningCargoModel,
    id: &str,
    work: &PlanWorkConfig,
) -> RailResult<WorkSpec> {
    let mut paths = sorted_unique(work.paths.clone());
    let config = sorted_unique(work.config.clone());
    let cargo = sorted_unique(work.cargo.clone());
    let (catalog, variant_catalog_identity) = if let Some(path) = &work.variant_catalog {
        paths.push(path.clone());
        paths.sort_unstable();
        paths.dedup();
        let bytes = ctx
            .read_planning_current_files(std::slice::from_ref(path))?
            .into_iter()
            .next()
            .ok_or_else(|| RailError::message(format!("variant catalog '{path}' was not captured")))?;
        let mut catalog: VariantCatalog = serde_json::from_slice(&bytes)
            .map_err(|error| RailError::message(format!("invalid variant catalog '{path}': {error}")))?;
        validate_variant_catalog(ctx, cargo_model, id, path, &cargo, &mut catalog)?;
        let identity = identity(
            &format!("variant-catalog-v{}", catalog.variant_catalog_version),
            &canonical_bytes(&catalog)?,
        )?;
        (Some(catalog), Some(identity))
    } else {
        (None, None)
    };
    Ok(WorkSpec {
        id: id.to_string(),
        origin: "repository",
        scope: match work.scope {
            PlanWorkScope::Repository => WorkSpecScope::Repository,
            PlanWorkScope::Cargo => WorkSpecScope::Cargo,
            PlanWorkScope::Variants => WorkSpecScope::Variants,
        },
        paths,
        config,
        cargo,
        cargo_prerequisites: work.cargo_prerequisites.clone(),
        resolved_prerequisites: work
            .cargo_prerequisites
            .iter()
            .enumerate()
            .map(|(index, prerequisite)| resolve_cargo_prerequisite(ctx, id, index, prerequisite))
            .collect::<RailResult<Vec<_>>>()?,
        variant_catalog: work.variant_catalog.clone(),
        catalog,
        variant_catalog_identity,
    })
}

fn validate_variant_catalog(
    ctx: &WorkspaceContext,
    cargo_model: &mut PlanningCargoModel,
    id: &str,
    path: &str,
    work_cargo: &[String],
    catalog: &mut VariantCatalog,
) -> RailResult<()> {
    if catalog.variant_catalog_version != 2 {
        return Err(RailError::message(format!(
            "variant catalog '{path}' uses unsupported contract {}",
            catalog.variant_catalog_version
        )));
    }
    if catalog.work != id {
        return Err(RailError::message(format!(
            "variant catalog '{path}' belongs to '{}' instead of '{id}'",
            catalog.work
        )));
    }
    if !work_cargo.is_empty() {
        return Err(RailError::message(format!(
            "variant catalog '{path}' requires plan.work.{id}.cargo to be empty; variant Cargo impact derives only from typed cargo_roots"
        )));
    }
    let mut ids = BTreeSet::new();
    for row in &mut catalog.variants {
        crate::config::plan::validate_stable_id(&row.id, &format!("{path}.variants.id")).map_err(RailError::Config)?;
        if !ids.insert(row.id.clone()) {
            return Err(RailError::message(format!(
                "variant catalog '{path}' contains duplicate variant '{}'",
                row.id
            )));
        }
        if row.external_paths.is_empty() && row.config.is_empty() && row.cargo_roots.is_empty() {
            return Err(RailError::message(format!(
                "variant '{}' in '{path}' has no input selectors",
                row.id
            )));
        }
        crate::config::plan::validate_unique(&row.config, &format!("{path}.{}.config", row.id))
            .map_err(RailError::Config)?;
        crate::config::plan::validate_unique(&row.external_paths, &format!("{path}.{}.external_paths", row.id))
            .map_err(RailError::Config)?;
        for pattern in &row.external_paths {
            crate::config::plan::validate_positive_path(pattern, &format!("{path}.{}.external_paths", row.id), true)
                .map_err(RailError::Config)?;
        }
        for field in &row.config {
            if crate::config::schema::field_spec(field).is_none() {
                return Err(RailError::message(format!(
                    "variant '{}' in '{path}' names unknown config field '{field}'",
                    row.id
                )));
            }
        }
        row.config.sort_unstable();
        row.external_paths.sort_unstable();
        for (index, root) in row.cargo_roots.iter_mut().enumerate() {
            if let Some(features) = &mut root.features {
                crate::config::plan::validate_unique(
                    features,
                    &format!("{path}.{}.cargo_roots.{index}.features", row.id),
                )
                .map_err(RailError::Config)?;
                features.sort_unstable();
            }
        }
        row.cargo_roots.sort_unstable();
        if row.cargo_roots.windows(2).any(|roots| roots[0] == roots[1]) {
            return Err(RailError::message(format!(
                "variant '{}' in '{path}' contains duplicate Cargo roots",
                row.id
            )));
        }
        row.resolved_cargo_roots = row
            .cargo_roots
            .iter()
            .enumerate()
            .map(|(index, root)| {
                resolve_variant_cargo_root(
                    ctx,
                    cargo_model,
                    root,
                    &format!("variant '{}.cargo_roots.{index}' in '{path}'", row.id),
                )
            })
            .collect::<RailResult<Vec<_>>>()?;
    }
    catalog.variants.sort_unstable_by(|left, right| left.id.cmp(&right.id));
    Ok(())
}

fn resolve_cargo_prerequisite(
    ctx: &WorkspaceContext,
    work: &str,
    index: usize,
    prerequisite: &CargoPrerequisiteConfig,
) -> RailResult<ResolvedCargoPrerequisite> {
    let subject = format!("plan.work.{work}.cargo_prerequisites.{index}");
    Ok(ResolvedCargoPrerequisite {
        source_work: prerequisite.source_work.clone(),
        when: prerequisite
            .when
            .iter()
            .enumerate()
            .map(|(root, value)| resolve_cargo_root(ctx, value, &format!("{subject}.when.{root}")))
            .collect::<RailResult<Vec<_>>>()?,
        require: prerequisite
            .require
            .iter()
            .enumerate()
            .map(|(root, value)| resolve_cargo_root(ctx, value, &format!("{subject}.require.{root}")))
            .collect::<RailResult<Vec<_>>>()?,
    })
}

fn resolve_cargo_root(ctx: &WorkspaceContext, root: &CargoRootConfig, subject: &str) -> RailResult<ResolvedCargoRoot> {
    resolve_metadata_cargo_root(
        ctx,
        ctx.cargo().metadata(),
        PRIMARY_CARGO_DOMAIN,
        &root.package,
        root.target.as_ref(),
        None,
        subject,
    )
}

fn resolve_variant_cargo_root(
    ctx: &WorkspaceContext,
    cargo_model: &mut PlanningCargoModel,
    root: &VariantCargoRoot,
    subject: &str,
) -> RailResult<ResolvedCargoRoot> {
    let domain = root.manifest.as_deref().unwrap_or(PRIMARY_CARGO_DOMAIN);
    if let Some(manifest) = &root.manifest {
        cargo_model.load_auxiliary(ctx, manifest)?;
    }
    let resolved = resolve_metadata_cargo_root(
        ctx,
        &cargo_model.domain(domain)?.metadata,
        domain,
        &root.package,
        root.target.as_ref(),
        root.features.clone(),
        subject,
    )?;
    if let Some(features) = &resolved.features {
        expanded_features(cargo_model.package(&resolved)?, features)?;
    }
    Ok(resolved)
}

fn resolve_metadata_cargo_root(
    ctx: &WorkspaceContext,
    metadata: &Metadata,
    domain: &str,
    package_name: &str,
    expected_target: Option<&CargoTargetRootConfig>,
    features: Option<Vec<String>>,
    subject: &str,
) -> RailResult<ResolvedCargoRoot> {
    let matches = metadata
        .workspace_packages()
        .iter()
        .filter(|package| package.name == package_name)
        .copied()
        .collect::<Vec<_>>();
    let package = match matches.as_slice() {
        [package] => *package,
        [] => {
            return Err(RailError::message(format!(
                "{subject} names unknown workspace package '{}'",
                package_name
            )));
        }
        _ => {
            return Err(RailError::message(format!(
                "{subject} names ambiguous workspace package '{}'",
                package_name
            )));
        }
    };
    let target = expected_target
        .map(|expected| {
            let matches = package
                .targets
                .iter()
                .filter_map(|target| {
                    let mut kinds = target.kind.iter().map(ToString::to_string).collect::<Vec<_>>();
                    kinds.sort_unstable();
                    kinds.dedup();
                    (target.name == expected.name && kinds.contains(&expected.kind)).then_some((target, kinds))
                })
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [(target, kinds)] => Ok(ResolvedCargoTarget {
                    name: target.name.clone(),
                    kinds: kinds.clone(),
                }),
                [] => Err(RailError::message(format!(
                    "{subject} names unknown target '{}:{}' in package '{}'",
                    expected.kind, expected.name, package_name
                ))),
                _ => Err(RailError::message(format!(
                    "{subject} names ambiguous target '{}:{}' in package '{}'",
                    expected.kind, expected.name, package_name
                ))),
            }
        })
        .transpose()?;
    Ok(ResolvedCargoRoot {
        domain: domain.to_string(),
        package: package.id.clone(),
        package_key: portable_package_key(ctx, package),
        target,
        features,
    })
}

fn structural_cargo_impact(
    ctx: &WorkspaceContext,
    cargo_model: &PlanningCargoModel,
    semantic_changes: &BTreeMap<String, SemanticFileChange>,
    base_model: Option<&PortableBaseModel>,
    paths: &[&str],
) -> RailResult<StructuralCargoImpact> {
    let mut impact = StructuralCargoImpact::default();
    let mut unresolved_workspace_paths = BTreeSet::new();
    for path in paths {
        match semantic_changes.get(*path).map(|change| &change.scope) {
            Some(SemanticScope::None) | None if path.ends_with(".rs") => {
                for package in cargo_model.owning_packages(ctx, Path::new(path)) {
                    impact.seeds.insert(package.clone());
                    impact
                        .seed_paths
                        .entry(package)
                        .or_default()
                        .insert((*path).to_string());
                }
            }
            Some(SemanticScope::None) | None => {}
            Some(SemanticScope::Packages(packages)) => {
                impact.seeds.extend(packages.iter().cloned());
                if path.ends_with(".rs") {
                    for package in packages {
                        impact
                            .seed_paths
                            .entry(package.clone())
                            .or_default()
                            .insert((*path).to_string());
                    }
                } else {
                    impact.unattributed_seeds.extend(packages.iter().cloned());
                }
            }
            Some(SemanticScope::Workspace) => {
                unresolved_workspace_paths.insert(*path);
            }
        }
    }
    impact.deliverables_workspace = paths.iter().any(|path| is_global_deliverable_cargo_input(path));

    let propagation = cargo_model.propagate(&impact.seeds)?;
    impact.build.extend(impact.seeds.iter().cloned());
    impact.build.extend(propagation.build);
    impact.development = propagation.development;
    for (package, origin) in propagation.build_origins {
        let portable_origin = cargo_model.package_key(ctx, &origin).ok_or_else(|| {
            RailError::message(format!(
                "originating Cargo package '{origin}' is absent from captured metadata"
            ))
        })?;
        impact.current_build_origins.insert(package, portable_origin);
    }
    for (package, origin) in propagation.development_origins {
        let portable_origin = cargo_model.package_key(ctx, &origin).ok_or_else(|| {
            RailError::message(format!(
                "originating Cargo package '{origin}' is absent from captured metadata"
            ))
        })?;
        impact.current_development_origins.insert(package, portable_origin);
    }

    if !unresolved_workspace_paths.is_empty() {
        let Some(model) = base_model else {
            impact.workspace = true;
            return Ok(impact);
        };
        let current_by_key = ctx
            .cargo()
            .metadata()
            .workspace_packages()
            .iter()
            .map(|package| (portable_package_key(ctx, package), package.id.clone()))
            .collect::<BTreeMap<_, _>>();
        let removed = model
            .packages
            .iter()
            .filter(|package| !current_by_key.contains_key(&package.key))
            .collect::<Vec<_>>();
        let mut historical_seeds = BTreeSet::new();
        for path in unresolved_workspace_paths {
            let before = historical_seeds.len();
            if path == "Cargo.toml" {
                historical_seeds.extend(removed.iter().map(|package| package.key.clone()));
            } else {
                historical_seeds.extend(
                    removed
                        .iter()
                        .filter(|package| package_owns_historical_path(package.root.as_str(), path))
                        .map(|package| package.key.clone()),
                );
            }
            if historical_seeds.len() == before {
                impact.workspace = true;
            }
        }
        let reverse_edges = model
            .edges
            .iter()
            .fold(BTreeMap::<&str, Vec<_>>::new(), |mut edges, edge| {
                edges.entry(edge.dependency.as_str()).or_default().push(edge);
                edges
            });
        let mut stack = historical_seeds.iter().cloned().collect::<Vec<_>>();
        let mut historical_build_origins = historical_seeds
            .into_iter()
            .map(|seed| (seed.clone(), seed))
            .collect::<BTreeMap<_, _>>();
        let mut historical_development_origins = BTreeMap::<String, String>::new();
        while let Some(dependency) = stack.pop() {
            let origin = historical_build_origins
                .get(&dependency)
                .expect("visited historical package has an originating seed")
                .clone();
            for edge in reverse_edges.get(dependency.as_str()).into_iter().flatten() {
                match edge.domain {
                    PortableBaseEdgeDomain::Build => {
                        if !historical_build_origins.contains_key(&edge.dependent) {
                            historical_build_origins.insert(edge.dependent.clone(), origin.clone());
                            stack.push(edge.dependent.clone());
                        }
                    }
                    PortableBaseEdgeDomain::Development => {
                        historical_development_origins
                            .entry(edge.dependent.clone())
                            .or_insert_with(|| origin.clone());
                    }
                }
            }
        }
        for (key, origin) in historical_build_origins {
            if let Some(package) = current_by_key.get(&key) {
                impact.build.insert(package.clone());
                impact.historical_build_origins.insert(package.clone(), origin);
            }
        }
        for (key, origin) in historical_development_origins {
            if let Some(package) = current_by_key.get(&key)
                && !impact.build.contains(package)
            {
                impact.development.insert(package.clone());
                impact.historical_development_origins.insert(package.clone(), origin);
            }
        }
    }
    Ok(impact)
}

fn package_owns_historical_path(root: &str, path: &str) -> bool {
    if root.is_empty() {
        path == "Cargo.toml"
    } else {
        path == format!("{root}/Cargo.toml") || path.strip_prefix(root).is_some_and(|suffix| suffix.starts_with('/'))
    }
}

fn is_global_deliverable_cargo_input(path: &str) -> bool {
    matches!(
        path,
        "Cargo.toml" | "Cargo.lock" | "rust-toolchain" | "rust-toolchain.toml"
    ) || path.starts_with(".cargo/")
}

fn prerequisite_source_roots(
    specs: &[WorkSpec],
    impact: &StructuralCargoImpact,
) -> BTreeMap<String, PrerequisiteSourceImpact> {
    if impact.workspace {
        return BTreeMap::new();
    }
    let mut additions = BTreeMap::<String, PrerequisiteSourceImpact>::new();
    for prerequisite in specs.iter().flat_map(|spec| &spec.resolved_prerequisites) {
        let impacted = prerequisite
            .require
            .iter()
            .filter(|root| impact.build.contains(&root.package))
            .collect::<Vec<_>>();
        if impacted.is_empty() {
            continue;
        }
        let addition = additions.entry(prerequisite.source_work.clone()).or_default();
        addition.roots.extend(prerequisite.when.iter().cloned());
        addition.inputs.extend(impacted.into_iter().map(|root| {
            let target = root
                .target
                .as_ref()
                .map(|target| format!(":{}", target.name))
                .unwrap_or_default();
            format!("runtime-artifact:{}{target}", root.package_key)
        }));
    }
    for addition in additions.values_mut() {
        addition.roots.sort();
        addition.roots.dedup();
    }
    additions
}

impl Evaluator<'_> {
    fn attribution(&self) -> RailResult<BTreeMap<String, WorkAttribution>> {
        let packages = self
            .ctx
            .cargo()
            .metadata()
            .workspace_packages()
            .into_iter()
            .map(|package| (portable_package_key(self.ctx, package), package))
            .collect::<BTreeMap<_, _>>();
        self.decisions
            .iter()
            .filter_map(|(id, decision)| {
                let WorkDecision::Required { scope, cause, .. } = decision else {
                    return None;
                };
                Some((|| {
                    let mut inputs = self.report_inputs.get(id).cloned().unwrap_or_default();
                    let mut selections = Vec::new();
                    if *cause == WorkCause::ForcedAll {
                        inputs.clear();
                    }
                    match scope {
                        WorkScope::Cargo {
                            selection: CargoSelection::Packages { packages: selected, .. },
                        } => {
                            for selector in selected {
                                let package = packages
                                    .get(&selector.key)
                                    .ok_or_else(|| RailError::message("selected package lacks captured attribution"))?;
                                let direct = self.structural_impact.seeds.contains(&package.id)
                                    || self.observed.work(id).is_some_and(|work| {
                                        work.inputs.iter().any(|input| {
                                            input.package.as_deref() == Some(selector.key.as_str())
                                                && self.paths.contains(&input.path.as_str())
                                        })
                                    });
                                let origin = self
                                    .structural_impact
                                    .current_build_origins
                                    .get(&package.id)
                                    .or_else(|| self.structural_impact.current_development_origins.get(&package.id))
                                    .or_else(|| self.structural_impact.historical_build_origins.get(&package.id))
                                    .or_else(|| self.structural_impact.historical_development_origins.get(&package.id));
                                selections.push(SelectionAttribution {
                                    subject: selector.key.clone(),
                                    relation: if direct {
                                        ImpactRelation::Direct
                                    } else if origin.is_some() {
                                        ImpactRelation::Dependency
                                    } else {
                                        ImpactRelation::Unattributed
                                    },
                                    origin: if direct { None } else { origin.cloned() },
                                });
                            }
                        }
                        WorkScope::Variants {
                            selection: VariantSelection::Selected { .. },
                        } => {
                            selections = self.variant_relations.get(id).cloned().unwrap_or_default();
                        }
                        _ => {}
                    }
                    selections.sort_by(|a, b| a.subject.cmp(&b.subject));
                    Ok((id.clone(), WorkAttribution { inputs, selections }))
                })())
            })
            .collect()
    }

    fn match_cargo_root(&self, root: &ResolvedCargoRoot) -> RailResult<CargoRootMatch> {
        if self.structural_impact.workspace || self.structural_impact.deliverables_workspace {
            return Ok(CargoRootMatch {
                selected: true,
                attributed_paths: BTreeSet::new(),
                origins: BTreeSet::new(),
            });
        }

        let historical = self
            .structural_impact
            .historical_build_origins
            .contains_key(&root.package);
        let mut matched = CargoRootMatch {
            selected: historical,
            attributed_paths: BTreeSet::new(),
            origins: [
                self.structural_impact.current_build_origins.get(&root.package),
                self.structural_impact.historical_build_origins.get(&root.package),
            ]
            .into_iter()
            .flatten()
            .filter(|_| historical)
            .cloned()
            .collect(),
        };
        for seed in &self.structural_impact.unattributed_seeds {
            if self.cargo_model.root_affected_by(root, seed)? {
                matched.selected = true;
                if let Some(origin) = self.structural_impact.current_build_origins.get(&root.package) {
                    matched.origins.insert(origin.clone());
                }
            }
        }

        for (seed, paths) in &self.structural_impact.seed_paths {
            if !self.cargo_model.root_affected_by(root, seed)? {
                continue;
            }
            let Some(package) = self.cargo_model.package_in_domain(&root.domain, seed) else {
                matched.selected = true;
                continue;
            };
            let selected_target = (seed == &root.package).then(|| {
                root.target
                    .as_ref()
                    .map(|target| (target.name.as_str(), target.kinds.as_slice()))
            });
            let selected_target = selected_target.flatten();
            let features = if seed == &root.package {
                root.features
                    .as_ref()
                    .map(|features| expanded_features(package, features))
                    .transpose()?
            } else if root.domain != PRIMARY_CARGO_DOMAIN {
                self.cargo_model.active_features(&root.domain, seed)
            } else if root.features.is_none() {
                None
            } else {
                matched.selected = true;
                continue;
            };
            if features.is_none() && (root.domain == PRIMARY_CARGO_DOMAIN || seed == &root.package) {
                matched.selected = true;
                matched.attributed_paths.extend(paths.iter().cloned());
                if let Some(origin) = self.structural_impact.current_build_origins.get(&root.package) {
                    matched.origins.insert(origin.clone());
                }
                continue;
            }
            let Some(features) = features else {
                matched.selected = true;
                continue;
            };
            for path in paths {
                match source_activation(self.ctx, package, selected_target, path, &features)? {
                    SourceActivation::Active => {
                        matched.selected = true;
                        matched.attributed_paths.insert(path.clone());
                        if let Some(origin) = self.structural_impact.current_build_origins.get(&root.package) {
                            matched.origins.insert(origin.clone());
                        }
                    }
                    SourceActivation::Inactive => {}
                    SourceActivation::Unknown => matched.selected = true,
                }
            }
        }
        Ok(matched)
    }

    fn cargo_selection(&self, work: &str, paths: &[&str]) -> RailResult<CargoSelection> {
        let selection = cargo_selection(self.ctx, self.structural_impact, work, paths)?;
        self.add_prerequisite_source_roots(work, selection)
    }

    fn add_prerequisite_source_roots(&self, work: &str, selection: CargoSelection) -> RailResult<CargoSelection> {
        let Some(roots) = self.prerequisite_source_roots.get(work) else {
            return Ok(selection);
        };
        merge_cargo_selections(vec![selection, selection_for_roots(self.ctx, &roots.roots)?])
    }

    fn evaluate_builtin(&mut self, spec: &WorkSpec) -> RailResult<()> {
        let config_inputs = self
            .config_deltas
            .iter()
            .filter(|delta| crate::config::schema::field_consumers(&delta.path).contains(&spec.id.as_str()))
            .map(|delta| WorkInput::new(WorkInputKind::Configuration, delta.path.clone()))
            .collect::<Vec<_>>();
        let paths = self.paths;
        let mut direct = match spec.id.as_str() {
            "cargo.fmt" => paths
                .iter()
                .filter(|path| path.ends_with(".rs") || path.ends_with("rustfmt.toml"))
                .map(|path| WorkInput::path(*path))
                .collect::<Vec<_>>(),
            "dependency-policy" => paths
                .iter()
                .filter(|path| {
                    (path.ends_with("Cargo.toml") || path.ends_with("Cargo.lock"))
                        && semantic_path_changed(self.semantic_changes, path)
                        || path.ends_with("deny.toml")
                })
                .map(|path| WorkInput::path(*path))
                .collect(),
            "release.semver" => paths
                .iter()
                .filter(|path| is_semver_input(path) && semantic_path_changed(self.semantic_changes, path))
                .map(|path| WorkInput::path(*path))
                .chain(config_inputs)
                .collect(),
            "surface" if !self.ctx.config().is_some_and(|config| config.surface.enabled) => Vec::new(),
            "surface" => paths
                .iter()
                .filter(|path| path.ends_with(".rs"))
                .map(|path| WorkInput::path(*path))
                .chain(config_inputs)
                .collect(),
            id if BUILTIN_CARGO_WORK.binary_search(&id).is_ok() => {
                cargo_direct_inputs(self.ctx, id, paths, self.semantic_changes)
            }
            _ => Vec::new(),
        };
        if let Some(prerequisite) = self.prerequisite_source_roots.get(&spec.id) {
            direct.extend(
                prerequisite
                    .inputs
                    .iter()
                    .cloned()
                    .map(|input| WorkInput::new(WorkInputKind::Prerequisite, input)),
            );
        }

        if !direct.is_empty() {
            self.report_inputs
                .insert(spec.id.clone(), direct.iter().cloned().collect());
            let input = direct
                .iter()
                .map(WorkInput::evidence_text)
                .collect::<Vec<_>>()
                .join(",");
            let evidence = self.add_evidence(
                "changed_input",
                &spec.id,
                Some(&input),
                true,
                format!("{} authoritative input(s) changed", direct.len()),
            )?;
            let scope = self.scope_for(spec, paths, evidence.clone(), None)?;
            self.decisions.insert(
                spec.id.clone(),
                WorkDecision::Required {
                    cause: WorkCause::ChangedInput,
                    scope,
                    evidence: vec![evidence],
                },
            );
            return Ok(());
        }

        if BUILTIN_CARGO_WORK.binary_search(&spec.id.as_str()).is_ok() && cargo_membership_uncertain(self.ctx, paths) {
            if let Some(decision) = self.observed_cargo_decision(spec, paths)? {
                self.decisions.insert(spec.id.clone(), decision);
                return Ok(());
            }
            let input = paths.join(",");
            let evidence = self.add_evidence(
                "compiler_inputs_incomplete",
                &spec.id,
                Some(&input),
                false,
                "portable compiler, build-script, or proc-macro negative evidence is unavailable".to_string(),
            )?;
            let selection = self.cargo_selection(&spec.id, paths)?;
            self.decisions.insert(
                spec.id.clone(),
                WorkDecision::Required {
                    cause: WorkCause::IncompleteEvidence,
                    scope: WorkScope::Cargo { selection },
                    evidence: vec![evidence],
                },
            );
            return Ok(());
        }

        let evidence = self.add_evidence(
            "complete_disjoint",
            &spec.id,
            Some(&paths.join(",")),
            true,
            "captured structural inputs are complete and disjoint from this work".to_string(),
        )?;
        self.decisions.insert(
            spec.id.clone(),
            WorkDecision::Skipped {
                evidence: vec![evidence],
            },
        );
        Ok(())
    }

    fn observed_cargo_decision(&mut self, spec: &WorkSpec, paths: &[&str]) -> RailResult<Option<WorkDecision>> {
        if let Some((code, description)) = self.observed.incompatibility() {
            let code = code.to_string();
            let description = description.to_string();
            let evidence = self.add_evidence(&code, &spec.id, None, false, description)?;
            let selection = self.cargo_selection(&spec.id, paths)?;
            return Ok(Some(WorkDecision::Required {
                cause: WorkCause::IncompleteEvidence,
                scope: WorkScope::Cargo { selection },
                evidence: vec![evidence],
            }));
        }
        let Some(observed) = self.observed.work(&spec.id).cloned() else {
            return Ok(None);
        };
        let capabilities = self.observed.capabilities().cloned().unwrap_or_default();
        let missing = required_capabilities(&spec.id)
            .iter()
            .filter(|capability| !capabilities.contains(**capability))
            .copied()
            .collect::<Vec<_>>();
        if !observed.complete || !observed.bypasses.is_empty() || !missing.is_empty() {
            let input = format!(
                "complete={},bypasses={},missing_capabilities={}",
                observed.complete,
                observed.bypasses.join("|"),
                missing.join("|")
            );
            let evidence = self.add_evidence(
                "observed_inputs_incomplete",
                &spec.id,
                Some(&input),
                false,
                "observed input evidence lacks a complete compiler, build-script, proc-macro, or process boundary"
                    .to_string(),
            )?;
            let selection = self.cargo_selection(&spec.id, paths)?;
            return Ok(Some(WorkDecision::Required {
                cause: WorkCause::IncompleteEvidence,
                scope: WorkScope::Cargo { selection },
                evidence: vec![evidence],
            }));
        }

        let changed = observed
            .inputs
            .iter()
            .filter(|input| paths.contains(&input.path.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if changed.is_empty() {
            let evidence = self.add_evidence(
                "observed_inputs_complete_disjoint",
                &spec.id,
                Some(&paths.join(",")),
                true,
                "compatible compiler, build-script, and proc-macro inputs are complete and disjoint".to_string(),
            )?;
            return Ok(Some(WorkDecision::Skipped {
                evidence: vec![evidence],
            }));
        }

        self.report_inputs.insert(
            spec.id.clone(),
            changed
                .iter()
                .map(|input| WorkInput::path(input.path.clone()))
                .collect(),
        );
        let input = changed
            .iter()
            .map(|input| format!("{}@{}", input.path, input.identity))
            .collect::<Vec<_>>()
            .join(",");
        let evidence = self.add_evidence(
            "observed_input_changed",
            &spec.id,
            Some(&input),
            true,
            "a compatible observed compiler input changed".to_string(),
        )?;
        let selection = observed_cargo_selection(self.ctx, &changed).unwrap_or(CargoSelection::Workspace {
            cargo_args: Vec::new(),
            targets: Vec::new(),
        });
        let selection = self.add_prerequisite_source_roots(&spec.id, selection)?;
        Ok(Some(WorkDecision::Required {
            cause: WorkCause::ChangedInput,
            scope: WorkScope::Cargo { selection },
            evidence: vec![evidence],
        }))
    }

    fn evaluate_declared(&mut self, spec: &WorkSpec) -> RailResult<()> {
        let path_inputs = matching_paths(&spec.paths, self.paths)?;
        let required_config = self
            .config_deltas
            .iter()
            .filter(|delta| {
                spec.config.contains(&delta.path)
                    || delta.path == format!("plan.work.{}", spec.id)
                    || delta.path.starts_with(&format!("plan.work.{}.", spec.id))
            })
            .map(|delta| delta.path.clone())
            .collect::<Vec<_>>();
        let selected_cargo = spec
            .cargo
            .iter()
            .filter(|id| matches!(self.decisions.get(*id), Some(WorkDecision::Required { .. })))
            .cloned()
            .collect::<Vec<_>>();
        let mut prerequisite_roots = Vec::new();
        let mut prerequisite_inputs = Vec::new();
        let mut incomplete_prerequisite = false;
        for prerequisite in &spec.resolved_prerequisites {
            let Some(WorkDecision::Required { cause, scope, .. }) = self.decisions.get(&prerequisite.source_work)
            else {
                continue;
            };
            let WorkScope::Cargo { selection } = scope else {
                return Err(RailError::message(format!(
                    "Cargo prerequisite source '{}' did not emit Cargo scope",
                    prerequisite.source_work
                )));
            };
            let matched = prerequisite
                .when
                .iter()
                .filter(|root| cargo_root_selected(selection, root))
                .collect::<Vec<_>>();
            if matched.is_empty() {
                continue;
            }
            incomplete_prerequisite |= *cause == WorkCause::IncompleteEvidence;
            prerequisite_inputs.extend(matched.into_iter().map(|root| {
                let target = root
                    .target
                    .as_ref()
                    .map(|target| format!(":{}", target.name))
                    .unwrap_or_default();
                format!("prerequisite:{}:{}{target}", prerequisite.source_work, root.package_key)
            }));
            prerequisite_roots.extend(prerequisite.require.iter().cloned());
        }
        prerequisite_roots.sort();
        prerequisite_roots.dedup();
        let incomplete_cargo = incomplete_prerequisite
            || selected_cargo.iter().any(|id| {
                matches!(
                    self.decisions.get(id),
                    Some(WorkDecision::Required {
                        cause: WorkCause::IncompleteEvidence,
                        ..
                    })
                )
            });
        let config_changed = !required_config.is_empty();
        let config_inputs = required_config
            .iter()
            .map(|path| WorkInput::new(WorkInputKind::Configuration, path.clone()))
            .collect::<Vec<_>>();
        let cargo_inputs = selected_cargo
            .iter()
            .map(|id| WorkInput::new(WorkInputKind::Work, id.clone()))
            .collect::<Vec<_>>();
        let mut direct = path_inputs
            .iter()
            .map(|path| WorkInput::path(path.clone()))
            .collect::<Vec<_>>();
        direct.extend(config_inputs);
        direct.extend(cargo_inputs);
        direct.extend(
            prerequisite_inputs
                .into_iter()
                .map(|input| WorkInput::new(WorkInputKind::Prerequisite, input)),
        );

        // Variant rows are a narrower authority than the work-level gate. A
        // changed work input can require the family while the catalog still
        // proves an exact row subset; only an unmatched required input widens
        // to the explicit `all` selection.
        let selected_variants = self.select_variants(spec, &path_inputs, &required_config)?;
        let mut report_inputs = direct.iter().cloned().collect::<BTreeSet<_>>();
        if let Some(selection) = &selected_variants {
            report_inputs.extend(selection.report_inputs.iter().cloned());
            self.variant_relations
                .insert(spec.id.clone(), selection.relations.clone());
        }
        self.report_inputs.insert(spec.id.clone(), report_inputs);
        if !direct.is_empty()
            || selected_variants
                .as_ref()
                .is_some_and(|selection| !selection.variants.is_empty())
        {
            let input = if direct.is_empty() {
                selected_variants
                    .as_ref()
                    .into_iter()
                    .flat_map(|selection| &selection.attributed_inputs)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(",")
            } else {
                direct
                    .iter()
                    .map(WorkInput::evidence_text)
                    .collect::<Vec<_>>()
                    .join(",")
            };
            let (code, complete, description, cause) = if incomplete_cargo {
                (
                    "subscribed_cargo_incomplete",
                    false,
                    "subscribed Cargo work lacks complete negative evidence".to_string(),
                    WorkCause::IncompleteEvidence,
                )
            } else {
                (
                    "changed_input",
                    true,
                    "a declared positive input changed".to_string(),
                    WorkCause::ChangedInput,
                )
            };
            let evidence = self.add_evidence(code, &spec.id, Some(&input), complete, description)?;
            let path_refs = path_inputs.iter().map(String::as_str).collect::<Vec<_>>();
            let scope = if spec.scope == WorkSpecScope::Cargo {
                WorkScope::Cargo {
                    selection: self.declared_cargo_selection(
                        &path_inputs,
                        config_changed,
                        &selected_cargo,
                        &prerequisite_roots,
                    )?,
                }
            } else {
                self.scope_for(spec, &path_refs, evidence.clone(), selected_variants)?
            };
            self.decisions.insert(
                spec.id.clone(),
                WorkDecision::Required {
                    cause,
                    scope,
                    evidence: vec![evidence],
                },
            );
        } else {
            let evidence = self.add_evidence(
                "declared_inputs_disjoint",
                &spec.id,
                Some(&self.paths.join(",")),
                true,
                "all declared path, config, and changed-Cargo inputs are disjoint".to_string(),
            )?;
            self.decisions.insert(
                spec.id.clone(),
                WorkDecision::Skipped {
                    evidence: vec![evidence],
                },
            );
        }
        Ok(())
    }

    fn declared_cargo_selection(
        &self,
        paths: &[String],
        config_changed: bool,
        selected_cargo: &[String],
        prerequisite_roots: &[ResolvedCargoRoot],
    ) -> RailResult<CargoSelection> {
        if config_changed {
            return Ok(CargoSelection::Workspace {
                cargo_args: Vec::new(),
                targets: Vec::new(),
            });
        }
        let mut selections = selected_cargo
            .iter()
            .filter_map(|id| match self.decisions.get(id) {
                Some(WorkDecision::Required {
                    scope: WorkScope::Cargo { selection },
                    ..
                }) => Some(selection.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        if !paths.is_empty() {
            let paths = paths.iter().map(String::as_str).collect::<Vec<_>>();
            selections.push(declared_path_selection(
                self.ctx,
                self.semantic_changes,
                self.structural_impact,
                &paths,
            )?);
        }
        if !prerequisite_roots.is_empty() {
            selections.push(selection_for_roots(self.ctx, prerequisite_roots)?);
        }
        merge_cargo_selections(selections)
    }

    fn select_variants(
        &self,
        spec: &WorkSpec,
        required_paths: &[String],
        required_config: &[String],
    ) -> RailResult<Option<SelectedVariants>> {
        let Some(catalog) = &spec.catalog else {
            return Ok(None);
        };
        let mut unmatched_paths = required_paths.iter().map(String::as_str).collect::<HashSet<_>>();
        let mut unmatched_config = required_config.iter().map(String::as_str).collect::<HashSet<_>>();
        let mut selected = Vec::new();
        let mut report_inputs = BTreeSet::new();
        let mut relations = Vec::new();
        let mut attributed_inputs = BTreeSet::new();
        for row in &catalog.variants {
            let matched_paths = matching_paths(&row.external_paths, self.paths)?;
            for path in &matched_paths {
                unmatched_paths.remove(path.as_str());
                attributed_inputs.insert(format!("path:{path}->variant:{}", row.id));
            }
            let mut matched = !matched_paths.is_empty();
            let mut direct = matched;
            let mut origins = BTreeSet::new();
            report_inputs.extend(matched_paths.iter().cloned().map(WorkInput::path));
            for delta in self
                .config_deltas
                .iter()
                .filter(|delta| row.config.contains(&delta.path))
            {
                matched = true;
                direct = true;
                report_inputs.insert(WorkInput::new(WorkInputKind::Configuration, delta.path.clone()));
                unmatched_config.remove(delta.path.as_str());
                attributed_inputs.insert(format!("config:{}->variant:{}", delta.path, row.id));
            }
            if !row.resolved_cargo_roots.is_empty() {
                for root in &row.resolved_cargo_roots {
                    let cargo_match = self.match_cargo_root(root)?;
                    matched |= cargo_match.selected;
                    if cargo_match.selected {
                        if self.structural_impact.seeds.contains(&root.package) {
                            direct = true;
                            report_inputs.insert(WorkInput::new(WorkInputKind::Package, root.package_key.clone()));
                        } else {
                            for origin in &cargo_match.origins {
                                report_inputs.insert(WorkInput::new(WorkInputKind::Dependency, origin.clone()));
                                origins.insert(origin.clone());
                            }
                        }
                    }
                    for path in &cargo_match.attributed_paths {
                        unmatched_paths.remove(path.as_str());
                    }
                    if cargo_match.origins.is_empty() {
                        attributed_inputs.extend(
                            cargo_match
                                .attributed_paths
                                .iter()
                                .map(|path| format!("cargo:{path}->variant:{}", row.id)),
                        );
                    } else {
                        attributed_inputs.extend(
                            cargo_match
                                .origins
                                .iter()
                                .map(|origin| format!("cargo:{origin}->variant:{}", row.id)),
                        );
                    }
                }
                if matched && (self.structural_impact.workspace || self.structural_impact.deliverables_workspace) {
                    attributed_inputs.insert(format!("cargo:workspace->variant:{}", row.id));
                }
            }
            if matched {
                relations.push(SelectionAttribution {
                    subject: row.id.clone(),
                    relation: if direct {
                        ImpactRelation::Direct
                    } else if origins.is_empty() {
                        ImpactRelation::Unattributed
                    } else {
                        ImpactRelation::Dependency
                    },
                    origin: if direct { None } else { origins.into_iter().next() },
                });
                selected.push(SelectedVariant {
                    id: row.id.clone(),
                    dimensions: row.dimensions.clone(),
                });
            }
        }
        Ok(Some(SelectedVariants {
            report_inputs,
            relations,
            variants: selected,
            attributed_inputs,
            all_required_inputs_attributed: unmatched_paths.is_empty() && unmatched_config.is_empty(),
        }))
    }

    fn scope_for(
        &self,
        spec: &WorkSpec,
        paths: &[&str],
        evidence: String,
        variants: Option<SelectedVariants>,
    ) -> RailResult<WorkScope> {
        Ok(match spec.scope {
            WorkSpecScope::Repository => WorkScope::Repository,
            WorkSpecScope::Cargo => WorkScope::Cargo {
                selection: self.cargo_selection(&spec.id, paths)?,
            },
            WorkSpecScope::Variants => WorkScope::Variants {
                selection: match variants {
                    Some(selection) if selection.all_required_inputs_attributed && !selection.variants.is_empty() => {
                        VariantSelection::Selected {
                            variants: selection.variants,
                            evidence,
                        }
                    }
                    Some(_) | None => VariantSelection::All { evidence },
                },
            },
        })
    }

    fn force_all(&mut self, specs: &[WorkSpec]) -> RailResult<()> {
        for spec in specs {
            let evidence = self.add_evidence(
                "forced_all",
                &spec.id,
                None,
                true,
                "the monotonic --all override requires full valid scope".to_string(),
            )?;
            let scope = match spec.scope {
                WorkSpecScope::Repository => WorkScope::Repository,
                WorkSpecScope::Cargo => WorkScope::Cargo {
                    selection: CargoSelection::Workspace {
                        cargo_args: Vec::new(),
                        targets: Vec::new(),
                    },
                },
                WorkSpecScope::Variants => WorkScope::Variants {
                    selection: VariantSelection::All {
                        evidence: evidence.clone(),
                    },
                },
            };
            self.decisions.insert(
                spec.id.clone(),
                WorkDecision::Required {
                    cause: WorkCause::ForcedAll,
                    scope,
                    evidence: vec![evidence],
                },
            );
        }
        Ok(())
    }

    fn add_evidence(
        &mut self,
        code: &str,
        subject: &str,
        input: Option<&str>,
        complete: bool,
        description: String,
    ) -> RailResult<String> {
        let identity_value = EvidenceIdentity {
            code,
            subject,
            input,
            complete,
        };
        let id = identity("evidence", &canonical_bytes(&identity_value)?)?;
        self.evidence.entry(id.clone()).or_insert_with(|| EvidenceRecord {
            code: code.to_string(),
            subject: subject.to_string(),
            description,
            input: input.map(str::to_string),
            complete,
        });
        Ok(id)
    }
}

fn cargo_root_selected(selection: &CargoSelection, root: &ResolvedCargoRoot) -> bool {
    match selection {
        CargoSelection::Workspace { .. } => true,
        CargoSelection::Packages { packages, targets, .. } => {
            if !packages.iter().any(|package| package.key == root.package_key) {
                return false;
            }
            let Some(root_target) = &root.target else {
                return true;
            };
            targets.is_empty()
                || targets.iter().any(|target| {
                    (target.package.as_str(), target.name.as_str(), target.kind.as_slice())
                        == (
                            root.package_key.as_str(),
                            root_target.name.as_str(),
                            root_target.kinds.as_slice(),
                        )
                })
        }
    }
}

fn merge_cargo_selections(selections: Vec<CargoSelection>) -> RailResult<CargoSelection> {
    if selections.is_empty() {
        return Ok(CargoSelection::Workspace {
            cargo_args: Vec::new(),
            targets: Vec::new(),
        });
    }
    let mut packages = BTreeMap::<String, PortablePackageSelector>::new();
    let mut targets = BTreeSet::new();
    for selection in selections {
        let (selected, selected_targets) = match selection {
            CargoSelection::Packages { packages, targets, .. } => (packages, targets),
            CargoSelection::Workspace { .. } => {
                return Ok(CargoSelection::Workspace {
                    cargo_args: Vec::new(),
                    targets: Vec::new(),
                });
            }
        };
        for package in selected {
            if let Some(existing) = packages.insert(package.key.clone(), package.clone())
                && existing != package
            {
                return Err(RailError::message(format!(
                    "Cargo package selector '{}' disagrees across subscribed work",
                    package.key
                )));
            }
        }
        targets.extend(selected_targets);
    }
    let packages = packages.into_values().collect::<Vec<_>>();
    let cargo_args = packages
        .iter()
        .flat_map(|package| ["-p".to_string(), package.cargo_spec.clone()])
        .collect();
    Ok(CargoSelection::Packages {
        packages,
        cargo_args,
        targets: targets.into_iter().collect(),
    })
}

fn selection_for_roots(ctx: &WorkspaceContext, roots: &[ResolvedCargoRoot]) -> RailResult<CargoSelection> {
    let package_ids = roots.iter().map(|root| &root.package).collect::<BTreeSet<_>>();
    let mut packages = ctx
        .cargo()
        .metadata()
        .workspace_packages()
        .iter()
        .filter(|package| package_ids.contains(&package.id))
        .copied()
        .collect::<Vec<_>>();
    let mut selection = selection_for_packages(ctx, &mut packages, &[])?;
    let targets = roots
        .iter()
        .filter_map(|root| {
            root.target.as_ref().map(|target| CargoTargetSelector {
                package: root.package_key.clone(),
                name: target.name.clone(),
                kind: target.kinds.clone(),
            })
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    match &mut selection {
        CargoSelection::Packages {
            targets: selected_targets,
            ..
        } => *selected_targets = targets,
        CargoSelection::Workspace { .. } => {
            return Err(RailError::message("resolved Cargo roots produced workspace scope"));
        }
    }
    Ok(selection)
}

fn cargo_direct_inputs(
    ctx: &WorkspaceContext,
    id: &str,
    paths: &[&str],
    semantic_changes: &BTreeMap<String, SemanticFileChange>,
) -> Vec<WorkInput> {
    paths
        .iter()
        .filter(|path| {
            if is_cargo_structural_input(path) {
                return semantic_path_changed(semantic_changes, path);
            }
            if id == "cargo.package"
                && ctx
                    .graph()
                    .file_to_package(Path::new(path))
                    .is_some_and(|package| package.is_workspace_member)
            {
                // Packaging owns the package source tree, not only compiler
                // reads. Directory ownership is therefore a conservative
                // positive package input here, never compiler membership.
                return true;
            }
            if !path.ends_with(".rs") || !is_current_target_root(ctx, path) {
                return false;
            }
            match id {
                "cargo.build" => !is_test_only_path(path),
                "cargo.doc" | "cargo.doctest" => !is_test_only_path(path),
                "cargo.package" => true,
                "cargo.clippy" | "cargo.test" => true,
                _ => false,
            }
        })
        .map(|path| WorkInput::path(*path))
        .collect()
}

fn semantic_path_changed(semantic_changes: &BTreeMap<String, SemanticFileChange>, path: &str) -> bool {
    !matches!(
        semantic_changes.get(path).map(|change| &change.scope),
        Some(SemanticScope::None)
    )
}

fn is_cargo_structural_input(path: &str) -> bool {
    path.ends_with("Cargo.toml")
        || path.ends_with("Cargo.lock")
        || path.ends_with("build.rs")
        || path.ends_with("rust-toolchain")
        || path.ends_with("rust-toolchain.toml")
        || path.contains(".cargo/config")
}

fn is_semver_input(path: &str) -> bool {
    path.ends_with(".rs") || path.ends_with("Cargo.toml") || path.ends_with("Cargo.lock")
}

fn is_test_only_path(path: &str) -> bool {
    path.starts_with("tests/") || path.contains("/tests/") || path.starts_with("benches/") || path.contains("/benches/")
}

fn cargo_membership_uncertain(ctx: &WorkspaceContext, paths: &[&str]) -> bool {
    paths.iter().any(|path| {
        !is_cargo_structural_input(path)
            && !(path.ends_with(".rs") && is_current_target_root(ctx, path))
            && !path.ends_with("rustfmt.toml")
            && !path.ends_with("deny.toml")
    })
}

fn is_current_target_root(ctx: &WorkspaceContext, path: &str) -> bool {
    ctx.cargo().metadata().workspace_packages().iter().any(|package| {
        package.targets.iter().any(|target| {
            target
                .src_path
                .as_std_path()
                .strip_prefix(ctx.workspace_root())
                .ok()
                .map(crate::utils::path_to_git_format)
                .as_deref()
                == Some(path)
        })
    })
}

fn matching_paths(patterns: &[String], files: &[&str]) -> RailResult<Vec<String>> {
    let patterns = patterns
        .iter()
        .map(|pattern| {
            Pattern::new(pattern)
                .map_err(|error| RailError::message(format!("invalid declared work glob '{pattern}': {error}")))
        })
        .collect::<RailResult<Vec<_>>>()?;
    Ok(files
        .iter()
        .filter(|path| patterns.iter().any(|pattern| pattern.matches(path)))
        .map(|path| (*path).to_string())
        .collect())
}

fn cargo_selection(
    ctx: &WorkspaceContext,
    impact: &StructuralCargoImpact,
    work: &str,
    paths: &[&str],
) -> RailResult<CargoSelection> {
    if impact.workspace {
        return Ok(CargoSelection::Workspace {
            cargo_args: Vec::new(),
            targets: Vec::new(),
        });
    }
    let mut selected = match work {
        "cargo.build" | "cargo.doc" | "cargo.doctest" => impact.build.clone(),
        "cargo.clippy" | "cargo.test" => {
            let mut selected = impact.build.clone();
            selected.extend(impact.development.iter().cloned());
            selected
        }
        _ => impact.seeds.iter().cloned().collect(),
    };

    // Dependency packages seed reverse-impact propagation, but Cargo work is
    // executed against this workspace. Never lower a registry, Git, or
    // non-member path dependency into `cargo --package`.
    selected.retain(|package| ctx.cargo().metadata().workspace_members.contains(package));
    if selected.is_empty() {
        return Ok(CargoSelection::Workspace {
            cargo_args: Vec::new(),
            targets: Vec::new(),
        });
    }

    let packages_by_id = ctx
        .cargo()
        .metadata()
        .packages
        .iter()
        .map(|package| (&package.id, package))
        .collect::<BTreeMap<_, _>>();
    let mut packages = selected
        .iter()
        .map(|id| {
            packages_by_id
                .get(id)
                .copied()
                .ok_or_else(|| RailError::message(format!("selected Cargo package '{id}' is absent from metadata")))
        })
        .collect::<RailResult<Vec<_>>>()?;
    selection_for_packages(ctx, &mut packages, paths)
}

fn declared_path_selection(
    ctx: &WorkspaceContext,
    semantic_changes: &BTreeMap<String, SemanticFileChange>,
    impact: &StructuralCargoImpact,
    paths: &[&str],
) -> RailResult<CargoSelection> {
    if paths.iter().any(|path| {
        matches!(
            semantic_changes.get(*path).map(|change| &change.scope),
            Some(SemanticScope::Workspace)
        )
    }) {
        if impact.workspace {
            return Ok(CargoSelection::Workspace {
                cargo_args: Vec::new(),
                targets: Vec::new(),
            });
        }
        let mut packages = ctx
            .cargo()
            .metadata()
            .workspace_packages()
            .iter()
            .filter(|package| impact.build.contains(&package.id))
            .copied()
            .collect::<Vec<_>>();
        if packages.is_empty() {
            return Ok(CargoSelection::Workspace {
                cargo_args: Vec::new(),
                targets: Vec::new(),
            });
        }
        return selection_for_packages(ctx, &mut packages, paths);
    }

    let mut selected = HashSet::new();
    for path in paths {
        match semantic_changes.get(*path).map(|change| &change.scope) {
            Some(SemanticScope::None) => {}
            Some(SemanticScope::Packages(packages)) => selected.extend(packages.iter().cloned()),
            Some(SemanticScope::Workspace) => {
                return Err(RailError::message(
                    "workspace semantic scope escaped declared-path selection",
                ));
            }
            None => {
                if let Some(package) = ctx
                    .graph()
                    .file_to_package(Path::new(path))
                    .filter(|package| package.is_workspace_member)
                {
                    selected.insert(package.id.clone());
                }
            }
        }
    }
    let mut packages = ctx
        .cargo()
        .metadata()
        .workspace_packages()
        .iter()
        .filter(|package| selected.contains(&package.id))
        .copied()
        .collect::<Vec<_>>();
    if packages.is_empty() {
        return Ok(CargoSelection::Workspace {
            cargo_args: Vec::new(),
            targets: Vec::new(),
        });
    }
    selection_for_packages(ctx, &mut packages, paths)
}

fn selection_for_packages(
    ctx: &WorkspaceContext,
    packages: &mut Vec<&Package>,
    paths: &[&str],
) -> RailResult<CargoSelection> {
    packages.sort_unstable_by_key(|package| portable_package_key(ctx, package));
    packages.dedup_by(|left, right| left.id == right.id);
    let duplicate_names = ctx.cargo().metadata().workspace_packages().iter().fold(
        BTreeMap::<&str, usize>::new(),
        |mut counts, package| {
            *counts.entry(package.name.as_str()).or_default() += 1;
            counts
        },
    );

    let selectors = packages
        .iter()
        .map(|package| portable_package_selector(ctx, package, &duplicate_names))
        .collect::<RailResult<Vec<_>>>()?;
    let cargo_args = selectors
        .iter()
        .flat_map(|package| ["-p".to_string(), package.cargo_spec.clone()])
        .collect();
    let targets = target_selectors(ctx, packages, paths);
    Ok(CargoSelection::Packages {
        packages: selectors,
        cargo_args,
        targets,
    })
}

fn portable_package_selector(
    ctx: &WorkspaceContext,
    package: &Package,
    duplicate_names: &BTreeMap<&str, usize>,
) -> RailResult<PortablePackageSelector> {
    let duplicates = duplicate_names.get(package.name.as_str()).copied().unwrap_or_default();
    let cargo_spec = if duplicates <= 1 {
        package.name.to_string()
    } else {
        let same_version = ctx
            .cargo()
            .metadata()
            .workspace_packages()
            .iter()
            .filter(|candidate| candidate.name == package.name && candidate.version == package.version)
            .count();
        if same_version > 1 {
            return Err(RailError::with_help(
                format!(
                    "workspace package '{}' version {} is ambiguous for Cargo -p lowering",
                    package.name, package.version
                ),
                "give workspace packages unique names or versions before consuming exact planner selectors",
            ));
        }
        format!("{}@{}", package.name, package.version)
    };
    Ok(PortablePackageSelector {
        key: portable_package_key(ctx, package),
        name: package.name.to_string(),
        cargo_spec,
    })
}

fn portable_package_key(ctx: &WorkspaceContext, package: &Package) -> String {
    if let Some(source) = &package.source {
        return format!("{}@{}#{}", package.name, package.version, source.repr);
    }
    let relative = package
        .manifest_path
        .parent()
        .and_then(|root| {
            root.as_std_path()
                .strip_prefix(ctx.planning_authority_source_root())
                .ok()
        })
        .map(crate::utils::path_to_git_format)
        .unwrap_or_else(|| "<outside-workspace>".to_string());
    format!("{}@{}#path:{relative}", package.name, package.version)
}

fn target_selectors(ctx: &WorkspaceContext, packages: &[&Package], paths: &[&str]) -> Vec<CargoTargetSelector> {
    let path_set = paths.iter().copied().collect::<BTreeSet<_>>();
    let mut targets = Vec::new();
    for package in packages {
        let package_key = portable_package_key(ctx, package);
        for target in &package.targets {
            let Some(path) = target
                .src_path
                .as_std_path()
                .strip_prefix(ctx.workspace_root())
                .ok()
                .map(crate::utils::path_to_git_format)
            else {
                continue;
            };
            if !path_set.contains(path.as_str()) {
                continue;
            }
            let mut kind = target.kind.iter().map(ToString::to_string).collect::<Vec<_>>();
            kind.sort_unstable();
            targets.push(CargoTargetSelector {
                package: package_key.clone(),
                name: target.name.clone(),
                kind,
            });
        }
    }
    targets.sort();
    targets.dedup();
    targets
}

fn cargo_structural_deltas(
    ctx: &WorkspaceContext,
    semantic_changes: &BTreeMap<String, SemanticFileChange>,
) -> Vec<CargoStructuralDelta> {
    let packages = ctx
        .cargo()
        .metadata()
        .packages
        .iter()
        .map(|package| (&package.id, package))
        .collect::<BTreeMap<_, _>>();
    let mut deltas = Vec::new();
    for change in semantic_changes.values() {
        let kind = change.fallback.map_or_else(
            || change.input.to_string(),
            |fallback| format!("{}:{fallback}", change.input),
        );
        match &change.scope {
            SemanticScope::None => {}
            SemanticScope::Packages(package_ids) => {
                for package_id in package_ids {
                    if let Some(package) = packages.get(package_id) {
                        deltas.push(CargoStructuralDelta {
                            package: portable_package_key(ctx, package),
                            target: None,
                            kind: kind.clone(),
                        });
                    }
                }
            }
            SemanticScope::Workspace => deltas.push(CargoStructuralDelta {
                package: "*".to_string(),
                target: None,
                kind,
            }),
        }
    }
    deltas.sort_unstable_by(|left, right| {
        (&left.package, &left.target, &left.kind).cmp(&(&right.package, &right.target, &right.kind))
    });
    deltas.dedup_by(|left, right| {
        left.package == right.package && left.target == right.target && left.kind == right.kind
    });
    deltas
}

fn observed_cargo_selection(ctx: &WorkspaceContext, inputs: &[ObservedInput]) -> Option<CargoSelection> {
    let package_keys = inputs
        .iter()
        .map(|input| input.package.as_deref())
        .collect::<Option<BTreeSet<_>>>()?;
    if package_keys.is_empty() {
        return None;
    }
    let mut packages = ctx
        .cargo()
        .metadata()
        .workspace_packages()
        .iter()
        .filter(|package| package_keys.contains(portable_package_key(ctx, package).as_str()))
        .copied()
        .collect::<Vec<_>>();
    if packages.len() != package_keys.len() {
        return None;
    }
    packages.sort_unstable_by_key(|package| portable_package_key(ctx, package));
    let duplicate_names = ctx.cargo().metadata().workspace_packages().iter().fold(
        BTreeMap::<&str, usize>::new(),
        |mut counts, package| {
            *counts.entry(package.name.as_str()).or_default() += 1;
            counts
        },
    );
    let selectors = packages
        .iter()
        .map(|package| portable_package_selector(ctx, package, &duplicate_names))
        .collect::<RailResult<Vec<_>>>()
        .ok()?;
    let cargo_args = selectors
        .iter()
        .flat_map(|package| ["-p".to_string(), package.cargo_spec.clone()])
        .collect();
    let mut targets = inputs
        .iter()
        .filter_map(|input| input.package.as_ref().zip(input.target.as_ref()))
        .filter_map(|(package_key, target_name)| {
            let package = packages
                .iter()
                .find(|package| portable_package_key(ctx, package) == *package_key)?;
            let target = package.targets.iter().find(|target| target.name == *target_name)?;
            let mut kind = target.kind.iter().map(ToString::to_string).collect::<Vec<_>>();
            kind.sort_unstable();
            Some(CargoTargetSelector {
                package: package_key.clone(),
                name: target_name.clone(),
                kind,
            })
        })
        .collect::<Vec<_>>();
    targets.sort();
    targets.dedup();
    Some(CargoSelection::Packages {
        packages: selectors,
        cargo_args,
        targets,
    })
}

fn catalog_identity(specs: &[WorkSpec]) -> RailResult<String> {
    let identities = specs
        .iter()
        .map(|spec| WorkSpecIdentity {
            id: &spec.id,
            origin: spec.origin,
            scope: spec.scope,
            paths: &spec.paths,
            config: &spec.config,
            cargo: &spec.cargo,
            cargo_prerequisites: &spec.cargo_prerequisites,
            variant_catalog: spec.variant_catalog.as_deref(),
            variant_catalog_identity: spec.variant_catalog_identity.as_deref(),
        })
        .collect::<Vec<_>>();
    let value = serde_json::json!({
        "work_catalog_version": WORK_CATALOG_VERSION,
        "work": identities,
    });
    identity("work-catalog-v1", &canonical_bytes(&value)?)
}

fn plan_identity(plan: &WorkPlan) -> RailResult<String> {
    #[derive(Serialize)]
    struct Portable<'a> {
        plan_contract_version: u32,
        inputs: &'a WorkPlanInputs,
        changes: &'a WorkChanges,
        work: &'a BTreeMap<String, WorkDecision>,
        required: &'a [String],
        evidence: Vec<&'a str>,
        attribution: &'a BTreeMap<String, WorkAttribution>,
    }
    identity(
        "plan-v9",
        &canonical_bytes(&Portable {
            plan_contract_version: plan.plan_contract_version,
            inputs: &plan.inputs,
            changes: &plan.changes,
            work: &plan.work,
            required: &plan.required,
            // Evidence IDs already bind code, subject, input, and
            // completeness. Human descriptions deliberately remain outside
            // the portable plan identity so wording can improve independently.
            evidence: plan.evidence.keys().map(String::as_str).collect(),
            attribution: &plan.attribution,
        })?,
    )
}

fn identity(prefix: &str, bytes: &[u8]) -> RailResult<String> {
    crate::instrumentation::record_hash(bytes.len());
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        hex.push(char::from(HEX[usize::from(byte >> 4)]));
        hex.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(format!("{prefix}:sha256:{hex}"))
}

fn canonical_bytes<T: Serialize>(value: &T) -> RailResult<Vec<u8>> {
    let value = serde_json::to_value(value)
        .map_err(|error| RailError::message(format!("failed to serialize planner identity: {error}")))?;
    serde_json::to_vec(&canonicalize(value))
        .map_err(|error| RailError::message(format!("failed to serialize canonical planner identity: {error}")))
}

fn canonicalize(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries = object.into_iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, canonicalize(value)))
                    .collect(),
            )
        }
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize).collect()),
        other => other,
    }
}

fn validate_decisions(
    specs: &[WorkSpec],
    decisions: &BTreeMap<String, WorkDecision>,
    required: &[String],
    evidence: &BTreeMap<String, EvidenceRecord>,
) -> RailResult<()> {
    if decisions.len() != specs.len() {
        return Err(RailError::message(
            "v9 planner did not decide every registered work item",
        ));
    }
    for spec in specs {
        let decision = decisions
            .get(&spec.id)
            .ok_or_else(|| RailError::message(format!("v9 planner omitted work '{}'", spec.id)))?;
        let refs = match decision {
            WorkDecision::Skipped { evidence } | WorkDecision::Required { evidence, .. } => evidence,
        };
        if refs.is_empty() || refs.iter().any(|id| !evidence.contains_key(id)) {
            return Err(RailError::message(format!(
                "v8 work decision '{}' has missing evidence",
                spec.id
            )));
        }
    }
    let projected = decisions
        .iter()
        .filter_map(|(id, decision)| matches!(decision, WorkDecision::Required { .. }).then_some(id.as_str()))
        .collect::<Vec<_>>();
    if projected != required.iter().map(String::as_str).collect::<Vec<_>>() {
        return Err(RailError::message("v8 required-work projection drifted from decisions"));
    }
    Ok(())
}

fn sorted_unique(values: Vec<String>) -> Vec<String> {
    values.into_iter().collect::<BTreeSet<_>>().into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_dimensions_reject_non_scalars() {
        serde_json::from_str::<ScalarDimension>("null").unwrap_err();
        serde_json::from_str::<ScalarDimension>("[]").unwrap_err();
        serde_json::from_str::<ScalarDimension>("{}").unwrap_err();
    }

    #[test]
    fn canonical_identity_ignores_object_insertion_order() {
        let left = serde_json::json!({"b": 2, "a": 1});
        let right = serde_json::json!({"a": 1, "b": 2});
        assert_eq!(
            identity("test", &canonical_bytes(&left).unwrap()).unwrap(),
            identity("test", &canonical_bytes(&right).unwrap()).unwrap()
        );
    }

    #[test]
    fn plan_identity_excludes_human_evidence_descriptions() {
        let evidence_id = format!("evidence:sha256:{}", "1".repeat(64));
        let mut plan = WorkPlan {
            plan_contract_version: PLAN_CONTRACT_VERSION,
            identity: String::new(),
            inputs: WorkPlanInputs {
                base: "base".to_string(),
                head: "WORKTREE".to_string(),
                head_commit: "0".repeat(40),
                capture: None,
                cargo: format!("resolution-universe-v1:sha256:{}", "2".repeat(64)),
                configuration: format!("cargo-configuration-v1:sha256:{}", "3".repeat(64)),
                toolchain: "toolchain".to_string(),
                target: format!("planning-target-v1:sha256:{}", "4".repeat(64)),
                platform: "test-platform".to_string(),
                catalog: format!("work-catalog-v1:sha256:{}", "5".repeat(64)),
                evidence: Vec::new(),
                r#override: PlanOverride::None,
            },
            changes: WorkChanges {
                files: Vec::new(),
                cargo: Vec::new(),
                config: Vec::new(),
            },
            work: BTreeMap::from([(
                "cargo.build".to_string(),
                WorkDecision::Skipped {
                    evidence: vec![evidence_id.clone()],
                },
            )]),
            required: Vec::new(),
            attribution: BTreeMap::new(),
            evidence: BTreeMap::from([(
                evidence_id,
                EvidenceRecord {
                    code: "complete_disjoint".to_string(),
                    subject: "cargo.build".to_string(),
                    description: "first wording".to_string(),
                    input: None,
                    complete: true,
                },
            )]),
        };
        let first = plan_identity(&plan).unwrap();
        plan.evidence.values_mut().next().unwrap().description = "clearer wording".to_string();
        assert_eq!(plan_identity(&plan).unwrap(), first);
    }
}
