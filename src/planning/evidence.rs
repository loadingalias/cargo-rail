//! Portable observed-input evidence consumed by named Cargo work.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use rscrypto::Sha256;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::workspace::WorkspaceContext;

pub(super) struct EvidenceBindings<'a> {
    pub(super) source_base: &'a str,
    pub(super) cargo_configuration_identity: &'a str,
    pub(super) toolchain_identity: &'a str,
    pub(super) target_identity: &'a str,
}

pub(super) const EVIDENCE_VERSION: u32 = 2;
const EVIDENCE_MAX_BYTES: u64 = 16 * 1024 * 1024;
const EVIDENCE_MAX_WORK: usize = 256;
const EVIDENCE_MAX_INPUTS: usize = 100_000;
const EVIDENCE_MAX_PACKAGES: usize = 10_000;
const EVIDENCE_MAX_TARGETS: usize = 100_000;
const EVIDENCE_MAX_EDGES: usize = 100_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EvidenceProvider {
    pub(super) identity: String,
    pub(super) capabilities: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PlanningEvidenceManifest {
    pub(super) planning_evidence_version: u32,
    pub(super) identity: String,
    pub(super) provider: EvidenceProvider,
    pub(super) source_base: String,
    pub(super) cargo_identity: String,
    pub(super) cargo_configuration_identity: String,
    pub(super) toolchain_identity: String,
    pub(super) target_identity: String,
    pub(super) platform: String,
    #[serde(default)]
    pub(super) environment: Vec<String>,
    pub(super) base_model: PortableBaseModel,
    pub(super) work: BTreeMap<String, ObservedWorkEvidence>,
}

/// Source-bound structural Cargo facts retained for historical scope.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PortableBaseModel {
    pub(super) packages: Vec<PortableBasePackage>,
    pub(super) edges: Vec<PortableBaseEdge>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PortableBasePackage {
    pub(super) key: String,
    pub(super) name: String,
    pub(super) root: String,
    pub(super) targets: Vec<PortableBaseTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PortableBaseTarget {
    pub(super) name: String,
    pub(super) kind: Vec<String>,
    pub(super) src_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PortableBaseEdge {
    pub(super) dependency: String,
    pub(super) dependent: String,
    pub(super) domain: PortableBaseEdgeDomain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum PortableBaseEdgeDomain {
    Build,
    Development,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ObservedWorkEvidence {
    pub(super) complete: bool,
    pub(super) bypasses: Vec<String>,
    pub(super) inputs: Vec<ObservedInput>,
    /// Directories whose every current and future entry is an input, such as the package
    /// sources of a build script that declares no `rerun-if-changed` path.
    pub(super) directories: Vec<ObservedDirectory>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ObservedInput {
    pub(super) path: String,
    pub(super) identity: String,
    pub(super) package: Option<String>,
    pub(super) target: Option<String>,
}

/// A directory input. `.` names the whole workspace.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ObservedDirectory {
    pub(super) path: String,
    pub(super) package: String,
}

impl ObservedDirectory {
    /// Whether a changed workspace-relative path is this directory or lies below it.
    pub(super) fn covers(&self, path: &str) -> bool {
        self.path == "."
            || path == self.path
            || path
                .strip_prefix(self.path.as_str())
                .is_some_and(|suffix| suffix.starts_with('/'))
    }
}

/// Every supplied manifest, validated independently.
///
/// A compatible manifest contributes its work items. An incompatible manifest, or two
/// manifests that both describe one work item, leave the affected work without evidence,
/// so that work widens while other work keeps its compatible evidence.
#[derive(Debug, Default)]
pub(super) struct PlanningEvidenceState {
    manifests: Vec<PlanningEvidenceManifest>,
    work: BTreeMap<String, usize>,
    duplicates: BTreeSet<String>,
    incompatibility: Option<(String, String)>,
}

impl PlanningEvidenceState {
    pub(super) fn load(
        paths: &[PathBuf],
        bindings: EvidenceBindings<'_>,
        cargo_identity: &str,
        ctx: &WorkspaceContext,
    ) -> Self {
        let mut state = Self::default();
        for path in paths {
            match load_compatible(path, &bindings, cargo_identity, ctx) {
                Ok(manifest) => state.admit(manifest),
                Err((code, description)) => {
                    state.incompatibility.get_or_insert((code, description));
                }
            }
        }
        state
    }

    fn admit(&mut self, manifest: PlanningEvidenceManifest) {
        if self
            .manifests
            .first()
            .is_some_and(|first| first.base_model != manifest.base_model)
        {
            self.incompatibility.get_or_insert_with(|| {
                (
                    "planning_evidence_base_model_conflict".to_string(),
                    "planning evidence manifests describe different base Cargo models".to_string(),
                )
            });
            return;
        }
        if self
            .manifests
            .iter()
            .any(|existing| existing.identity == manifest.identity)
        {
            return;
        }
        let index = self.manifests.len();
        for work in manifest.work.keys() {
            if self.work.insert(work.clone(), index).is_some() {
                self.duplicates.insert(work.clone());
            }
        }
        self.manifests.push(manifest);
    }

    /// Identities of every compatible manifest, in canonical order.
    pub(super) fn identities(&self) -> Vec<String> {
        let mut identities = self
            .manifests
            .iter()
            .map(|manifest| manifest.identity.clone())
            .collect::<Vec<_>>();
        identities.sort();
        identities
    }

    pub(super) fn work(&self, id: &str) -> Option<&ObservedWorkEvidence> {
        if self.duplicates.contains(id) {
            return None;
        }
        self.work.get(id).and_then(|index| self.manifests[*index].work.get(id))
    }

    /// Capabilities of the manifest that describes `id`.
    pub(super) fn capabilities(&self, id: &str) -> Option<&BTreeSet<String>> {
        self.work(id)?;
        self.work
            .get(id)
            .map(|index| &self.manifests[*index].provider.capabilities)
    }

    pub(super) fn base_model(&self) -> Option<&PortableBaseModel> {
        self.manifests.first().map(|manifest| &manifest.base_model)
    }

    /// Why no compatible evidence describes `id`, when a supplied manifest could have.
    pub(super) fn unusable(&self, id: &str) -> Option<(&str, &str)> {
        if self.duplicates.contains(id) {
            return Some((
                "planning_evidence_work_duplicate",
                "more than one planning evidence manifest describes this work",
            ));
        }
        if self.work.contains_key(id) {
            return None;
        }
        self.incompatibility
            .as_ref()
            .map(|(code, description)| (code.as_str(), description.as_str()))
    }
}

fn load_compatible(
    path: &Path,
    bindings: &EvidenceBindings<'_>,
    cargo_identity: &str,
    ctx: &WorkspaceContext,
) -> Result<PlanningEvidenceManifest, (String, String)> {
    let bytes = read_bounded(path).map_err(|error| {
        incompatible(
            "planning_evidence_unreadable",
            format!("cannot read planning evidence '{}': {error}", path.display()),
        )
    })?;
    let manifest = serde_json::from_slice::<PlanningEvidenceManifest>(&bytes).map_err(|error| {
        incompatible(
            "planning_evidence_malformed",
            format!("cannot parse planning evidence '{}': {error}", path.display()),
        )
    })?;
    validate(manifest, bindings, cargo_identity, ctx)
}

fn validate(
    mut manifest: PlanningEvidenceManifest,
    bindings: &EvidenceBindings<'_>,
    cargo_identity: &str,
    ctx: &WorkspaceContext,
) -> Result<PlanningEvidenceManifest, (String, String)> {
    if manifest.planning_evidence_version != EVIDENCE_VERSION {
        return Err(incompatible(
            "planning_evidence_contract_unknown",
            format!(
                "planning evidence contract {} is unsupported",
                manifest.planning_evidence_version
            ),
        ));
    }
    if manifest.provider.identity.is_empty() {
        return Err(incompatible(
            "planning_evidence_provider_invalid",
            "planning evidence provider identity is empty".to_string(),
        ));
    }
    if manifest.work.len() > EVIDENCE_MAX_WORK {
        return Err(incompatible(
            "planning_evidence_size_invalid",
            format!("planning evidence names more than {EVIDENCE_MAX_WORK} work items"),
        ));
    }
    if manifest
        .work
        .values()
        .map(|work| work.inputs.len() + work.directories.len())
        .sum::<usize>()
        > EVIDENCE_MAX_INPUTS
    {
        return Err(incompatible(
            "planning_evidence_size_invalid",
            format!("planning evidence names more than {EVIDENCE_MAX_INPUTS} observed inputs"),
        ));
    }
    if manifest.base_model.packages.len() > EVIDENCE_MAX_PACKAGES
        || manifest
            .base_model
            .packages
            .iter()
            .map(|package| package.targets.len())
            .sum::<usize>()
            > EVIDENCE_MAX_TARGETS
        || manifest.base_model.edges.len() > EVIDENCE_MAX_EDGES
    {
        return Err(incompatible(
            "planning_evidence_size_invalid",
            "planning evidence base model exceeds its package, target, or edge bound".to_string(),
        ));
    }
    normalize_manifest(&mut manifest)?;
    let claimed = std::mem::take(&mut manifest.identity);
    let encoded = canonical_bytes(&manifest)
        .map_err(|description| incompatible("planning_evidence_identity_invalid", description))?;
    let actual = digest_identity(&encoded);
    manifest.identity = claimed.clone();
    if claimed != actual {
        return Err(incompatible(
            "planning_evidence_identity_invalid",
            format!("planning evidence identity '{claimed}' does not match '{actual}'"),
        ));
    }
    if manifest.source_base != bindings.source_base {
        return Err(incompatible(
            "planning_evidence_source_mismatch",
            "planning evidence is bound to a different base source identity".to_string(),
        ));
    }
    if manifest.cargo_identity != cargo_identity {
        return Err(incompatible(
            "planning_evidence_cargo_mismatch",
            "planning evidence is bound to a different Cargo resolution universe".to_string(),
        ));
    }
    if manifest.cargo_configuration_identity != bindings.cargo_configuration_identity {
        return Err(incompatible(
            "planning_evidence_cargo_configuration_mismatch",
            "planning evidence is bound to different Cargo configuration".to_string(),
        ));
    }
    if manifest.toolchain_identity != bindings.toolchain_identity {
        return Err(incompatible(
            "planning_evidence_toolchain_mismatch",
            "planning evidence is bound to a different toolchain".to_string(),
        ));
    }
    if manifest.target_identity != bindings.target_identity {
        return Err(incompatible(
            "planning_evidence_target_mismatch",
            "planning evidence is bound to a different target identity".to_string(),
        ));
    }
    let current_platform = super::work::host_platform();
    if manifest.platform != current_platform {
        return Err(incompatible(
            "planning_evidence_platform_mismatch",
            format!(
                "planning evidence platform '{}' does not match '{current_platform}'",
                manifest.platform
            ),
        ));
    }
    if manifest.environment.iter().any(|name| secret_capability_name(name)) {
        return Err(incompatible(
            "planning_evidence_secret_environment",
            "planning evidence contains a secret-capability environment name".to_string(),
        ));
    }
    for (work, evidence) in &manifest.work {
        if !work.starts_with("cargo.") {
            return Err(incompatible(
                "planning_evidence_work_unknown",
                format!("planning evidence names non-Cargo work '{work}'"),
            ));
        }
        for input in &evidence.inputs {
            if input.identity.is_empty()
                || crate::config::plan::validate_positive_path(&input.path, "planning evidence input", false).is_err()
            {
                return Err(incompatible(
                    "planning_evidence_input_invalid",
                    format!("planning evidence for '{work}' contains invalid input '{}'", input.path),
                ));
            }
            match (&input.package, &input.target) {
                (None, None) => {}
                (None, Some(_)) => {
                    return Err(incompatible(
                        "planning_evidence_input_invalid",
                        format!("planning evidence for '{work}' names a target without a package"),
                    ));
                }
                (Some(package), target) => {
                    let Some(base_package) = manifest
                        .base_model
                        .packages
                        .iter()
                        .find(|candidate| &candidate.key == package)
                    else {
                        return Err(incompatible(
                            "planning_evidence_input_invalid",
                            format!("planning evidence for '{work}' names unknown base package '{package}'"),
                        ));
                    };
                    if let Some(target) = target
                        && !base_package.targets.iter().any(|candidate| &candidate.name == target)
                    {
                        return Err(incompatible(
                            "planning_evidence_input_invalid",
                            format!("planning evidence for '{work}' names unknown target '{target}'"),
                        ));
                    }
                }
            }
        }
    }
    for (work, evidence) in &manifest.work {
        for directory in &evidence.directories {
            if directory.path != "."
                && crate::config::plan::validate_positive_path(&directory.path, "planning evidence directory", false)
                    .is_err()
            {
                return Err(incompatible(
                    "planning_evidence_input_invalid",
                    format!(
                        "planning evidence for '{work}' contains invalid directory '{}'",
                        directory.path
                    ),
                ));
            }
            if !manifest
                .base_model
                .packages
                .iter()
                .any(|candidate| candidate.key == directory.package)
            {
                return Err(incompatible(
                    "planning_evidence_input_invalid",
                    format!(
                        "planning evidence for '{work}' names unknown base package '{}'",
                        directory.package
                    ),
                ));
            }
        }
    }
    revalidate_base_model(ctx, &manifest.source_base, &manifest.base_model)?;
    revalidate_observed_inputs(ctx, &manifest.source_base, &manifest.work)?;
    revalidate_observed_directories(ctx, &manifest.source_base, &manifest.work)?;
    Ok(manifest)
}

/// Normalize a producer's manifest and attach its canonical identity.
pub(super) fn sign_manifest(
    mut manifest: PlanningEvidenceManifest,
) -> Result<PlanningEvidenceManifest, (String, String)> {
    normalize_manifest(&mut manifest)?;
    manifest.identity = String::new();
    let encoded = canonical_bytes(&manifest)
        .map_err(|description| incompatible("planning_evidence_identity_invalid", description))?;
    manifest.identity = digest_identity(&encoded);
    Ok(manifest)
}

/// Read one manifest and verify its contract and identity, without checkout bindings.
pub(super) fn load_signed_manifest(path: &Path) -> Result<PlanningEvidenceManifest, (String, String)> {
    let bytes = read_bounded(path).map_err(|error| {
        incompatible(
            "planning_evidence_unreadable",
            format!("cannot read planning evidence '{}': {error}", path.display()),
        )
    })?;
    let manifest = serde_json::from_slice::<PlanningEvidenceManifest>(&bytes).map_err(|error| {
        incompatible(
            "planning_evidence_malformed",
            format!("cannot parse planning evidence '{}': {error}", path.display()),
        )
    })?;
    if manifest.planning_evidence_version != EVIDENCE_VERSION {
        return Err(incompatible(
            "planning_evidence_contract_unknown",
            format!(
                "planning evidence contract {} is unsupported",
                manifest.planning_evidence_version
            ),
        ));
    }
    let claimed = manifest.identity.clone();
    let signed = sign_manifest(manifest)?;
    if signed.identity != claimed {
        return Err(incompatible(
            "planning_evidence_identity_invalid",
            format!(
                "planning evidence identity '{claimed}' does not match '{}'",
                signed.identity
            ),
        ));
    }
    Ok(signed)
}

fn read_bounded(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.by_ref().take(EVIDENCE_MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > EVIDENCE_MAX_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "planning evidence exceeds the {} MiB bound",
                EVIDENCE_MAX_BYTES / 1024 / 1024
            ),
        ));
    }
    Ok(bytes)
}

fn normalize_manifest(manifest: &mut PlanningEvidenceManifest) -> Result<(), (String, String)> {
    sort_unique(&mut manifest.environment, "environment")?;
    normalize_base_model(&mut manifest.base_model)?;
    for (work, evidence) in &mut manifest.work {
        sort_unique(&mut evidence.bypasses, &format!("{work} bypasses"))?;
        evidence.inputs.sort();
        evidence.directories.sort();
        if evidence.inputs.windows(2).any(|pair| pair[0] == pair[1])
            || evidence.directories.windows(2).any(|pair| pair[0] == pair[1])
        {
            return Err(incompatible(
                "planning_evidence_input_invalid",
                format!("planning evidence for '{work}' contains a duplicate input"),
            ));
        }
    }
    Ok(())
}

fn normalize_base_model(model: &mut PortableBaseModel) -> Result<(), (String, String)> {
    for package in &mut model.packages {
        let expected_suffix = format!("#path:{}", package.root);
        if package.key.is_empty()
            || package.name.is_empty()
            || !package.key.starts_with(&format!("{}@", package.name))
            || !package.key.ends_with(&expected_suffix)
        {
            return Err(incompatible(
                "planning_evidence_base_model_invalid",
                "planning evidence base package key does not match its name and repository root".to_string(),
            ));
        }
        if !package.root.is_empty()
            && crate::config::plan::validate_positive_path(&package.root, "planning evidence base package root", false)
                .is_err()
        {
            return Err(incompatible(
                "planning_evidence_base_model_invalid",
                format!("planning evidence base package '{}' has an invalid root", package.key),
            ));
        }
        for target in &mut package.targets {
            if target.name.is_empty()
                || crate::config::plan::validate_positive_path(
                    &target.src_path,
                    "planning evidence base target source",
                    false,
                )
                .is_err()
            {
                return Err(incompatible(
                    "planning_evidence_base_model_invalid",
                    format!("planning evidence base package '{}' has an invalid target", package.key),
                ));
            }
            if !package.root.is_empty()
                && !target
                    .src_path
                    .strip_prefix(&package.root)
                    .is_some_and(|suffix| suffix.starts_with('/'))
            {
                return Err(incompatible(
                    "planning_evidence_base_model_invalid",
                    format!(
                        "planning evidence base target '{}' is outside package '{}'",
                        target.src_path, package.key
                    ),
                ));
            }
            sort_unique(&mut target.kind, "base target kinds")?;
            if target.kind.is_empty() {
                return Err(incompatible(
                    "planning_evidence_base_model_invalid",
                    format!("planning evidence base target '{}' has no kind", target.name),
                ));
            }
        }
        package.targets.sort();
        if package.targets.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(incompatible(
                "planning_evidence_base_model_invalid",
                format!("planning evidence base package '{}' has duplicate targets", package.key),
            ));
        }
    }
    model.packages.sort();
    if model.packages.windows(2).any(|pair| pair[0].key == pair[1].key) {
        return Err(incompatible(
            "planning_evidence_base_model_invalid",
            "planning evidence base model has duplicate package keys".to_string(),
        ));
    }
    let packages = model
        .packages
        .iter()
        .map(|package| package.key.as_str())
        .collect::<BTreeSet<_>>();
    for edge in &model.edges {
        if !packages.contains(edge.dependency.as_str()) || !packages.contains(edge.dependent.as_str()) {
            return Err(incompatible(
                "planning_evidence_base_model_invalid",
                "planning evidence base edge references an unknown package".to_string(),
            ));
        }
    }
    model.edges.sort();
    if model.edges.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(incompatible(
            "planning_evidence_base_model_invalid",
            "planning evidence base model has duplicate edges".to_string(),
        ));
    }
    Ok(())
}

fn revalidate_base_model(
    ctx: &WorkspaceContext,
    source_base: &str,
    model: &PortableBaseModel,
) -> Result<(), (String, String)> {
    if model.packages.is_empty() {
        return Ok(());
    }
    let workspace_paths = model
        .packages
        .iter()
        .flat_map(|package| {
            let manifest = if package.root.is_empty() {
                "Cargo.toml".to_string()
            } else {
                format!("{}/Cargo.toml", package.root)
            };
            std::iter::once(manifest).chain(package.targets.iter().map(|target| target.src_path.clone()))
        })
        .collect::<BTreeSet<_>>();
    let repository_paths = workspace_paths
        .iter()
        .map(|path| {
            ctx.workspace_prefix()
                .map_or_else(|| path.into(), |prefix| prefix.join(path))
        })
        .collect::<Vec<_>>();
    let entries = ctx
        .git()
        .and_then(|git| git.git().collect_tree_entries_for_paths(source_base, &repository_paths))
        .map_err(|error| {
            incompatible(
                "planning_evidence_base_model_unverifiable",
                format!("cannot verify the portable base model against '{source_base}': {error}"),
            )
        })?;
    let present = entries.into_iter().map(|entry| entry.path).collect::<BTreeSet<_>>();
    if let Some(path) = repository_paths.iter().find(|path| !present.contains(*path)) {
        return Err(incompatible(
            "planning_evidence_base_model_invalid",
            format!(
                "planning evidence base-model path '{}' is absent from '{source_base}'",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn sort_unique(values: &mut [String], subject: &str) -> Result<(), (String, String)> {
    values.sort_unstable();
    if values.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(incompatible(
            "planning_evidence_input_invalid",
            format!("planning evidence contains duplicate {subject}"),
        ));
    }
    Ok(())
}

fn revalidate_observed_inputs(
    ctx: &WorkspaceContext,
    source_base: &str,
    work: &BTreeMap<String, ObservedWorkEvidence>,
) -> Result<(), (String, String)> {
    let mut inputs = BTreeMap::<String, String>::new();
    for evidence in work.values() {
        for input in &evidence.inputs {
            if let Some(existing) = inputs.insert(input.path.clone(), input.identity.clone())
                && existing != input.identity
            {
                return Err(incompatible(
                    "planning_evidence_input_identity_invalid",
                    format!("observed input '{}' has conflicting base identities", input.path),
                ));
            }
        }
    }
    if inputs.is_empty() {
        return Ok(());
    }

    let repository_paths = inputs
        .keys()
        .map(|path| {
            ctx.workspace_prefix()
                .map_or_else(|| path.into(), |prefix| prefix.join(path))
        })
        .collect::<Vec<_>>();
    let entries = ctx
        .git()
        .and_then(|git| git.git().collect_tree_entries_for_paths(source_base, &repository_paths))
        .map_err(|error| {
            incompatible(
                "planning_evidence_input_unverifiable",
                format!("cannot verify observed inputs against the base source: {error}"),
            )
        })?;
    let entries = entries
        .into_iter()
        .map(|entry| (entry.path, (entry.mode, entry.object_id)))
        .collect::<BTreeMap<_, _>>();

    let mut sha_paths = Vec::new();
    for (path, identity) in &inputs {
        let repository_path = ctx
            .workspace_prefix()
            .map_or_else(|| path.into(), |prefix| prefix.join(path));
        let Some((mode, object_id)) = entries.get(&repository_path) else {
            return Err(incompatible(
                "planning_evidence_input_identity_invalid",
                format!("observed input '{path}' is absent from base source '{source_base}'"),
            ));
        };
        let git_identity = format!("git:{mode}:{object_id}");
        if identity == &git_identity {
            continue;
        }
        if valid_sha256_identity(identity) {
            sha_paths.push((path.as_str(), identity.as_str()));
            continue;
        }
        return Err(incompatible(
            "planning_evidence_input_identity_invalid",
            format!("observed input '{path}' identity does not match its base Git object"),
        ));
    }

    if !sha_paths.is_empty() {
        let absolute_paths = sha_paths
            .iter()
            .map(|(path, _)| ctx.workspace_root().join(path))
            .collect::<Vec<_>>();
        let items = absolute_paths
            .iter()
            .map(|path| (source_base, path.as_path()))
            .collect::<Vec<_>>();
        let bytes = ctx
            .git()
            .and_then(|git| git.git().read_files_bulk(&items))
            .map_err(|error| {
                incompatible(
                    "planning_evidence_input_unverifiable",
                    format!("cannot hash observed base inputs: {error}"),
                )
            })?;
        for ((path, identity), bytes) in sha_paths.into_iter().zip(bytes) {
            let actual = format!("sha256:{}", crate::source::ContentDigest::sha256(&bytes));
            if identity != actual {
                return Err(incompatible(
                    "planning_evidence_input_identity_invalid",
                    format!("observed input '{path}' digest does not match the base source"),
                ));
            }
        }
    }
    Ok(())
}

/// Require every named directory to hold at least one tracked entry at the base source.
fn revalidate_observed_directories(
    ctx: &WorkspaceContext,
    source_base: &str,
    work: &BTreeMap<String, ObservedWorkEvidence>,
) -> Result<(), (String, String)> {
    let directories = work
        .values()
        .flat_map(|evidence| &evidence.directories)
        .filter(|directory| directory.path != ".")
        .map(|directory| directory.path.as_str())
        .collect::<BTreeSet<_>>();
    if directories.is_empty() {
        return Ok(());
    }
    let repository_path = |path: &str| {
        ctx.workspace_prefix()
            .map_or_else(|| std::path::PathBuf::from(path), |prefix| prefix.join(path))
    };
    let entries = ctx
        .git()
        .and_then(|git| {
            git.git().collect_tree_entries_for_paths(
                source_base,
                &directories.iter().map(|path| repository_path(path)).collect::<Vec<_>>(),
            )
        })
        .map_err(|error| {
            incompatible(
                "planning_evidence_input_unverifiable",
                format!("cannot verify observed directories against the base source: {error}"),
            )
        })?;
    for directory in directories {
        let root = repository_path(directory);
        if !entries.iter().any(|entry| entry.path.starts_with(&root)) {
            return Err(incompatible(
                "planning_evidence_input_identity_invalid",
                format!("observed directory '{directory}' has no entries in base source '{source_base}'"),
            ));
        }
    }
    Ok(())
}

fn valid_sha256_identity(identity: &str) -> bool {
    identity
        .strip_prefix("sha256:")
        .is_some_and(|digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

pub(super) fn required_capabilities(work: &str) -> &'static [&'static str] {
    match work {
        "cargo.doc" | "cargo.doctest" => &[
            "build_script_reads",
            "compiler_reads",
            "process_domain",
            "proc_macro_reads",
            "rustdoc_dep_info",
        ],
        _ => &[
            "build_script_reads",
            "compiler_reads",
            "process_domain",
            "proc_macro_reads",
            "rustc_dep_info",
        ],
    }
}

pub(super) fn secret_capability_name(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase();
    ["credential", "password", "secret", "token"]
        .iter()
        .any(|marker| normalized.contains(marker))
        || normalized.ends_with("_key")
}

fn incompatible(code: &str, description: String) -> (String, String) {
    (code.to_string(), description)
}

fn canonical_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    let value = serde_json::to_value(value).map_err(|error| error.to_string())?;
    serde_json::to_vec(&canonicalize(value)).map_err(|error| error.to_string())
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

fn digest_identity(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        hex.push(char::from(HEX[usize::from(byte >> 4)]));
        hex.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    format!("planning-evidence-v{EVIDENCE_VERSION}:sha256:{hex}")
}

#[cfg(test)]
mod tests {
    use super::{ObservedDirectory, secret_capability_name};

    #[test]
    fn directory_inputs_cover_their_entries_and_the_workspace_root_covers_everything() {
        let directory = ObservedDirectory {
            path: "crates/gen/schema".to_string(),
            package: "gen@0.1.0#path:crates/gen".to_string(),
        };
        assert!(directory.covers("crates/gen/schema/new.json"));
        assert!(directory.covers("crates/gen/schema"));
        assert!(!directory.covers("crates/gen/schema-old/new.json"));
        assert!(!directory.covers("crates/gen/build.rs"));
        let root = ObservedDirectory {
            path: ".".to_string(),
            package: "root@0.1.0#path:".to_string(),
        };
        assert!(root.covers("README.md"));
    }

    #[test]
    fn secret_capability_environment_names_are_rejected() {
        assert!(secret_capability_name("AWS_SECRET_ACCESS_KEY"));
        assert!(secret_capability_name("github_token"));
        assert!(!secret_capability_name("RUSTFLAGS"));
    }
}
