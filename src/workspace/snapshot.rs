//! Immutable authoritative inputs for one Cargo workspace capture.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use cargo_metadata::{Metadata, PackageId, Source};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::cargo::MultiTargetMetadata;
use crate::cargo::resolution::{
  CargoConfigSnapshot, ResolutionInputs, ResolutionRequest, ResolutionView, ResolutionViews, TargetIdentity,
  ToolchainIdentity, credential_bearing_url,
};
use crate::compiler::cfg_eval::{TargetCfgSet, load_target_cfg_sets};
use crate::config::RailConfig;
use crate::error::{RailError, RailResult};
use crate::source::{ContentDigest, RepositoryPath, SourceEntryKind, SourceSnapshot};
use crate::workspace::context::CargoState;

const SNAPSHOT_ID_VERSION: u32 = 1;

/// Versioned SHA-256 identity of canonical authoritative workspace inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SnapshotId {
  version: u32,
  digest: ContentDigest,
}

impl SnapshotId {
  /// Return the framing schema version used for this identity.
  pub fn version(self) -> u32 {
    self.version
  }

  /// Return the raw SHA-256 digest.
  pub fn digest(self) -> ContentDigest {
    self.digest
  }
}

impl fmt::Display for SnapshotId {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(formatter, "v{}-sha256-{}", self.version, self.digest)
  }
}

/// Exact bytes of one authoritative workspace file.
pub struct SnapshotFile {
  path: RepositoryPath,
  bytes: Arc<[u8]>,
  digest: ContentDigest,
}

impl SnapshotFile {
  /// Return the logical source-root-relative path.
  pub fn path(&self) -> &RepositoryPath {
    &self.path
  }

  /// Return the exact captured bytes.
  pub fn bytes(&self) -> &[u8] {
    &self.bytes
  }

  /// Return the SHA-256 identity of the exact bytes.
  pub fn digest(&self) -> ContentDigest {
    self.digest
  }
}

/// One package identity parsed from the exact Cargo lockfile.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LockedPackageIdentity {
  name: String,
  version: String,
  source: Option<String>,
  checksum: Option<String>,
}

impl LockedPackageIdentity {
  /// Return the locked package name.
  pub fn name(&self) -> &str {
    &self.name
  }

  /// Return the locked semantic version as Cargo wrote it.
  pub fn version(&self) -> &str {
    &self.version
  }

  /// Return Cargo's locked source identity, when present.
  pub fn source(&self) -> Option<&str> {
    self.source.as_deref()
  }

  /// Return the registry checksum, when Cargo recorded one.
  pub fn checksum(&self) -> Option<&str> {
    self.checksum.as_deref()
  }
}

/// Exact Cargo lockfile bytes plus parsed package/source identities.
pub struct LockfileSnapshot {
  file: SnapshotFile,
  packages: Vec<LockedPackageIdentity>,
}

impl LockfileSnapshot {
  /// Return the exact lockfile capture.
  pub fn file(&self) -> &SnapshotFile {
    &self.file
  }

  /// Return locked package identities in deterministic order.
  pub fn packages(&self) -> &[LockedPackageIdentity] {
    &self.packages
  }
}

/// Cargo package identity and its logical local root, when source-backed here.
pub struct SnapshotPackage {
  id: PackageId,
  name: String,
  version: String,
  source: Option<Source>,
  checksum: Option<String>,
  workspace_member: bool,
  manifest_path: Option<RepositoryPath>,
  package_root: Option<PathBuf>,
}

impl SnapshotPackage {
  /// Return Cargo's exact package identity.
  pub fn id(&self) -> &PackageId {
    &self.id
  }

  /// Return the package name.
  pub fn name(&self) -> &str {
    &self.name
  }

  /// Return the package semantic version as Cargo rendered it.
  pub fn version(&self) -> &str {
    &self.version
  }

  /// Return Cargo's external source identity, or `None` for a local package.
  pub fn source(&self) -> Option<&Source> {
    self.source.as_ref()
  }

  /// Return the checksum bound by Cargo.lock, when available.
  pub fn checksum(&self) -> Option<&str> {
    self.checksum.as_deref()
  }

  /// Return whether Cargo selected this package as a workspace member.
  pub fn is_workspace_member(&self) -> bool {
    self.workspace_member
  }

  /// Return the local manifest path relative to the source root.
  pub fn manifest_path(&self) -> Option<&RepositoryPath> {
    self.manifest_path.as_ref()
  }

  /// Return the normalized local package root.
  ///
  /// The empty path denotes a package at the logical source root. External
  /// registry and Git packages return `None`.
  pub fn package_root(&self) -> Option<&Path> {
    self.package_root.as_deref()
  }
}

/// One coherent immutable capture of authoritative workspace inputs.
///
/// Derived Cargo metadata views are cached lazily and never contribute to the
/// snapshot identity. Selected target specifications and effective tool
/// settings are authoritative inputs, while the base resolution is the exact
/// view already loaded for the owning context. Repository-owned paths use
/// logical identities; external paths remain identity inputs when their
/// location changes Cargo or compiler behavior.
pub struct WorkspaceSnapshot {
  id: SnapshotId,
  source_root: PathBuf,
  configuration_fingerprint: String,
  toolchain_fingerprint: String,
  cargo_config_identity: Arc<[u8]>,
  toolchain_identity: Arc<[u8]>,
  cargo: Arc<CargoState>,
  source: Arc<SourceSnapshot>,
  manifests: Vec<SnapshotFile>,
  lockfile: Option<LockfileSnapshot>,
  rail_config: Option<SnapshotFile>,
  config: Option<Arc<RailConfig>>,
  cargo_config: Arc<CargoConfigSnapshot>,
  packages: Vec<SnapshotPackage>,
  toolchain: ToolchainIdentity,
  targets: Vec<TargetIdentity>,
  base_resolution: Arc<ResolutionView>,
  derived: Arc<DerivedViews>,
}

pub(crate) struct DerivedViews {
  workspace_root: PathBuf,
  analysis_targets: Vec<String>,
  resolutions: Arc<ResolutionViews>,
  multi_target_metadata: OnceLock<Result<Arc<MultiTargetMetadata>, SnapshotViewError>>,
  target_cfg_sets: OnceLock<Result<Arc<std::collections::HashMap<String, TargetCfgSet>>, SnapshotViewError>>,
}

impl DerivedViews {
  pub(crate) fn new(workspace_root: PathBuf, analysis_targets: Vec<String>, resolutions: Arc<ResolutionViews>) -> Self {
    Self {
      workspace_root,
      analysis_targets,
      resolutions,
      multi_target_metadata: OnceLock::new(),
      target_cfg_sets: OnceLock::new(),
    }
  }

  pub(crate) fn resolution_view(&self, request: ResolutionRequest) -> RailResult<Arc<ResolutionView>> {
    self.resolutions.view(request)
  }

  pub(crate) fn cargo_current_dir(&self) -> &Path {
    self.resolutions.cargo_current_dir()
  }

  pub(crate) fn multi_target_metadata(&self) -> RailResult<Arc<MultiTargetMetadata>> {
    cached_snapshot_view(self.multi_target_metadata.get_or_init(|| {
      if !self.analysis_targets.is_empty() {
        crate::progress!("Loading metadata for {} target(s)...", self.analysis_targets.len());
      }
      MultiTargetMetadata::load_parallel(&self.resolutions, &self.analysis_targets)
        .map(Arc::new)
        .map_err(SnapshotViewError::capture)
    }))
  }

  pub(crate) fn target_cfg_sets(&self) -> RailResult<Arc<std::collections::HashMap<String, TargetCfgSet>>> {
    cached_snapshot_view(self.target_cfg_sets.get_or_init(|| {
      let mut targets = Vec::with_capacity(self.analysis_targets.len() + 1);
      targets.push("default");
      targets.extend(self.analysis_targets.iter().map(String::as_str));
      load_target_cfg_sets(&self.workspace_root, &targets)
        .map(Arc::new)
        .map_err(SnapshotViewError::capture)
    }))
  }
}

#[derive(Clone)]
struct SnapshotViewError {
  message: String,
  help: Option<String>,
}

impl SnapshotViewError {
  fn capture(error: RailError) -> Self {
    Self {
      message: error.to_string(),
      help: error.help_message(),
    }
  }

  fn to_error(&self) -> RailError {
    self.help.as_ref().map_or_else(
      || RailError::message(self.message.clone()),
      |help| RailError::with_help(self.message.clone(), help.clone()),
    )
  }
}

pub(crate) struct CapturedRailConfig {
  path: PathBuf,
  bytes: Vec<u8>,
  config: Arc<RailConfig>,
}

pub(crate) struct CapturedLockfile {
  path: PathBuf,
  bytes: Vec<u8>,
}

impl CapturedLockfile {
  pub(crate) fn new(path: PathBuf, bytes: Vec<u8>) -> Self {
    Self { path, bytes }
  }

  pub(crate) fn path(&self) -> &Path {
    &self.path
  }

  pub(crate) fn bytes(&self) -> &[u8] {
    &self.bytes
  }
}

impl CapturedRailConfig {
  pub(crate) fn new(path: PathBuf, bytes: Vec<u8>, config: Arc<RailConfig>) -> Self {
    Self { path, bytes, config }
  }
}

impl WorkspaceSnapshot {
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn capture(
    source_root: &Path,
    mut source: Arc<SourceSnapshot>,
    cargo: Arc<CargoState>,
    rail_config: Option<CapturedRailConfig>,
    generated_lockfile: Option<CapturedLockfile>,
    resolution_inputs: ResolutionInputs,
    targets: Vec<TargetIdentity>,
    derived: Arc<DerivedViews>,
  ) -> RailResult<Self> {
    let metadata = cargo.metadata();
    let source_root = crate::utils::canonicalize_existing(source_root)?;
    let mut partitioned_source_paths = resolution_inputs
      .cargo_config
      .repository_config_paths(&source_root)?
      .into_iter()
      .collect::<BTreeSet<_>>();
    let lockfile_path = source_root_for_lockfile(metadata)?;
    partitioned_source_paths.insert(relative_path(&source_root, &lockfile_path)?);
    if !partitioned_source_paths.is_empty() {
      Arc::make_mut(&mut source).retain(|entry| !partitioned_source_paths.contains(&entry.path));
    }
    let root_manifest = source_root_for_workspace_manifest(source_root.as_path(), metadata)?;
    let mut manifest_paths = BTreeMap::new();
    manifest_paths.insert(relative_path(&source_root, &root_manifest)?, root_manifest);

    let workspace_members: BTreeSet<_> = metadata.workspace_members.iter().collect();
    let mut package_locations = BTreeMap::new();
    let mut local_manifest_owners = BTreeMap::new();
    for package in &metadata.packages {
      if package.source.is_some() {
        continue;
      }
      let manifest = package.manifest_path.as_std_path();
      let relative = relative_path(&source_root, manifest).map_err(|_| {
        RailError::with_help(
          format!(
            "Local package '{}' manifest '{}' is outside snapshot source root '{}'",
            package.id,
            manifest.display(),
            source_root.display()
          ),
          "place local path packages inside the captured repository/workspace boundary before creating a workspace snapshot",
        )
      })?;
      let package_root = relative
        .as_path()
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .to_path_buf();
      if let Some(existing) = local_manifest_owners.insert(relative.clone(), package.id.clone()) {
        let mut package_ids = [existing.to_string(), package.id.to_string()];
        package_ids.sort_unstable();
        return Err(RailError::message(format!(
          "Local packages '{}' and '{}' claim the same manifest '{}'",
          package_ids[0], package_ids[1], relative
        )));
      }
      manifest_paths.insert(relative.clone(), manifest.to_path_buf());
      package_locations.insert(package.id.clone(), (relative, package_root));
    }

    let manifests = manifest_paths
      .into_values()
      .map(|path| capture_required_file(&source, &source_root, &path, "Cargo manifest"))
      .collect::<RailResult<Vec<_>>>()?;
    let source_lockfile = capture_optional_file(&source, &source_root, &lockfile_path, "Cargo lockfile")?;
    let lockfile = match (source_lockfile, generated_lockfile) {
      (Some(file), None) => Some(file),
      (None, Some(captured)) => Some(capture_generated_lockfile(&source_root, metadata, captured)?),
      (None, None) => None,
      (Some(file), Some(captured)) => {
        if file.bytes() != captured.bytes {
          return Err(RailError::with_help(
            "Cargo-generated lockfile changed while the workspace snapshot was being captured",
            "retry after workspace input changes have stopped",
          ));
        }
        Some(file)
      }
    }
    .map(LockfileSnapshot::from_file)
    .transpose()?;
    let (rail_config, config) = rail_config
      .map(|config| {
        let captured = capture_required_file(&source, &source_root, &config.path, "cargo-rail configuration")?;
        if captured.bytes() != config.bytes.as_slice() {
          return Err(RailError::with_help(
            format!(
              "cargo-rail configuration '{}' changed after it was parsed",
              captured.path()
            ),
            "retry after workspace configuration changes have stopped",
          ));
        }
        Ok((captured, config.config))
      })
      .transpose()?
      .map_or((None, None), |(file, config)| (Some(file), Some(config)));

    let packages = package_snapshots(metadata, &workspace_members, &package_locations, lockfile.as_ref())?;
    let base_resolution = derived.resolution_view(ResolutionRequest::default())?;
    let cargo_config_identity = resolution_inputs
      .cargo_config
      .portable_snapshot_identity(&source_root)?;
    let toolchain_identity = resolution_inputs.toolchain.portable_snapshot_identity(&source_root)?;
    let configuration_fingerprint = snapshot_configuration_fingerprint(rail_config.as_ref(), &cargo_config_identity);
    let toolchain_fingerprint = format!("sha256:{}", ContentDigest::sha256(&toolchain_identity));
    let excluded_paths = BTreeSet::new();
    let id = compute_snapshot_id(
      &source,
      &manifests,
      lockfile.as_ref(),
      rail_config.as_ref(),
      &cargo_config_identity,
      &toolchain_identity,
      &source_root,
      &targets,
      &excluded_paths,
    )?;
    Ok(Self {
      id,
      source_root,
      configuration_fingerprint,
      toolchain_fingerprint,
      cargo_config_identity: cargo_config_identity.into(),
      toolchain_identity: toolchain_identity.into(),
      cargo,
      source,
      manifests,
      lockfile,
      rail_config,
      config,
      cargo_config: resolution_inputs.cargo_config,
      packages,
      toolchain: resolution_inputs.toolchain,
      targets,
      base_resolution,
      derived,
    })
  }

  /// Return the portable identity of this exact authoritative capture.
  pub fn id(&self) -> SnapshotId {
    self.id
  }

  pub(crate) fn configuration_fingerprint(&self) -> &str {
    &self.configuration_fingerprint
  }

  pub(crate) fn toolchain_fingerprint(&self) -> &str {
    &self.toolchain_fingerprint
  }

  pub(crate) fn lockfile_fingerprint(&self) -> String {
    self
      .lockfile()
      .map(|lockfile| format!("sha256:{}", lockfile.file().digest()))
      .unwrap_or_else(|| "absent".to_string())
  }

  pub(crate) fn id_excluding_paths(&self, paths: &BTreeSet<PathBuf>) -> RailResult<SnapshotId> {
    let paths = paths
      .iter()
      .map(|path| RepositoryPath::new(path))
      .collect::<RailResult<BTreeSet<_>>>()?;
    compute_snapshot_id(
      &self.source,
      &self.manifests,
      self.lockfile.as_ref(),
      self.rail_config.as_ref(),
      &self.cargo_config_identity,
      &self.toolchain_identity,
      &self.source_root,
      &self.targets,
      &paths,
    )
  }

  /// Return the backend-independent canonical source capture.
  pub fn source(&self) -> &SourceSnapshot {
    &self.source
  }

  pub(crate) fn cargo(&self) -> &Arc<CargoState> {
    &self.cargo
  }

  /// Return root and local-package manifests in logical path order.
  pub fn manifests(&self) -> &[SnapshotFile] {
    &self.manifests
  }

  /// Return the exact lockfile and parsed identities, when present.
  pub fn lockfile(&self) -> Option<&LockfileSnapshot> {
    self.lockfile.as_ref()
  }

  /// Return the lossless cargo-rail configuration bytes, when present.
  pub fn rail_config(&self) -> Option<&SnapshotFile> {
    self.rail_config.as_ref()
  }

  /// Return the parsed cargo-rail configuration paired with the lossless bytes.
  pub fn config(&self) -> Option<&RailConfig> {
    self.config.as_deref()
  }

  pub(crate) fn shared_config(&self) -> Option<&Arc<RailConfig>> {
    self.config.as_ref()
  }

  /// Return effective non-secret Cargo configuration inputs and provenance.
  pub fn cargo_config(&self) -> &CargoConfigSnapshot {
    &self.cargo_config
  }

  /// Return exact Cargo package/source identities in package-ID order.
  pub fn packages(&self) -> &[SnapshotPackage] {
    &self.packages
  }

  /// Return the resolved Cargo, rustc, rustdoc, and wrapper identity.
  pub fn toolchain(&self) -> &ToolchainIdentity {
    &self.toolchain
  }

  /// Return host and selected target identities in deterministic order.
  pub fn targets(&self) -> &[TargetIdentity] {
    &self.targets
  }

  /// Return the already-loaded canonical native/default resolution.
  pub fn base_resolution(&self) -> &ResolutionView {
    &self.base_resolution
  }

  pub(crate) fn resolution_view(&self, request: ResolutionRequest) -> RailResult<Arc<ResolutionView>> {
    self.derived.resolution_view(request)
  }

  pub(crate) fn cargo_current_dir(&self) -> &Path {
    self.derived.cargo_current_dir()
  }

  pub(crate) fn multi_target_metadata(&self) -> RailResult<Arc<MultiTargetMetadata>> {
    self.derived.multi_target_metadata()
  }

  pub(crate) fn target_cfg_sets(&self) -> RailResult<Arc<std::collections::HashMap<String, TargetCfgSet>>> {
    self.derived.target_cfg_sets()
  }

  pub(crate) fn validate_external_targets_unchanged(&self) -> RailResult<()> {
    self
      .targets
      .iter()
      .try_for_each(TargetIdentity::validate_custom_specification_unchanged)
  }

  pub(crate) fn validate_live_authoritative_inputs(&self) -> RailResult<()> {
    self.validate_resolution_environment_unchanged()?;
    self.validate_authoritative_files_unchanged()?;
    self.validate_external_targets_unchanged()
  }

  pub(crate) fn validate_authoritative_files_unchanged(&self) -> RailResult<()> {
    for file in self
      .manifests
      .iter()
      .chain(self.lockfile.iter().map(LockfileSnapshot::file))
      .chain(self.rail_config.iter())
    {
      let path = self.source_root.join(file.path().as_path());
      let metadata = fs::symlink_metadata(&path).map_err(|error| {
        RailError::message(format!(
          "failed to revalidate authoritative snapshot file '{}': {error}",
          file.path()
        ))
      })?;
      if !metadata.file_type().is_file() {
        return Err(RailError::with_help(
          format!(
            "authoritative snapshot file '{}' is no longer a regular file",
            file.path()
          ),
          "retry after workspace input changes have stopped",
        ));
      }
      let bytes = fs::read(&path).map_err(|error| {
        RailError::message(format!(
          "failed to revalidate authoritative snapshot file '{}': {error}",
          file.path()
        ))
      })?;
      if ContentDigest::sha256(&bytes) != file.digest() {
        return Err(RailError::with_help(
          format!("authoritative snapshot file '{}' changed after capture", file.path()),
          "retry after workspace input changes have stopped",
        ));
      }
    }
    Ok(())
  }

  pub(crate) fn validate_resolution_environment_unchanged(&self) -> RailResult<()> {
    let current = ResolutionViews::capture_inputs(self.derived.cargo_current_dir())?;
    if current.cargo_config != self.cargo_config || current.toolchain != self.toolchain {
      return Err(RailError::with_help(
        "Cargo configuration or toolchain identity changed after workspace snapshot capture",
        "retry after workspace inputs and toolchain updates have stopped",
      ));
    }
    Ok(())
  }
}

fn capture_generated_lockfile(
  source_root: &Path,
  metadata: &Metadata,
  captured: CapturedLockfile,
) -> RailResult<SnapshotFile> {
  let expected = source_root_for_lockfile(metadata)?;
  let file_type = fs::symlink_metadata(&captured.path)
    .map_err(|error| {
      RailError::message(format!(
        "failed to inspect generated Cargo lockfile '{}': {error}",
        captured.path.display()
      ))
    })?
    .file_type();
  if !file_type.is_file() {
    return Err(RailError::with_help(
      format!(
        "generated Cargo lockfile '{}' is not a regular file",
        captured.path.display()
      ),
      "replace the lockfile symlink or special entry with a regular file and retry",
    ));
  }
  let path = crate::utils::canonicalize_existing(&captured.path)?;
  if path != crate::utils::canonicalize_existing(&expected)? {
    return Err(RailError::message(format!(
      "generated Cargo lockfile '{}' does not match workspace lockfile '{}'",
      path.display(),
      expected.display()
    )));
  }
  let current = fs::read(&path).map_err(|error| {
    RailError::message(format!(
      "failed to revalidate generated Cargo lockfile '{}': {error}",
      path.display()
    ))
  })?;
  if current != captured.bytes {
    return Err(RailError::with_help(
      "Cargo-generated lockfile changed while the workspace snapshot was being captured",
      "retry after workspace input changes have stopped",
    ));
  }
  Ok(SnapshotFile {
    path: relative_path(source_root, &path)?,
    digest: ContentDigest::sha256(&captured.bytes),
    bytes: captured.bytes.into(),
  })
}

fn cached_snapshot_view<T>(value: &Result<Arc<T>, SnapshotViewError>) -> RailResult<Arc<T>> {
  value.as_ref().map(Arc::clone).map_err(SnapshotViewError::to_error)
}

#[allow(clippy::too_many_arguments)]
fn compute_snapshot_id(
  source: &SourceSnapshot,
  manifests: &[SnapshotFile],
  lockfile: Option<&LockfileSnapshot>,
  rail_config: Option<&SnapshotFile>,
  cargo_config_identity: &[u8],
  toolchain_identity: &[u8],
  source_root: &Path,
  targets: &[TargetIdentity],
  excluded_paths: &BTreeSet<RepositoryPath>,
) -> RailResult<SnapshotId> {
  let mut identity = SnapshotIdentityHasher::new(b"cargo-rail-workspace-snapshot\0");
  identity.frame(b"version", &SNAPSHOT_ID_VERSION.to_le_bytes());

  for entry in source.tree().entries() {
    if excluded_paths.contains(&entry.path) {
      continue;
    }
    let mut framed = Vec::new();
    append_identity_frame(&mut framed, b"path", entry.path.as_str().as_bytes());
    match &entry.kind {
      SourceEntryKind::RegularFile { digest, executable } => {
        append_identity_frame(&mut framed, b"kind", b"regular-file");
        append_identity_frame(&mut framed, b"content", digest.as_bytes());
        append_identity_frame(&mut framed, b"executable", &[u8::from(*executable)]);
      }
      SourceEntryKind::Symlink { target } => {
        append_identity_frame(&mut framed, b"kind", b"symlink");
        append_identity_frame(&mut framed, b"target", target.as_bytes());
      }
      SourceEntryKind::Deleted => {
        return Err(RailError::message(format!(
          "workspace snapshot source tree contains deleted entry '{}'",
          entry.path
        )));
      }
    }
    identity.frame(b"source-entry", &framed);
  }

  for manifest in manifests {
    identity.snapshot_file(b"manifest", manifest);
  }
  match lockfile {
    Some(lockfile) => identity.snapshot_file(b"lockfile", lockfile.file()),
    None => identity.frame(b"lockfile", b"absent"),
  }
  match rail_config {
    Some(config) => identity.snapshot_file(b"rail-config", config),
    None => identity.frame(b"rail-config", b"absent"),
  }

  identity.frame(b"cargo-config", cargo_config_identity);
  identity.frame(b"toolchain", toolchain_identity);
  for target in targets {
    identity.frame(b"target", &target.portable_snapshot_identity(source_root)?);
  }

  Ok(SnapshotId {
    version: SNAPSHOT_ID_VERSION,
    digest: identity.finish(),
  })
}

struct SnapshotIdentityHasher {
  hasher: Sha256,
  input_bytes: usize,
}

impl SnapshotIdentityHasher {
  fn new(domain: &[u8]) -> Self {
    let mut identity = Self {
      hasher: Sha256::new(),
      input_bytes: 0,
    };
    identity.update(domain);
    identity
  }

  fn frame(&mut self, tag: &[u8], value: &[u8]) {
    self.update(&(tag.len() as u64).to_le_bytes());
    self.update(tag);
    self.update(&(value.len() as u64).to_le_bytes());
    self.update(value);
  }

  fn snapshot_file(&mut self, tag: &[u8], file: &SnapshotFile) {
    let mut framed = Vec::new();
    append_identity_frame(&mut framed, b"path", file.path().as_str().as_bytes());
    append_identity_frame(&mut framed, b"content", file.digest().as_bytes());
    self.frame(tag, &framed);
  }

  fn update(&mut self, bytes: &[u8]) {
    self.hasher.update(bytes);
    self.input_bytes = self.input_bytes.saturating_add(bytes.len());
  }

  fn finish(self) -> ContentDigest {
    crate::instrumentation::record_hash_operation();
    crate::instrumentation::record_hash_input_bytes(self.input_bytes);
    ContentDigest::from_sha256_bytes(self.hasher.finalize().into())
  }
}

fn snapshot_configuration_fingerprint(rail_config: Option<&SnapshotFile>, cargo_config_identity: &[u8]) -> String {
  let mut identity = Vec::from(&b"cargo-rail-snapshot-configuration-v1\0"[..]);
  match rail_config {
    Some(config) => {
      let mut framed = Vec::new();
      let normalized = crate::utils::normalize_line_endings(config.bytes());
      let digest = ContentDigest::sha256(&normalized);
      append_identity_frame(&mut framed, b"path", config.path().as_str().as_bytes());
      append_identity_frame(&mut framed, b"content", digest.as_bytes());
      append_identity_frame(&mut identity, b"rail-config", &framed);
    }
    None => append_identity_frame(&mut identity, b"rail-config", b"absent"),
  }
  append_identity_frame(&mut identity, b"cargo-config", cargo_config_identity);
  format!("sha256:{}", ContentDigest::sha256(&identity))
}

fn append_identity_frame(output: &mut Vec<u8>, tag: &[u8], value: &[u8]) {
  output.extend_from_slice(&(tag.len() as u64).to_le_bytes());
  output.extend_from_slice(tag);
  output.extend_from_slice(&(value.len() as u64).to_le_bytes());
  output.extend_from_slice(value);
}

impl LockfileSnapshot {
  fn from_file(file: SnapshotFile) -> RailResult<Self> {
    #[derive(Deserialize)]
    struct Document {
      #[serde(default)]
      package: Vec<Package>,
    }
    #[derive(Deserialize)]
    struct Package {
      name: String,
      version: String,
      source: Option<String>,
      checksum: Option<String>,
    }

    let text = std::str::from_utf8(file.bytes())
      .map_err(|_| RailError::message(format!("Cargo lockfile '{}' is not valid UTF-8", file.path())))?;
    let document: Document = toml_edit::de::from_str(text).map_err(|_| {
      RailError::with_help(
        format!("failed to parse Cargo lockfile '{}'", file.path()),
        "fix Cargo.lock syntax; raw parser context is suppressed because source URLs may contain credentials",
      )
    })?;
    let mut packages = document
      .package
      .into_iter()
      .map(|package| LockedPackageIdentity {
        name: package.name,
        version: package.version,
        source: package.source,
        checksum: package.checksum,
      })
      .collect::<Vec<_>>();
    if let Some(package) = packages
      .iter()
      .find(|package| package.source.as_deref().is_some_and(credential_bearing_url))
    {
      return Err(RailError::with_help(
        format!(
          "Cargo lockfile package '{} {}' contains a credential-bearing source URL",
          package.name, package.version
        ),
        "move credentials to Cargo's credential provider and regenerate Cargo.lock with a secret-free source URL",
      ));
    }
    packages.sort_unstable();
    if let Some(duplicate) = packages.windows(2).find(|pair| {
      pair[0].name == pair[1].name && pair[0].version == pair[1].version && pair[0].source == pair[1].source
    }) {
      return Err(RailError::message(format!(
        "Cargo lockfile contains duplicate package identity '{} {}'",
        duplicate[0].name, duplicate[0].version
      )));
    }
    Ok(Self { file, packages })
  }
}

fn package_snapshots(
  metadata: &Metadata,
  workspace_members: &BTreeSet<&PackageId>,
  locations: &BTreeMap<PackageId, (RepositoryPath, PathBuf)>,
  lockfile: Option<&LockfileSnapshot>,
) -> RailResult<Vec<SnapshotPackage>> {
  let checksums = lockfile
    .map(|lockfile| {
      lockfile
        .packages
        .iter()
        .map(|locked| {
          (
            (locked.name.clone(), locked.version.clone(), locked.source.clone()),
            locked.checksum.clone(),
          )
        })
        .collect::<BTreeMap<_, _>>()
    })
    .unwrap_or_default();
  let mut packages = metadata
    .packages
    .iter()
    .map(|package| {
      if package
        .source
        .as_ref()
        .is_some_and(|source| credential_bearing_url(&source.repr))
      {
        return Err(RailError::with_help(
          format!(
            "Package '{} {}' contains a credential-bearing Cargo source URL",
            package.name, package.version
          ),
          "move credentials to Cargo's credential provider and keep dependency source URLs secret-free",
        ));
      }
      let location = locations.get(&package.id);
      let name = package.name.to_string();
      let version = package.version.to_string();
      let source = package.source.clone();
      let checksum = checksums
        .get(&(
          name.clone(),
          version.clone(),
          source.as_ref().map(|source| source.repr.clone()),
        ))
        .cloned()
        .flatten();
      Ok(SnapshotPackage {
        id: package.id.clone(),
        name,
        version,
        source,
        checksum,
        workspace_member: workspace_members.contains(&package.id),
        manifest_path: location.map(|(manifest, _)| manifest.clone()),
        package_root: location.map(|(_, root)| root.clone()),
      })
    })
    .collect::<RailResult<Vec<_>>>()?;
  packages.sort_unstable_by(|left, right| left.id.repr.cmp(&right.id.repr));
  Ok(packages)
}

fn source_root_for_workspace_manifest(source_root: &Path, metadata: &Metadata) -> RailResult<PathBuf> {
  let workspace_root = metadata.workspace_root.as_std_path();
  let manifest = workspace_root.join("Cargo.toml");
  relative_path(source_root, &manifest)?;
  Ok(manifest)
}

fn source_root_for_lockfile(metadata: &Metadata) -> RailResult<PathBuf> {
  let workspace_root = metadata.workspace_root.as_std_path();
  if !workspace_root.is_absolute() {
    return Err(RailError::message(format!(
      "Cargo workspace root '{}' is not absolute",
      workspace_root.display()
    )));
  }
  Ok(workspace_root.join("Cargo.lock"))
}

fn relative_path(source_root: &Path, path: &Path) -> RailResult<RepositoryPath> {
  if let Ok(relative) = path.strip_prefix(source_root) {
    return RepositoryPath::new(relative);
  }
  let relative = crate::utils::path_relative_to(source_root, path).map_err(|_| {
    RailError::message(format!(
      "authoritative workspace path '{}' is outside snapshot source root '{}'",
      path.display(),
      source_root.display()
    ))
  })?;
  RepositoryPath::new(&relative)
}

fn capture_required_file(
  source: &SourceSnapshot,
  source_root: &Path,
  path: &Path,
  description: &str,
) -> RailResult<SnapshotFile> {
  capture_file(source, source_root, path, description)?.ok_or_else(|| {
    RailError::message(format!(
      "{description} '{}' is absent from the canonical source tree",
      path.display()
    ))
  })
}

fn capture_optional_file(
  source: &SourceSnapshot,
  source_root: &Path,
  path: &Path,
  description: &str,
) -> RailResult<Option<SnapshotFile>> {
  capture_file(source, source_root, path, description)
}

fn capture_file(
  source: &SourceSnapshot,
  source_root: &Path,
  path: &Path,
  description: &str,
) -> RailResult<Option<SnapshotFile>> {
  if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
    return Err(RailError::with_help(
      format!("{description} '{}' is a symbolic link", path.display()),
      "replace the authoritative file symlink with a regular file before snapshot capture",
    ));
  }
  let relative = relative_path(source_root, path)?;
  let Some(entry) = source
    .tree()
    .entries()
    .binary_search_by(|entry| entry.path.cmp(&relative))
    .ok()
    .map(|index| &source.tree().entries()[index])
  else {
    return capture_declared_file_outside_tree(relative, path, description);
  };
  let SourceEntryKind::RegularFile { digest, .. } = entry.kind else {
    return Err(RailError::with_help(
      format!("{description} '{}' is not a regular file", path.display()),
      "replace the authoritative file symlink or special entry with a regular file before snapshot capture",
    ));
  };
  let bytes = fs::read(path)
    .map_err(|error| RailError::message(format!("failed to read {description} '{}': {error}", path.display())))?;
  let current_digest = ContentDigest::sha256(&bytes);
  if current_digest != digest {
    return Err(RailError::with_help(
      format!(
        "{description} '{}' changed after canonical source capture",
        path.display()
      ),
      "retry after workspace source changes have stopped",
    ));
  }
  Ok(Some(SnapshotFile {
    path: relative,
    bytes: bytes.into(),
    digest,
  }))
}

fn capture_declared_file_outside_tree(
  relative: RepositoryPath,
  path: &Path,
  description: &str,
) -> RailResult<Option<SnapshotFile>> {
  let before = match fs::symlink_metadata(path) {
    Ok(metadata) => metadata,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
    Err(error) => {
      return Err(RailError::message(format!(
        "failed to inspect {description} '{}': {error}",
        path.display()
      )));
    }
  };
  if !before.file_type().is_file() {
    return Err(RailError::with_help(
      format!("{description} '{}' is not a regular file", path.display()),
      "replace the authoritative file symlink or special entry with a regular file before snapshot capture",
    ));
  }
  let bytes = fs::read(path)
    .map_err(|error| RailError::message(format!("failed to read {description} '{}': {error}", path.display())))?;
  let after = fs::symlink_metadata(path).map_err(|error| {
    RailError::message(format!(
      "failed to reinspect {description} '{}': {error}",
      path.display()
    ))
  })?;
  let repeated = fs::read(path)
    .map_err(|error| RailError::message(format!("failed to reread {description} '{}': {error}", path.display())))?;
  let repeated_metadata = fs::symlink_metadata(path).map_err(|error| {
    RailError::message(format!(
      "failed to reinspect {description} '{}' after reread: {error}",
      path.display()
    ))
  })?;
  let metadata_changed = |left: &fs::Metadata, right: &fs::Metadata| {
    left.file_type() != right.file_type()
      || left.len() != right.len()
      || left.modified().ok() != right.modified().ok()
      || left.permissions().readonly() != right.permissions().readonly()
  };
  if bytes != repeated || metadata_changed(&before, &after) || metadata_changed(&after, &repeated_metadata) {
    return Err(RailError::with_help(
      format!("{description} '{}' changed during snapshot capture", path.display()),
      "retry after workspace source changes have stopped",
    ));
  }
  let digest = ContentDigest::sha256(&bytes);
  Ok(Some(SnapshotFile {
    path: relative,
    bytes: bytes.into(),
    digest,
  }))
}

#[cfg(test)]
mod tests {
  use super::*;

  fn lockfile(bytes: &[u8]) -> SnapshotFile {
    SnapshotFile {
      path: RepositoryPath::new(Path::new("Cargo.lock")).expect("lockfile path should be valid"),
      bytes: Arc::from(bytes),
      digest: ContentDigest::sha256(bytes),
    }
  }

  #[test]
  fn lockfile_snapshot_preserves_source_and_checksum_identity() {
    let snapshot = LockfileSnapshot::from_file(lockfile(
      b"version = 4\n\n[[package]]\nname = \"dep\"\nversion = \"1.2.3\"\nsource = \"registry+https://example.invalid/index\"\nchecksum = \"abc123\"\n",
    ))
    .expect("valid lockfile should parse");

    assert_eq!(snapshot.packages().len(), 1);
    assert_eq!(snapshot.packages()[0].name(), "dep");
    assert_eq!(snapshot.packages()[0].version(), "1.2.3");
    assert_eq!(
      snapshot.packages()[0].source(),
      Some("registry+https://example.invalid/index")
    );
    assert_eq!(snapshot.packages()[0].checksum(), Some("abc123"));
  }

  #[test]
  fn lockfile_snapshot_rejects_duplicate_identity_with_conflicting_checksum() {
    let error = LockfileSnapshot::from_file(lockfile(
      b"version = 4\n\n[[package]]\nname = \"dep\"\nversion = \"1.2.3\"\nsource = \"registry+https://example.invalid/index\"\nchecksum = \"abc123\"\n\n[[package]]\nname = \"dep\"\nversion = \"1.2.3\"\nsource = \"registry+https://example.invalid/index\"\nchecksum = \"def456\"\n",
    ))
    .err()
    .expect("duplicate locked identities must fail closed");
    assert!(error.to_string().contains("duplicate package identity"), "{error}");
  }

  #[test]
  fn lockfile_snapshot_rejects_credential_bearing_source_urls() {
    let error = LockfileSnapshot::from_file(lockfile(
      b"version = 4\n\n[[package]]\nname = \"dep\"\nversion = \"1.2.3\"\nsource = \"registry+https://user:password@example.invalid/index\"\nchecksum = \"abc123\"\n",
    ))
    .err()
    .expect("credential-bearing locked source must fail closed");
    assert!(error.to_string().contains("credential-bearing source URL"), "{error}");
  }
}
