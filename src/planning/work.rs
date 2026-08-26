//! Evidence-backed named-work decisions for the v8 planner contract.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use cargo_metadata::Package;
use glob::Pattern;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::evidence::{
    EvidenceBindings, ObservedInput, PlanningEvidenceState, PortableBaseEdgeDomain, PortableBaseModel,
    required_capabilities,
};
use super::{ConfigDelta, IndexedFileChange, PlanningIndex};
use crate::change_detection::semantic::{SemanticFileChange, SemanticScope};
use crate::config::{PlanWorkConfig, PlanWorkScope};
use crate::error::{RailError, RailResult};
use crate::graph::{DependencyUniverse, ImpactDomain};
use crate::workspace::WorkspaceContext;

const PLAN_CONTRACT_VERSION: u32 = 8;
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

/// Complete v8 named-work plan.
#[derive(Debug, Serialize)]
pub(crate) struct WorkPlan {
    pub(crate) plan_contract_version: u32,
    pub(crate) identity: String,
    pub(crate) inputs: WorkPlanInputs,
    pub(crate) changes: WorkChanges,
    pub(crate) work: BTreeMap<String, WorkDecision>,
    pub(crate) required: Vec<String>,
    pub(crate) evidence: BTreeMap<String, EvidenceRecord>,
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

pub(crate) fn format_work_plan(plan: &WorkPlan, explain: bool) -> String {
    let skipped = plan.work.len().saturating_sub(plan.required.len());
    let fallbacks = plan
        .work
        .values()
        .filter(|decision| {
            matches!(
                decision,
                WorkDecision::Required {
                    cause: WorkCause::IncompleteEvidence,
                    ..
                }
            )
        })
        .count();
    let mut output = format!(
        "plan\n\nrequired work: {}\nskipped work: {skipped}\nfallback work: {fallbacks}\nchanged inputs: {}\noverride: {}\n",
        if plan.required.is_empty() {
            "none".to_string()
        } else {
            plan.required.join(", ")
        },
        plan.changes.files.len() + plan.changes.cargo.len() + plan.changes.config.len(),
        match plan.inputs.r#override {
            PlanOverride::None => "none",
            PlanOverride::All => "all",
        }
    );
    if explain {
        output.push('\n');
        for (id, decision) in &plan.work {
            let (state, refs) = match decision {
                WorkDecision::Skipped { evidence } => ("skipped".to_string(), evidence),
                WorkDecision::Required { cause, evidence, .. } => (
                    format!(
                        "required ({})",
                        match cause {
                            WorkCause::ChangedInput => "changed input",
                            WorkCause::IncompleteEvidence => "incomplete evidence",
                            WorkCause::ForcedAll => "forced all",
                        }
                    ),
                    evidence,
                ),
            };
            output.push_str(&format!("{id}: {state}\n"));
            for evidence in refs.iter().filter_map(|reference| plan.evidence.get(reference)) {
                output.push_str(&format!("  {}\n", evidence.description));
            }
        }
    }
    output
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
    paths: Vec<String>,
    #[serde(default)]
    config: Vec<String>,
    #[serde(default)]
    cargo: Vec<String>,
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
    index: &'a PlanningIndex,
    dependency_universe: &'a DependencyUniverse,
    semantic_changes: &'a BTreeMap<String, SemanticFileChange>,
    observed: &'a PlanningEvidenceState,
    evidence: BTreeMap<String, EvidenceRecord>,
    decisions: BTreeMap<String, WorkDecision>,
}

pub(crate) fn build_work_plan(
    ctx: &WorkspaceContext,
    index: &PlanningIndex,
    authority: WorkPlanAuthority,
    dependency_universe: &DependencyUniverse,
    semantic_changes: &BTreeMap<String, SemanticFileChange>,
    evidence_path: Option<&Path>,
    force_all: bool,
) -> RailResult<WorkPlan> {
    let specs = build_catalog(ctx)?;
    let catalog = catalog_identity(&specs)?;
    let observed = PlanningEvidenceState::load(
        evidence_path,
        EvidenceBindings {
            source_base: &authority.base,
            cargo_configuration_identity: &authority.cargo_configuration_identity,
            toolchain_identity: &authority.toolchain_identity,
            target_identity: &authority.target_identity,
        },
        dependency_universe,
        ctx,
    );
    let mut evaluator = Evaluator {
        ctx,
        index,
        dependency_universe,
        semantic_changes,
        observed: &observed,
        evidence: BTreeMap::new(),
        decisions: BTreeMap::new(),
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

    let mut plan = WorkPlan {
        plan_contract_version: PLAN_CONTRACT_VERSION,
        identity: String::new(),
        inputs: WorkPlanInputs {
            base: authority.base,
            head: authority.head,
            head_commit: authority.head_commit,
            capture: authority.capture,
            cargo: dependency_universe.identity().to_string(),
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
            files: index.file_changes().to_vec(),
            cargo: cargo_structural_deltas(ctx, semantic_changes),
            config: index.config_deltas().to_vec(),
        },
        work: evaluator.decisions,
        required,
        evidence: evaluator.evidence,
    };
    plan.identity = plan_identity(&plan)?;
    Ok(plan)
}

fn build_catalog(ctx: &WorkspaceContext) -> RailResult<Vec<WorkSpec>> {
    let mut specs = BUILTIN_WORK
        .iter()
        .map(|(id, scope)| WorkSpec {
            id: (*id).to_string(),
            origin: "builtin",
            scope: *scope,
            paths: Vec::new(),
            config: Vec::new(),
            cargo: Vec::new(),
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
            specs.push(declared_spec(ctx, id, work)?);
        }
    }
    specs.sort_unstable_by(|left, right| left.id.cmp(&right.id));
    Ok(specs)
}

fn declared_spec(ctx: &WorkspaceContext, id: &str, work: &PlanWorkConfig) -> RailResult<WorkSpec> {
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
        validate_variant_catalog(id, path, &mut catalog)?;
        let identity = identity("variant-catalog-v1", &canonical_bytes(&catalog)?)?;
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
        variant_catalog: work.variant_catalog.clone(),
        catalog,
        variant_catalog_identity,
    })
}

fn validate_variant_catalog(id: &str, path: &str, catalog: &mut VariantCatalog) -> RailResult<()> {
    if catalog.variant_catalog_version != 1 {
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
    let mut ids = BTreeSet::new();
    for row in &mut catalog.variants {
        crate::config::plan::validate_stable_id(&row.id, &format!("{path}.variants.id")).map_err(RailError::Config)?;
        if !ids.insert(row.id.clone()) {
            return Err(RailError::message(format!(
                "variant catalog '{path}' contains duplicate variant '{}'",
                row.id
            )));
        }
        if row.paths.is_empty() && row.config.is_empty() && row.cargo.is_empty() {
            return Err(RailError::message(format!(
                "variant '{}' in '{path}' has no input selectors",
                row.id
            )));
        }
        crate::config::plan::validate_unique(&row.paths, &format!("{path}.{}.paths", row.id))
            .map_err(RailError::Config)?;
        crate::config::plan::validate_unique(&row.config, &format!("{path}.{}.config", row.id))
            .map_err(RailError::Config)?;
        crate::config::plan::validate_unique(&row.cargo, &format!("{path}.{}.cargo", row.id))
            .map_err(RailError::Config)?;
        for pattern in &row.paths {
            crate::config::plan::validate_positive_path(pattern, &format!("{path}.{}.paths", row.id), true)
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
        for cargo in &row.cargo {
            if crate::config::plan::CARGO_WORK_IDS
                .binary_search(&cargo.as_str())
                .is_err()
            {
                return Err(RailError::message(format!(
                    "variant '{}' in '{path}' names unknown Cargo work '{cargo}'",
                    row.id
                )));
            }
        }
        row.paths.sort_unstable();
        row.config.sort_unstable();
        row.cargo.sort_unstable();
    }
    catalog.variants.sort_unstable_by(|left, right| left.id.cmp(&right.id));
    Ok(())
}

impl Evaluator<'_> {
    fn evaluate_builtin(&mut self, spec: &WorkSpec) -> RailResult<()> {
        let config_inputs = self
            .index
            .config_deltas()
            .iter()
            .filter(|delta| crate::config::schema::field_consumers(&delta.path).contains(&spec.id.as_str()))
            .map(|delta| format!("config:{}", delta.path))
            .collect::<Vec<_>>();
        let paths = self.index.files();
        let direct = match spec.id.as_str() {
            "cargo.fmt" => paths
                .iter()
                .filter(|path| path.ends_with(".rs") || path.ends_with("rustfmt.toml"))
                .map(|path| format!("path:{path}"))
                .collect::<Vec<_>>(),
            "dependency-policy" => paths
                .iter()
                .filter(|path| {
                    (path.ends_with("Cargo.toml") || path.ends_with("Cargo.lock"))
                        && semantic_path_changed(self.semantic_changes, path)
                        || path.ends_with("deny.toml")
                })
                .map(|path| format!("path:{path}"))
                .collect(),
            "release.semver" => paths
                .iter()
                .filter(|path| is_semver_input(path) && semantic_path_changed(self.semantic_changes, path))
                .map(|path| format!("path:{path}"))
                .chain(config_inputs)
                .collect(),
            "surface" if !self.ctx.config().is_some_and(|config| config.surface.enabled) => Vec::new(),
            "surface" => paths
                .iter()
                .filter(|path| path.ends_with(".rs"))
                .map(|path| format!("path:{path}"))
                .chain(config_inputs)
                .collect(),
            id if BUILTIN_CARGO_WORK.binary_search(&id).is_ok() => {
                cargo_direct_inputs(self.ctx, id, paths, self.semantic_changes)
            }
            _ => Vec::new(),
        };

        if !direct.is_empty() {
            let input = direct.join(",");
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
            let selection = cargo_selection(
                self.ctx,
                self.dependency_universe,
                self.semantic_changes,
                self.observed.base_model(),
                &spec.id,
                paths,
            )?;
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

    fn observed_cargo_decision(&mut self, spec: &WorkSpec, paths: &[String]) -> RailResult<Option<WorkDecision>> {
        if let Some((code, description)) = self.observed.incompatibility() {
            let code = code.to_string();
            let description = description.to_string();
            let evidence = self.add_evidence(&code, &spec.id, None, false, description)?;
            let selection = cargo_selection(
                self.ctx,
                self.dependency_universe,
                self.semantic_changes,
                self.observed.base_model(),
                &spec.id,
                paths,
            )?;
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
            let selection = cargo_selection(
                self.ctx,
                self.dependency_universe,
                self.semantic_changes,
                self.observed.base_model(),
                &spec.id,
                paths,
            )?;
            return Ok(Some(WorkDecision::Required {
                cause: WorkCause::IncompleteEvidence,
                scope: WorkScope::Cargo { selection },
                evidence: vec![evidence],
            }));
        }

        let changed = observed
            .inputs
            .iter()
            .filter(|input| paths.contains(&input.path))
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
        Ok(Some(WorkDecision::Required {
            cause: WorkCause::ChangedInput,
            scope: WorkScope::Cargo { selection },
            evidence: vec![evidence],
        }))
    }

    fn evaluate_declared(&mut self, spec: &WorkSpec) -> RailResult<()> {
        let path_inputs = matching_paths(&spec.paths, self.index.files())?;
        let config_inputs = self
            .index
            .config_deltas()
            .iter()
            .filter(|delta| {
                spec.config.contains(&delta.path)
                    || delta.path == format!("plan.work.{}", spec.id)
                    || delta.path.starts_with(&format!("plan.work.{}.", spec.id))
            })
            .map(|delta| format!("config:{}", delta.path))
            .collect::<Vec<_>>();
        let selected_cargo = spec
            .cargo
            .iter()
            .filter(|id| {
                matches!(
                    self.decisions.get(*id),
                    Some(WorkDecision::Required {
                        cause: WorkCause::ChangedInput,
                        ..
                    })
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        let config_changed = !config_inputs.is_empty();
        let cargo_inputs = selected_cargo
            .iter()
            .map(|id| format!("cargo:{id}"))
            .collect::<Vec<_>>();
        let mut direct = path_inputs
            .iter()
            .map(|path| format!("path:{path}"))
            .collect::<Vec<_>>();
        direct.extend(config_inputs);
        direct.extend(cargo_inputs);

        // Variant rows are a narrower authority than the work-level gate. A
        // changed work input can require the family while the catalog still
        // proves an exact row subset; only an unmatched required input widens
        // to the explicit `all` selection.
        let selected_variants = self.select_variants(spec)?;
        if !direct.is_empty() || selected_variants.as_ref().is_some_and(|variants| !variants.is_empty()) {
            let input = if direct.is_empty() {
                selected_variants
                    .as_ref()
                    .into_iter()
                    .flatten()
                    .map(|variant| format!("variant:{}", variant.id))
                    .collect::<Vec<_>>()
                    .join(",")
            } else {
                direct.join(",")
            };
            let evidence = self.add_evidence(
                "changed_input",
                &spec.id,
                Some(&input),
                true,
                "a declared positive input changed".to_string(),
            )?;
            let scope = if spec.scope == WorkSpecScope::Cargo {
                WorkScope::Cargo {
                    selection: self.declared_cargo_selection(spec, &path_inputs, config_changed, &selected_cargo)?,
                }
            } else {
                self.scope_for(spec, &path_inputs, evidence.clone(), selected_variants)?
            };
            self.decisions.insert(
                spec.id.clone(),
                WorkDecision::Required {
                    cause: WorkCause::ChangedInput,
                    scope,
                    evidence: vec![evidence],
                },
            );
        } else {
            let evidence = self.add_evidence(
                "declared_inputs_disjoint",
                &spec.id,
                Some(&self.index.files().join(",")),
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
        spec: &WorkSpec,
        paths: &[String],
        config_changed: bool,
        selected_cargo: &[String],
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
                    cause: WorkCause::ChangedInput,
                    scope: WorkScope::Cargo { selection },
                    ..
                }) => Some(selection.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        if !paths.is_empty() {
            selections.push(cargo_selection(
                self.ctx,
                self.dependency_universe,
                self.semantic_changes,
                self.observed.base_model(),
                &spec.id,
                paths,
            )?);
        }
        merge_cargo_selections(selections)
    }

    fn select_variants(&self, spec: &WorkSpec) -> RailResult<Option<Vec<SelectedVariant>>> {
        let Some(catalog) = &spec.catalog else {
            return Ok(None);
        };
        let mut selected = Vec::new();
        for row in &catalog.variants {
            let path_match = !matching_paths(&row.paths, self.index.files())?.is_empty();
            let config_match = self
                .index
                .config_deltas()
                .iter()
                .any(|delta| row.config.contains(&delta.path));
            let cargo_match = row.cargo.iter().any(|id| {
                matches!(
                    self.decisions.get(id),
                    Some(WorkDecision::Required {
                        cause: WorkCause::ChangedInput,
                        ..
                    })
                )
            });
            if path_match || config_match || cargo_match {
                selected.push(SelectedVariant {
                    id: row.id.clone(),
                    dimensions: row.dimensions.clone(),
                });
            }
        }
        Ok(Some(selected))
    }

    fn scope_for(
        &self,
        spec: &WorkSpec,
        paths: &[String],
        evidence: String,
        variants: Option<Vec<SelectedVariant>>,
    ) -> RailResult<WorkScope> {
        Ok(match spec.scope {
            WorkSpecScope::Repository => WorkScope::Repository,
            WorkSpecScope::Cargo => WorkScope::Cargo {
                selection: cargo_selection(
                    self.ctx,
                    self.dependency_universe,
                    self.semantic_changes,
                    self.observed.base_model(),
                    &spec.id,
                    paths,
                )?,
            },
            WorkSpecScope::Variants => WorkScope::Variants {
                selection: match variants {
                    Some(variants) if !variants.is_empty() => VariantSelection::Selected { variants, evidence },
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

fn cargo_direct_inputs(
    ctx: &WorkspaceContext,
    id: &str,
    paths: &[String],
    semantic_changes: &BTreeMap<String, SemanticFileChange>,
) -> Vec<String> {
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
        .map(|path| format!("path:{path}"))
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

fn cargo_membership_uncertain(ctx: &WorkspaceContext, paths: &[String]) -> bool {
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

fn matching_paths(patterns: &[String], files: &[String]) -> RailResult<Vec<String>> {
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
        .cloned()
        .collect())
}

fn cargo_selection(
    ctx: &WorkspaceContext,
    dependency_universe: &DependencyUniverse,
    semantic_changes: &BTreeMap<String, SemanticFileChange>,
    base_model: Option<&PortableBaseModel>,
    work: &str,
    paths: &[String],
) -> RailResult<CargoSelection> {
    if paths.iter().any(|path| {
        matches!(
            semantic_changes.get(path).map(|change| &change.scope),
            Some(SemanticScope::Workspace)
        )
    }) {
        if let Some(selection) =
            base_model.and_then(|model| historical_cargo_selection(ctx, model, semantic_changes, work, paths))
        {
            return selection;
        }
        return Ok(CargoSelection::Workspace {
            cargo_args: Vec::new(),
            targets: Vec::new(),
        });
    }
    let mut seeds = HashSet::new();
    for path in paths {
        match semantic_changes.get(path).map(|change| &change.scope) {
            Some(SemanticScope::None) => {}
            Some(SemanticScope::Packages(packages)) => seeds.extend(packages.iter().cloned()),
            Some(SemanticScope::Workspace) => {
                return Ok(CargoSelection::Workspace {
                    cargo_args: Vec::new(),
                    targets: Vec::new(),
                });
            }
            None => {
                if let Some(package) = ctx
                    .graph()
                    .file_to_package(Path::new(path))
                    .filter(|package| package.is_workspace_member)
                {
                    seeds.insert(package.id.clone());
                }
            }
        }
    }
    if seeds.is_empty() {
        return Ok(CargoSelection::Workspace {
            cargo_args: Vec::new(),
            targets: Vec::new(),
        });
    }

    let mut selected = seeds.clone();
    if BUILTIN_CARGO_WORK.binary_search(&work).is_ok() {
        let impact = dependency_universe.propagate(&seeds)?;
        match work {
            "cargo.build" | "cargo.doc" | "cargo.doctest" => selected.extend(impact.build),
            "cargo.clippy" | "cargo.test" => {
                selected.extend(impact.build);
                selected.extend(impact.development);
            }
            "cargo.package" => {}
            _ => {
                for step in impact.steps {
                    if step.domain == ImpactDomain::Build {
                        selected.insert(step.dependent);
                    }
                }
            }
        }
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

fn historical_cargo_selection(
    ctx: &WorkspaceContext,
    model: &PortableBaseModel,
    semantic_changes: &BTreeMap<String, SemanticFileChange>,
    work: &str,
    paths: &[String],
) -> Option<RailResult<CargoSelection>> {
    let current = ctx
        .cargo()
        .metadata()
        .workspace_packages()
        .iter()
        .map(|package| (portable_package_key(ctx, package), *package))
        .collect::<BTreeMap<_, _>>();
    let removed = model
        .packages
        .iter()
        .filter(|package| !current.contains_key(&package.key))
        .collect::<Vec<_>>();
    if removed.is_empty() {
        return None;
    }
    let root_manifest_changed = paths.iter().any(|path| path == "Cargo.toml");
    let removed_seeds = removed
        .into_iter()
        .filter(|package| {
            root_manifest_changed
                || (!package.root.is_empty()
                    && paths.iter().any(|path| {
                        path == &format!("{}/Cargo.toml", package.root)
                            || path
                                .strip_prefix(&package.root)
                                .is_some_and(|suffix| suffix.starts_with('/'))
                    }))
        })
        .map(|package| package.key.clone())
        .collect::<BTreeSet<_>>();
    if removed_seeds.is_empty() {
        return None;
    }

    let mut selected = removed_seeds;
    for change in semantic_changes.values() {
        if let SemanticScope::Packages(packages) = &change.scope {
            selected.extend(packages.iter().filter_map(|id| {
                ctx.cargo()
                    .metadata()
                    .packages
                    .iter()
                    .find(|package| &package.id == id)
                    .map(|package| portable_package_key(ctx, package))
            }));
        }
    }
    let include_development = matches!(work, "cargo.clippy" | "cargo.test");
    if work != "cargo.package" {
        loop {
            let dependents = model
                .edges
                .iter()
                .filter(|edge| {
                    selected.contains(&edge.dependency)
                        && (edge.domain == PortableBaseEdgeDomain::Build || include_development)
                })
                .map(|edge| edge.dependent.clone())
                .collect::<Vec<_>>();
            let before = selected.len();
            selected.extend(dependents);
            if selected.len() == before {
                break;
            }
        }
    }
    let mut packages = selected
        .iter()
        .filter_map(|key| current.get(key).copied())
        .collect::<Vec<_>>();
    if packages.is_empty() {
        return None;
    }
    Some(selection_for_packages(ctx, &mut packages, paths))
}

fn selection_for_packages(
    ctx: &WorkspaceContext,
    packages: &mut Vec<&Package>,
    paths: &[String],
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
        .and_then(|root| root.as_std_path().strip_prefix(ctx.workspace_root()).ok())
        .map(crate::utils::path_to_git_format)
        .unwrap_or_else(|| "<outside-workspace>".to_string());
    format!("{}@{}#path:{relative}", package.name, package.version)
}

fn target_selectors(ctx: &WorkspaceContext, packages: &[&Package], paths: &[String]) -> Vec<CargoTargetSelector> {
    let path_set = paths.iter().map(String::as_str).collect::<BTreeSet<_>>();
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
    }
    identity(
        "plan-v8",
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
            "v8 planner did not decide every registered work item",
        ));
    }
    for spec in specs {
        let decision = decisions
            .get(&spec.id)
            .ok_or_else(|| RailError::message(format!("v8 planner omitted work '{}'", spec.id)))?;
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
