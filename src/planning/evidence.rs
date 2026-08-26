//! Portable observed-input evidence consumed by named Cargo work.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read as _;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::graph::DependencyUniverse;
use crate::workspace::WorkspaceContext;

pub(super) struct EvidenceBindings<'a> {
    pub(super) source_base: &'a str,
    pub(super) cargo_configuration_identity: &'a str,
    pub(super) toolchain_identity: &'a str,
    pub(super) target_identity: &'a str,
}

const EVIDENCE_VERSION: u32 = 1;
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    #[serde(default)]
    pub(super) bypasses: Vec<String>,
    #[serde(default)]
    pub(super) inputs: Vec<ObservedInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ObservedInput {
    pub(super) path: String,
    pub(super) identity: String,
    pub(super) package: Option<String>,
    pub(super) target: Option<String>,
}

#[derive(Debug)]
pub(super) enum PlanningEvidenceState {
    Absent,
    Compatible(Box<PlanningEvidenceManifest>),
    Incompatible { code: String, description: String },
}

impl PlanningEvidenceState {
    pub(super) fn load(
        path: Option<&Path>,
        bindings: EvidenceBindings<'_>,
        dependency_universe: &DependencyUniverse,
        ctx: &WorkspaceContext,
    ) -> Self {
        let Some(path) = path else {
            return Self::Absent;
        };
        let bytes = match read_bounded(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                return Self::Incompatible {
                    code: "planning_evidence_unreadable".to_string(),
                    description: format!("cannot read planning evidence '{}': {error}", path.display()),
                };
            }
        };
        let manifest = match serde_json::from_slice::<PlanningEvidenceManifest>(&bytes) {
            Ok(manifest) => manifest,
            Err(error) => {
                return Self::Incompatible {
                    code: "planning_evidence_malformed".to_string(),
                    description: format!("cannot parse planning evidence '{}': {error}", path.display()),
                };
            }
        };
        match validate(manifest, bindings, dependency_universe, ctx) {
            Ok(manifest) => Self::Compatible(Box::new(manifest)),
            Err((code, description)) => Self::Incompatible { code, description },
        }
    }

    pub(super) fn identity(&self) -> Option<&str> {
        match self {
            Self::Compatible(manifest) => Some(&manifest.identity),
            Self::Absent | Self::Incompatible { .. } => None,
        }
    }

    pub(super) fn work(&self, id: &str) -> Option<&ObservedWorkEvidence> {
        match self {
            Self::Compatible(manifest) => manifest.work.get(id),
            Self::Absent | Self::Incompatible { .. } => None,
        }
    }

    pub(super) fn capabilities(&self) -> Option<&BTreeSet<String>> {
        match self {
            Self::Compatible(manifest) => Some(&manifest.provider.capabilities),
            Self::Absent | Self::Incompatible { .. } => None,
        }
    }

    pub(super) fn base_model(&self) -> Option<&PortableBaseModel> {
        match self {
            Self::Compatible(manifest) => Some(&manifest.base_model),
            Self::Absent | Self::Incompatible { .. } => None,
        }
    }

    pub(super) fn incompatibility(&self) -> Option<(&str, &str)> {
        match self {
            Self::Incompatible { code, description } => Some((code, description)),
            Self::Absent | Self::Compatible(_) => None,
        }
    }
}

fn validate(
    mut manifest: PlanningEvidenceManifest,
    bindings: EvidenceBindings<'_>,
    dependency_universe: &DependencyUniverse,
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
    if manifest.work.values().map(|work| work.inputs.len()).sum::<usize>() > EVIDENCE_MAX_INPUTS {
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
    if manifest.cargo_identity != dependency_universe.identity() {
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
    let current_platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
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
    revalidate_base_model(ctx, &manifest.source_base, &manifest.base_model)?;
    revalidate_observed_inputs(ctx, &manifest.source_base, &manifest.work)?;
    Ok(manifest)
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
        if evidence.inputs.windows(2).any(|pair| pair[0] == pair[1]) {
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

fn secret_capability_name(name: &str) -> bool {
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
    format!("planning-evidence-v1:sha256:{hex}")
}

#[cfg(test)]
mod tests {
    use super::secret_capability_name;

    #[test]
    fn secret_capability_environment_names_are_rejected() {
        assert!(secret_capability_name("AWS_SECRET_ACCESS_KEY"));
        assert!(secret_capability_name("github_token"));
        assert!(!secret_capability_name("RUSTFLAGS"));
    }
}
