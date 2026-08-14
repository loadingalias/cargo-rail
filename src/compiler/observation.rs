//! Versioned compilation-unit identities and exact post-execution evidence.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs;
#[cfg(windows)]
use std::io::Read as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{RailError, RailResult};
use crate::executable::ExecutableIdentity;
use crate::source::ContentDigest;
use crate::workspace::WorkspaceSnapshot;

pub(crate) const COMPILATION_OBSERVATION_VERSION: u32 = 6;
const COMPILER_CACHE_WRAPPER_METADATA_VERSION: u32 = 2;
const COMPILATION_UNIT_VERSION: u32 = 2;
const RAW_INVOCATION_VERSION: u32 = 5;

/// Typed Cargo target domain for one compiler or rustdoc invocation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompilationTargetKind {
  Library,
  Binary,
  Test,
  Example,
  Benchmark,
  Documentation,
  ProcMacro,
  BuildScript,
  Other(String),
}

/// Whether the unit executes on the compiler host or produces target code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompilationRole {
  Host,
  Target,
  Unknown,
}

/// Stable compiler surface responsible for the unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompilerMode {
  Rustc,
  Rustdoc,
  Unknown,
}

/// Whether rustc delegates a final native-link step for this unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LinkerResponsibility {
  None,
  RustcDriver,
  Unknown,
}

/// Cargo's stable compiler-artifact profile fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CompilationProfile {
  pub(crate) opt_level: String,
  pub(crate) debuginfo: String,
  pub(crate) debug_assertions: bool,
  pub(crate) overflow_checks: bool,
  pub(crate) test: bool,
}

/// One exact dependency artifact edge supplied to the compiler.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct CompilationDependencyEdge {
  pub(crate) extern_name: String,
  pub(crate) artifact_digest: String,
  pub(crate) producer_unit: Option<String>,
}

/// Verified build-script action/result pair that affects one compilation unit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct BuildScriptResultDependency {
  pub(crate) producer_action: String,
  pub(crate) result_digest: String,
}

/// One observed build-script execution ready for downstream propagation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BuildScriptResultBinding {
  pub(crate) package: String,
  pub(crate) action_key: Option<String>,
  pub(crate) result_digest: Option<String>,
}

/// Smallest complete typed identity for one observed compilation unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CompilationUnit {
  version: u32,
  pub(crate) package: String,
  pub(crate) target_kind: CompilationTargetKind,
  pub(crate) cargo_target_kinds: BTreeSet<String>,
  pub(crate) target_name: String,
  pub(crate) crate_types: BTreeSet<String>,
  pub(crate) role: CompilationRole,
  pub(crate) mode: CompilerMode,
  pub(crate) platform: String,
  pub(crate) target_specification: String,
  pub(crate) profile: CompilationProfile,
  pub(crate) features: BTreeSet<String>,
  pub(crate) cfg: BTreeSet<String>,
  pub(crate) emit_modes: BTreeSet<String>,
  pub(crate) linker_responsibility: LinkerResponsibility,
  pub(crate) compiler_arguments: Vec<String>,
  pub(crate) dependencies: Vec<CompilationDependencyEdge>,
  pub(crate) build_script_results: BTreeSet<BuildScriptResultDependency>,
}

impl CompilationUnit {
  /// Content identity of every modeled compilation-unit field.
  pub(crate) fn identity(&self) -> RailResult<String> {
    let bytes = serde_json::to_vec(self)?;
    Ok(format!(
      "v{COMPILATION_UNIT_VERSION}-sha256-{}",
      ContentDigest::sha256(&bytes)
    ))
  }
}

/// Portable path retained for later exact-byte revalidation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "root", content = "path", rename_all = "snake_case")]
pub(crate) enum ObservationPath {
  Repository(String),
  Host(String),
}

impl ObservationPath {
  pub(crate) fn capture(path: &Path, current_dir: &Path, source_root: &Path) -> Self {
    let absolute = if path.is_absolute() {
      path.to_path_buf()
    } else {
      current_dir.join(path)
    };
    if let Ok(relative) = absolute.strip_prefix(source_root) {
      return Self::Repository(crate::utils::path_to_git_format(relative));
    }
    if let Some(relative) = canonical_repository_relative(&absolute, source_root) {
      return Self::Repository(crate::utils::path_to_git_format(&relative));
    }
    Self::Host(crate::utils::path_to_git_format(&absolute))
  }

  pub(crate) fn resolve(&self, source_root: &Path) -> PathBuf {
    match self {
      Self::Repository(path) => source_root.join(path),
      Self::Host(path) => PathBuf::from(path),
    }
  }
}

fn canonical_repository_relative(path: &Path, source_root: &Path) -> Option<PathBuf> {
  let root = crate::utils::canonicalize_existing(source_root).ok()?;
  let canonical = match fs::symlink_metadata(path) {
    Ok(metadata) if metadata.file_type().is_symlink() => {
      let parent = crate::utils::canonicalize_existing(path.parent()?).ok()?;
      parent.join(path.file_name()?)
    }
    Ok(_) => crate::utils::canonicalize_existing(path).ok()?,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
      crate::utils::canonicalize_allow_missing(path).ok()?
    }
    Err(_) => return None,
  };
  canonical.strip_prefix(root).ok().map(Path::to_path_buf)
}

/// Exact regular-file or symlink observation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct FileObservation {
  pub(crate) path: ObservationPath,
  pub(crate) content_digest: String,
  pub(crate) executable: bool,
  pub(crate) symlink_target: Option<String>,
}

impl FileObservation {
  pub(crate) fn capture(path: &Path, current_dir: &Path, source_root: &Path) -> RailResult<Self> {
    Self::capture_counted(path, current_dir, source_root).map(|(observation, _)| observation)
  }

  pub(crate) fn capture_counted(path: &Path, current_dir: &Path, source_root: &Path) -> RailResult<(Self, u64)> {
    let absolute = if path.is_absolute() {
      path.to_path_buf()
    } else {
      current_dir.join(path)
    };
    let link_metadata = fs::symlink_metadata(&absolute).map_err(|error| {
      RailError::message(format!(
        "failed to inspect observed file '{}': {error}",
        absolute.display()
      ))
    })?;
    let symlink_target = link_metadata
      .file_type()
      .is_symlink()
      .then(|| fs::read_link(&absolute))
      .transpose()
      .map_err(|error| {
        RailError::message(format!(
          "failed to read observed symlink '{}': {error}",
          absolute.display()
        ))
      })?
      .map(|target| crate::utils::path_to_git_format(&target));
    let metadata = fs::metadata(&absolute).map_err(|error| {
      RailError::message(format!(
        "failed to inspect observed file '{}': {error}",
        absolute.display()
      ))
    })?;
    if !metadata.is_file() {
      return Err(RailError::message(format!(
        "observed path '{}' is not a regular file",
        absolute.display()
      )));
    }
    let bytes = read_observed_file(&absolute, &metadata)?;
    let bytes_read = bytes.len() as u64;
    Ok((
      Self {
        path: ObservationPath::capture(&absolute, current_dir, source_root),
        content_digest: format!("sha256:{}", ContentDigest::sha256(&bytes)),
        executable: is_executable(&metadata),
        symlink_target,
      },
      bytes_read,
    ))
  }

  pub(crate) fn revalidate(&self, source_root: &Path) -> bool {
    let path = self.path.resolve(source_root);
    Self::capture(&path, source_root, source_root).is_ok_and(|current| current == *self)
  }
}

#[cfg(windows)]
fn read_observed_file(path: &Path, metadata: &fs::Metadata) -> RailResult<Vec<u8>> {
  use crate::windows_fs::{observe_file, open_for_observation, open_for_stable_byte_observation, prove_local_ntfs};

  let mut opened = open_for_stable_byte_observation(path).map_err(|error| {
    RailError::message(format!(
      "failed to open observed file '{}' without following reparse points: {error}",
      path.display()
    ))
  })?;
  let before = observe_file(&opened).map_err(|error| {
    RailError::message(format!(
      "failed to capture handle-bound evidence for observed file '{}': {error}",
      path.display()
    ))
  })?;
  prove_local_ntfs(&opened, before.volume_serial_number).map_err(|error| {
    RailError::message(format!(
      "observed file '{}' is not on a proven local NTFS volume: {error}",
      path.display()
    ))
  })?;
  if before.size != metadata.len() {
    return Err(RailError::message(format!(
      "observed file '{}' changed before its bytes were read",
      path.display()
    )));
  }

  let capacity = usize::try_from(before.size).map_err(|_| {
    RailError::message(format!(
      "observed file '{}' exceeds the addressable byte bound",
      path.display()
    ))
  })?;
  let limit = before
    .size
    .checked_add(1)
    .ok_or_else(|| RailError::message(format!("observed file '{}' exceeds the byte bound", path.display())))?;
  let mut bytes = Vec::with_capacity(capacity);
  (&mut opened).take(limit).read_to_end(&mut bytes)?;

  let after = observe_file(&opened).map_err(|error| {
    RailError::message(format!(
      "observed file '{}' changed while its bytes were read: {error}",
      path.display()
    ))
  })?;
  let current = open_for_observation(path).and_then(|current| {
    let observation = observe_file(&current)?;
    prove_local_ntfs(&current, observation.volume_serial_number)?;
    Ok(observation)
  });
  let current = current.map_err(|error| {
    RailError::message(format!(
      "observed file '{}' changed before its path was revalidated: {error}",
      path.display()
    ))
  })?;
  if before != after || after != current || bytes.len() as u64 != before.size {
    return Err(RailError::message(format!(
      "observed file '{}' changed while it was captured",
      path.display()
    )));
  }
  Ok(bytes)
}

#[cfg(not(windows))]
fn read_observed_file(path: &Path, _metadata: &fs::Metadata) -> RailResult<Vec<u8>> {
  fs::read(path)
    .map_err(|error| RailError::message(format!("failed to read observed file '{}': {error}", path.display())))
}

/// Environment read reported by rustc dep-info without retaining plaintext values.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct EnvironmentObservation {
  pub(crate) name: String,
  pub(crate) value_digest: Option<String>,
  pub(crate) secret_capability: bool,
}

/// Process evidence that is not a pre-execution input declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CompilationExecutionMetadata {
  pub(crate) compiler: Option<ExecutableIdentity>,
  pub(crate) wrappers: Vec<CompilerWrapperIdentity>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub(crate) cache_wrapper: Option<CompilerCacheWrapperMetadata>,
  pub(crate) platform_identity: String,
  pub(crate) environment_reads: BTreeSet<EnvironmentObservation>,
  pub(crate) success: bool,
  pub(crate) cargo_fresh: bool,
}

/// Stable position of one executable in Cargo's effective compiler-wrapper chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum CompilerWrapperRole {
  #[serde(rename = "cargo_rail_cache")]
  Cache,
  #[serde(rename = "cargo_global")]
  Global,
  #[serde(rename = "cargo_rail_diagnostic")]
  Diagnostic,
  #[serde(rename = "cargo_workspace")]
  Workspace,
}

/// Exact executable identity bound to one stable wrapper-chain position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CompilerWrapperIdentity {
  role: CompilerWrapperRole,
  executable: ExecutableIdentity,
}

impl CompilerWrapperIdentity {
  pub(crate) fn new(role: CompilerWrapperRole, executable: ExecutableIdentity) -> Self {
    Self { role, executable }
  }
}

/// Whether the cargo-rail native compiler cache ran for this observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompilerCacheWrapperStatus {
  Hit,
  Miss,
  Disabled,
  Bypassed,
}

/// Redaction-safe compiler-cache disposition attached to the exact wrapper chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CompilerCacheWrapperMetadata {
  version: u32,
  status: CompilerCacheWrapperStatus,
  reason: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  action_key: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  result_key: Option<String>,
  #[serde(default, skip_serializing_if = "is_zero")]
  bytes_hashed: u64,
  #[serde(default, skip_serializing_if = "is_zero")]
  bytes_restored: u64,
}

impl CompilerCacheWrapperMetadata {
  pub(crate) fn new(status: CompilerCacheWrapperStatus, reason: &str) -> Self {
    Self {
      version: COMPILER_CACHE_WRAPPER_METADATA_VERSION,
      status,
      reason: reason.to_string(),
      action_key: None,
      result_key: None,
      bytes_hashed: 0,
      bytes_restored: 0,
    }
  }

  pub(crate) fn native(
    status: CompilerCacheWrapperStatus,
    reason: impl Into<String>,
    action_key: Option<String>,
    result_key: Option<String>,
    bytes_hashed: u64,
    bytes_restored: u64,
  ) -> Self {
    Self {
      version: COMPILER_CACHE_WRAPPER_METADATA_VERSION,
      status,
      reason: reason.into(),
      action_key,
      result_key,
      bytes_hashed,
      bytes_restored,
    }
  }

  pub(crate) fn reason(&self) -> &str {
    &self.reason
  }

  pub(crate) fn action_key(&self) -> Option<&str> {
    self.action_key.as_deref()
  }

  pub(crate) fn result_key(&self) -> Option<&str> {
    self.result_key.as_deref()
  }

  pub(crate) fn bytes_hashed(&self) -> u64 {
    self.bytes_hashed
  }
}

const fn is_zero(value: &u64) -> bool {
  *value == 0
}

/// Immutable post-execution evidence for one compilation unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CompilationObservationManifest {
  pub(crate) version: u32,
  pub(crate) cargo_artifact_identity: Option<String>,
  pub(crate) unit: CompilationUnit,
  pub(crate) unit_identity: String,
  pub(crate) declared_inputs: Vec<FileObservation>,
  pub(crate) observed_reads: Vec<FileObservation>,
  pub(crate) dependency_artifacts: Vec<FileObservation>,
  pub(crate) emitted_outputs: Vec<FileObservation>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub(crate) executable_output: Option<FileObservation>,
  pub(crate) execution: CompilationExecutionMetadata,
  pub(crate) bypasses: BTreeSet<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub(crate) build_script_action_key: Option<crate::build_script::BuildScriptActionKeyAnalysis>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub(crate) build_script_result: Option<crate::build_script::BuildScriptResultAnalysis>,
}

impl CompilationObservationManifest {
  /// Re-digest every file that supplied result evidence.
  ///
  /// Executables are re-digested once by the enclosing compiler-cache identity;
  /// repeating that work for every unit would hash the same toolchain many times.
  pub(crate) fn revalidation_reason(&self, source_root: &Path) -> Option<&'static str> {
    if self.version != COMPILATION_OBSERVATION_VERSION
      || self
        .execution
        .cache_wrapper
        .as_ref()
        .is_none_or(|metadata| metadata.version != COMPILER_CACHE_WRAPPER_METADATA_VERSION)
    {
      return Some("compilation_observation_schema_changed");
    }
    for (files, reason) in [
      (&self.declared_inputs, "declared_compiler_input_changed"),
      (&self.observed_reads, "observed_compiler_read_changed"),
      (&self.dependency_artifacts, "dependency_artifact_changed"),
      (&self.emitted_outputs, "compiler_output_changed"),
    ] {
      for file in files {
        if !file.revalidate(source_root) {
          return Some(reason);
        }
      }
    }
    for environment in &self.execution.environment_reads {
      if environment.secret_capability {
        return Some("secret_compiler_environment");
      }
      if is_cargo_provided_environment(&environment.name) {
        continue;
      }
      let current = std::env::var_os(&environment.name);
      let current_digest = current
        .as_deref()
        .map(OsStr::as_encoded_bytes)
        .map(ContentDigest::sha256)
        .map(|digest| format!("sha256:{digest}"));
      if current_digest != environment.value_digest {
        return Some("compiler_environment_changed");
      }
    }
    None
  }

  pub(crate) fn has_bypass(&self, reason: &str) -> bool {
    self.bypasses.contains(reason)
  }
}

/// Pre-execution portion of one wrapper observation.
pub(crate) struct InvocationRecorder {
  directory: PathBuf,
  source_root: PathBuf,
  current_dir: PathBuf,
  raw: RawCompilerInvocation,
  dep_info_paths: Vec<PathBuf>,
  metadata_paths: Vec<PathBuf>,
  rlib_paths: Vec<PathBuf>,
  output_paths: Vec<PathBuf>,
}

pub(crate) struct NativeOutputPaths {
  pub(crate) dep_info: PathBuf,
  pub(crate) artifacts: Vec<NativeOutputArtifact>,
}

/// One typed compiler artifact destination from the original rustc invocation.
pub(crate) struct NativeOutputArtifact {
  pub(crate) role: NativeOutputRole,
  pub(crate) path: PathBuf,
}

/// Closed output-role vocabulary shared by action identity, CAS slots, and restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeOutputRole {
  Metadata,
  Rlib,
  Executable,
  ProcMacro,
  Dylib,
  Cdylib,
  Staticlib,
}

impl NativeOutputRole {
  pub(crate) const fn name(self) -> &'static str {
    match self {
      Self::Metadata => "metadata",
      Self::Rlib => "rlib",
      Self::Executable => "executable",
      Self::ProcMacro => "proc_macro",
      Self::Dylib => "dylib",
      Self::Cdylib => "cdylib",
      Self::Staticlib => "staticlib",
    }
  }
}

/// Wrapper evidence before Cargo compiler-artifact correlation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RawCompilerInvocation {
  pub(crate) version: u32,
  pub(crate) mode: CompilerMode,
  pub(crate) crate_name: Option<String>,
  pub(crate) crate_types: BTreeSet<String>,
  pub(crate) target_argument: Option<String>,
  pub(crate) cfg: BTreeSet<String>,
  pub(crate) emit_modes: BTreeSet<String>,
  pub(crate) test_mode: bool,
  pub(crate) compiler_arguments: Vec<String>,
  pub(crate) declared_inputs: Vec<FileObservation>,
  pub(crate) observed_reads: Vec<FileObservation>,
  pub(crate) dependency_artifacts: Vec<(String, FileObservation)>,
  pub(crate) emitted_outputs: Vec<FileObservation>,
  pub(crate) environment_reads: BTreeSet<EnvironmentObservation>,
  pub(crate) compiler: Option<ExecutableIdentity>,
  pub(crate) wrappers: Vec<CompilerWrapperIdentity>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub(crate) cache_wrapper: Option<CompilerCacheWrapperMetadata>,
  pub(crate) success: bool,
  pub(crate) bypasses: BTreeSet<String>,
}

/// Stable Cargo JSON fields correlated with one wrapper invocation.
pub(crate) struct CargoArtifactObservation {
  pub(crate) package: String,
  pub(crate) target_kinds: BTreeSet<String>,
  pub(crate) target_name: String,
  pub(crate) crate_types: BTreeSet<String>,
  pub(crate) source: ObservationPath,
  pub(crate) profile: CompilationProfile,
  pub(crate) features: BTreeSet<String>,
  pub(crate) outputs: Vec<FileObservation>,
  pub(crate) executable: Option<FileObservation>,
  pub(crate) fresh: bool,
  pub(crate) bypasses: BTreeSet<String>,
}

/// Snapshot-derived target facts required to interpret wrapper argv without rediscovery.
#[derive(Clone)]
pub(crate) struct CompilationObservationContext {
  pub(crate) source_root: PathBuf,
  host_target: String,
  targets: Vec<ObservedTargetIdentity>,
}

#[derive(Clone)]
struct ObservedTargetIdentity {
  selectors: BTreeSet<String>,
  platform: String,
  identity: String,
  cfg: BTreeSet<String>,
}

impl CompilationObservationContext {
  pub(crate) fn capture(snapshot: &WorkspaceSnapshot) -> RailResult<Self> {
    let targets = snapshot
      .targets()
      .iter()
      .map(|target| {
        let mut selectors = BTreeSet::new();
        let platform = match target.specification() {
          crate::cargo::resolution::TargetSpecificationIdentity::BuiltIn(name) => {
            selectors.insert(name.clone());
            name.clone()
          }
          crate::cargo::resolution::TargetSpecificationIdentity::Custom(specification) => {
            selectors.insert(specification.name().to_string());
            selectors.insert(crate::utils::path_to_git_format(specification.path()));
            specification.name().to_string()
          }
        };
        Ok(ObservedTargetIdentity {
          selectors,
          platform,
          identity: format!(
            "sha256:{}",
            ContentDigest::sha256(&target.portable_snapshot_identity(snapshot.source_root())?)
          ),
          cfg: target.cfg().iter().cloned().collect(),
        })
      })
      .collect::<RailResult<Vec<_>>>()?;
    Ok(Self {
      source_root: snapshot.source_root().to_path_buf(),
      host_target: snapshot.toolchain().host_target().to_string(),
      targets,
    })
  }
}

/// Correlate stable Cargo artifacts with wrapper evidence and derive typed unit identities.
pub(crate) fn build_manifests(
  raw_invocations: Vec<RawCompilerInvocation>,
  artifacts: Vec<CargoArtifactObservation>,
  context: &CompilationObservationContext,
  requested_target: &str,
  expected_mode: CompilerMode,
) -> RailResult<Vec<CompilationObservationManifest>> {
  let mut raw_invocations = raw_invocations.into_iter().map(Some).collect::<Vec<_>>();
  let mut manifests = Vec::with_capacity(artifacts.len());
  for artifact in artifacts {
    let raw_index = raw_invocations.iter().position(|raw| {
      raw
        .as_ref()
        .is_some_and(|raw| invocation_matches_artifact(raw, &artifact))
    });
    let raw = raw_index.and_then(|index| raw_invocations[index].take());
    manifests.push(manifest_for_artifact(
      artifact,
      raw,
      context,
      requested_target,
      expected_mode,
    )?);
  }
  for raw in raw_invocations.into_iter().flatten() {
    manifests.push(manifest_without_artifact(raw, context, requested_target)?);
  }

  let producer_by_output = manifests
    .iter()
    .enumerate()
    .flat_map(|(index, manifest)| {
      manifest
        .emitted_outputs
        .iter()
        .map(move |output| (output.path.clone(), index))
    })
    .collect::<BTreeMap<_, _>>();
  let base_identities = manifests
    .iter()
    .map(|manifest| manifest.unit.identity())
    .collect::<RailResult<Vec<_>>>()?;
  for manifest in &mut manifests {
    for (edge, artifact) in manifest
      .unit
      .dependencies
      .iter_mut()
      .zip(&manifest.dependency_artifacts)
    {
      if let Some(producer) = producer_by_output.get(&artifact.path) {
        edge.producer_unit = Some(base_identities[*producer].clone());
      } else {
        manifest.bypasses.insert("dependency_unit_unresolved".to_string());
      }
    }
    manifest.unit_identity = manifest.unit.identity()?;
  }
  manifests.sort_unstable_by(|left, right| left.unit_identity.cmp(&right.unit_identity));
  Ok(manifests)
}

pub(crate) fn attach_execution_identities(
  manifests: &mut [CompilationObservationManifest],
  compiler: &ExecutableIdentity,
  wrappers: &[CompilerWrapperIdentity],
  cache_wrapper: &CompilerCacheWrapperMetadata,
  executable_bypasses: &BTreeSet<String>,
) {
  for manifest in manifests {
    if manifest.execution.compiler.is_none() {
      manifest.execution.compiler = Some(compiler.clone());
    }
    if manifest.execution.wrappers.is_empty() {
      manifest.execution.wrappers = wrappers.to_vec();
    }
    if manifest.execution.cache_wrapper.is_none() {
      manifest.execution.cache_wrapper = Some(cache_wrapper.clone());
    }
    manifest.bypasses.extend(executable_bypasses.iter().cloned());
  }
}

/// Bind each verified build-script result into every transitive consumer unit.
///
/// `package_dependencies` is oriented from consumer package to dependency
/// package. The producing build-script compilation unit is excluded because a
/// post-execution result can never be an input to its own pre-execution action.
pub(crate) fn attach_build_script_result_dependencies(
  manifests: &mut [CompilationObservationManifest],
  package_dependencies: &HashMap<String, BTreeSet<String>>,
  bindings: &[BuildScriptResultBinding],
) -> RailResult<()> {
  if bindings.is_empty() {
    return Ok(());
  }

  let mut dependents = BTreeMap::<&str, BTreeSet<&str>>::new();
  for (consumer, dependencies) in package_dependencies {
    dependents.entry(consumer).or_default();
    for dependency in dependencies {
      dependents.entry(dependency).or_default().insert(consumer);
    }
  }

  for manifest in manifests.iter_mut() {
    manifest.unit.build_script_results.clear();
    manifest.bypasses.remove("build_script_result_unavailable");
    manifest.bypasses.remove("build_script_action_key_unavailable");
    manifest.bypasses.remove("build_script_dependency_graph_incomplete");
  }

  for binding in bindings {
    let mut affected = BTreeSet::from([binding.package.as_str()]);
    let mut pending = vec![binding.package.as_str()];
    if !dependents.contains_key(binding.package.as_str()) {
      for manifest in manifests.iter_mut() {
        if manifest.unit.package != binding.package || manifest.unit.target_kind != CompilationTargetKind::BuildScript {
          manifest
            .bypasses
            .insert("build_script_dependency_graph_incomplete".to_string());
        }
      }
      continue;
    }
    while let Some(package) = pending.pop() {
      if let Some(package_dependents) = dependents.get(package) {
        for dependent in package_dependents {
          if affected.insert(dependent) {
            pending.push(dependent);
          }
        }
      }
    }

    for manifest in manifests.iter_mut().filter(|manifest| {
      affected.contains(manifest.unit.package.as_str())
        && !(manifest.unit.package == binding.package
          && manifest.unit.target_kind == CompilationTargetKind::BuildScript)
    }) {
      match (&binding.action_key, &binding.result_digest) {
        (Some(action_key), Some(result_digest)) => {
          manifest.unit.build_script_results.insert(BuildScriptResultDependency {
            producer_action: action_key.clone(),
            result_digest: result_digest.clone(),
          });
        }
        (None, Some(_)) => {
          manifest
            .bypasses
            .insert("build_script_action_key_unavailable".to_string());
        }
        (_, None) => {
          manifest.bypasses.insert("build_script_result_unavailable".to_string());
        }
      }
    }
  }

  for manifest in manifests.iter_mut() {
    manifest.unit_identity = manifest.unit.identity()?;
  }
  manifests.sort_unstable_by(|left, right| left.unit_identity.cmp(&right.unit_identity));
  Ok(())
}

fn invocation_matches_artifact(raw: &RawCompilerInvocation, artifact: &CargoArtifactObservation) -> bool {
  let crate_name_matches = raw
    .crate_name
    .as_ref()
    .is_some_and(|name| name == &artifact.target_name.replace('-', "_"));
  if !crate_name_matches || raw.test_mode != artifact.profile.test {
    return false;
  }
  let raw_outputs = raw
    .emitted_outputs
    .iter()
    .map(|output| &output.path)
    .collect::<BTreeSet<_>>();
  artifact.outputs.iter().any(|output| raw_outputs.contains(&output.path))
    || raw.declared_inputs.iter().any(|input| input.path == artifact.source)
}

fn manifest_for_artifact(
  artifact: CargoArtifactObservation,
  raw: Option<RawCompilerInvocation>,
  context: &CompilationObservationContext,
  requested_target: &str,
  expected_mode: CompilerMode,
) -> RailResult<CompilationObservationManifest> {
  let cargo_artifact_identity = cargo_artifact_identity(&artifact)?;
  let mode = raw.as_ref().map_or(expected_mode, |raw| raw.mode);
  let target_kind = match mode {
    CompilerMode::Rustdoc if raw.as_ref().is_some_and(|raw| raw.test_mode) => CompilationTargetKind::Test,
    CompilerMode::Rustdoc => CompilationTargetKind::Documentation,
    CompilerMode::Rustc | CompilerMode::Unknown => classify_target_kind(&artifact.target_kinds),
  };
  let domain = compilation_domain(raw.as_ref(), &target_kind, context, requested_target)?;
  let mut bypasses = raw
    .as_ref()
    .map(|raw| raw.bypasses.clone())
    .unwrap_or_else(|| BTreeSet::from([invocation_unavailable_reason(mode).to_string()]));
  bypasses.extend(artifact.bypasses.iter().cloned());
  if let Some(reason) = domain.bypass {
    bypasses.insert(reason.to_string());
  }
  if matches!(target_kind, CompilationTargetKind::BuildScript) {
    bypasses.insert("build_script_execution_observations_unavailable".to_string());
  }
  if matches!(target_kind, CompilationTargetKind::ProcMacro) {
    bypasses.insert("proc_macro_filesystem_observations_unavailable".to_string());
  }
  let emit_modes = raw.as_ref().map_or_else(BTreeSet::new, |raw| raw.emit_modes.clone());
  let linker_responsibility = if mode == CompilerMode::Rustdoc {
    LinkerResponsibility::None
  } else {
    raw.as_ref().map_or(LinkerResponsibility::Unknown, |_| {
      if emit_modes.contains("link") {
        LinkerResponsibility::RustcDriver
      } else {
        LinkerResponsibility::None
      }
    })
  };
  if linker_responsibility == LinkerResponsibility::RustcDriver {
    bypasses.insert("native_link_sdk_inputs_unavailable".to_string());
  }
  let mut cfg = domain.cfg;
  if let Some(raw) = &raw {
    cfg.extend(raw.cfg.iter().cloned());
  }
  let dependency_artifacts = raw.as_ref().map_or_else(Vec::new, |raw| {
    raw
      .dependency_artifacts
      .iter()
      .map(|(_, artifact)| artifact.clone())
      .collect()
  });
  let dependencies = raw.as_ref().map_or_else(Vec::new, |raw| {
    raw
      .dependency_artifacts
      .iter()
      .map(|(name, artifact)| CompilationDependencyEdge {
        extern_name: name.clone(),
        artifact_digest: artifact.content_digest.clone(),
        producer_unit: None,
      })
      .collect()
  });
  let mut emitted_outputs = artifact.outputs;
  if let Some(raw) = &raw {
    emitted_outputs.extend(raw.emitted_outputs.iter().cloned());
  }
  sort_and_deduplicate_files(&mut emitted_outputs);
  let execution = CompilationExecutionMetadata {
    compiler: raw.as_ref().and_then(|raw| raw.compiler.clone()),
    wrappers: raw.as_ref().map_or_else(Vec::new, |raw| raw.wrappers.clone()),
    cache_wrapper: raw.as_ref().and_then(|raw| raw.cache_wrapper.clone()),
    platform_identity: platform_identity(),
    environment_reads: raw
      .as_ref()
      .map_or_else(BTreeSet::new, |raw| raw.environment_reads.clone()),
    success: raw.as_ref().is_some_and(|raw| raw.success),
    cargo_fresh: artifact.fresh,
  };
  let unit = CompilationUnit {
    version: COMPILATION_UNIT_VERSION,
    package: artifact.package,
    target_kind,
    cargo_target_kinds: artifact.target_kinds,
    target_name: artifact.target_name,
    crate_types: artifact.crate_types,
    role: domain.role,
    mode,
    platform: domain.platform,
    target_specification: domain.target_specification,
    profile: artifact.profile,
    features: artifact.features,
    cfg,
    emit_modes,
    linker_responsibility,
    compiler_arguments: raw.as_ref().map_or_else(Vec::new, |raw| raw.compiler_arguments.clone()),
    dependencies,
    build_script_results: BTreeSet::new(),
  };
  let unit_identity = unit.identity()?;
  Ok(CompilationObservationManifest {
    version: COMPILATION_OBSERVATION_VERSION,
    cargo_artifact_identity: Some(cargo_artifact_identity),
    unit,
    unit_identity,
    declared_inputs: raw.as_ref().map_or_else(Vec::new, |raw| raw.declared_inputs.clone()),
    observed_reads: raw.as_ref().map_or_else(Vec::new, |raw| raw.observed_reads.clone()),
    dependency_artifacts,
    emitted_outputs,
    executable_output: artifact.executable,
    execution,
    bypasses,
    build_script_action_key: None,
    build_script_result: None,
  })
}

fn manifest_without_artifact(
  raw: RawCompilerInvocation,
  context: &CompilationObservationContext,
  requested_target: &str,
) -> RailResult<CompilationObservationManifest> {
  let target_kind = match raw.mode {
    CompilerMode::Rustdoc if raw.test_mode => CompilationTargetKind::Test,
    CompilerMode::Rustdoc => CompilationTargetKind::Documentation,
    CompilerMode::Rustc | CompilerMode::Unknown => raw_target_kind(&raw.crate_types, raw.test_mode),
  };
  let domain = compilation_domain(Some(&raw), &target_kind, context, requested_target)?;
  let mut bypasses = raw.bypasses.clone();
  bypasses.insert("cargo_artifact_unavailable".to_string());
  if let Some(reason) = domain.bypass {
    bypasses.insert(reason.to_string());
  }
  let dependency_artifacts = raw
    .dependency_artifacts
    .iter()
    .map(|(_, artifact)| artifact.clone())
    .collect::<Vec<_>>();
  let mut cfg = domain.cfg;
  cfg.extend(raw.cfg.iter().cloned());
  let linker_responsibility = if raw.emit_modes.contains("link") {
    LinkerResponsibility::RustcDriver
  } else {
    LinkerResponsibility::None
  };
  let unit = CompilationUnit {
    version: COMPILATION_UNIT_VERSION,
    package: "unknown".to_string(),
    target_kind,
    cargo_target_kinds: BTreeSet::new(),
    target_name: raw.crate_name.clone().unwrap_or_else(|| "unknown".to_string()),
    crate_types: raw.crate_types.clone(),
    role: domain.role,
    mode: raw.mode,
    platform: domain.platform,
    target_specification: domain.target_specification,
    profile: CompilationProfile {
      opt_level: "unknown".to_string(),
      debuginfo: "unknown".to_string(),
      debug_assertions: false,
      overflow_checks: false,
      test: raw.test_mode,
    },
    features: BTreeSet::new(),
    cfg,
    emit_modes: raw.emit_modes.clone(),
    linker_responsibility,
    compiler_arguments: raw.compiler_arguments.clone(),
    dependencies: raw
      .dependency_artifacts
      .iter()
      .map(|(name, artifact)| CompilationDependencyEdge {
        extern_name: name.clone(),
        artifact_digest: artifact.content_digest.clone(),
        producer_unit: None,
      })
      .collect(),
    build_script_results: BTreeSet::new(),
  };
  let unit_identity = unit.identity()?;
  Ok(CompilationObservationManifest {
    version: COMPILATION_OBSERVATION_VERSION,
    cargo_artifact_identity: None,
    unit,
    unit_identity,
    declared_inputs: raw.declared_inputs,
    observed_reads: raw.observed_reads,
    dependency_artifacts,
    emitted_outputs: raw.emitted_outputs,
    executable_output: None,
    execution: CompilationExecutionMetadata {
      compiler: raw.compiler,
      wrappers: raw.wrappers,
      cache_wrapper: raw.cache_wrapper,
      platform_identity: platform_identity(),
      environment_reads: raw.environment_reads,
      success: raw.success,
      cargo_fresh: false,
    },
    bypasses,
    build_script_action_key: None,
    build_script_result: None,
  })
}

fn invocation_unavailable_reason(mode: CompilerMode) -> &'static str {
  match mode {
    CompilerMode::Rustc => "rustc_invocation_unavailable",
    CompilerMode::Rustdoc => "rustdoc_invocation_unavailable",
    CompilerMode::Unknown => "compiler_invocation_unavailable",
  }
}

fn cargo_artifact_identity(artifact: &CargoArtifactObservation) -> RailResult<String> {
  let bytes = serde_json::to_vec(&(
    &artifact.package,
    &artifact.target_kinds,
    &artifact.target_name,
    &artifact.crate_types,
    &artifact.source,
    &artifact.profile,
    &artifact.features,
    &artifact.outputs,
    &artifact.executable,
  ))?;
  Ok(format!("sha256:{}", ContentDigest::sha256(&bytes)))
}

struct CompilationDomain {
  role: CompilationRole,
  platform: String,
  target_specification: String,
  cfg: BTreeSet<String>,
  bypass: Option<&'static str>,
}

fn compilation_domain(
  raw: Option<&RawCompilerInvocation>,
  kind: &CompilationTargetKind,
  context: &CompilationObservationContext,
  requested_target: &str,
) -> RailResult<CompilationDomain> {
  let role = if matches!(
    kind,
    CompilationTargetKind::BuildScript | CompilationTargetKind::ProcMacro
  ) {
    CompilationRole::Host
  } else {
    CompilationRole::Target
  };
  let role_bypass = raw.is_none().then_some("compiler_invocation_role_evidence_unavailable");
  let target_argument = raw
    .and_then(|raw| raw.target_argument.as_deref())
    .or_else(|| (role == CompilationRole::Target && requested_target != "default").then_some(requested_target));
  let requested_platform = target_argument.unwrap_or(&context.host_target);
  let target = matching_target(context, target_argument).ok_or_else(|| {
    RailError::message(format!(
      "compilation target identity is unavailable for '{requested_platform}'"
    ))
  })?;
  Ok(CompilationDomain {
    role,
    platform: target.platform.clone(),
    target_specification: target.identity.clone(),
    cfg: target.cfg.clone(),
    bypass: role_bypass,
  })
}

fn matching_target<'a>(
  context: &'a CompilationObservationContext,
  target_argument: Option<&str>,
) -> Option<&'a ObservedTargetIdentity> {
  let requested = target_argument.unwrap_or(&context.host_target);
  context
    .targets
    .iter()
    .find(|target| target.selectors.contains(requested))
}

fn classify_target_kind(kinds: &BTreeSet<String>) -> CompilationTargetKind {
  if kinds.contains("custom-build") {
    CompilationTargetKind::BuildScript
  } else if kinds.contains("proc-macro") {
    CompilationTargetKind::ProcMacro
  } else if kinds.contains("test") {
    CompilationTargetKind::Test
  } else if kinds.contains("example") {
    CompilationTargetKind::Example
  } else if kinds.contains("bench") {
    CompilationTargetKind::Benchmark
  } else if kinds.contains("bin") {
    CompilationTargetKind::Binary
  } else if kinds
    .iter()
    .any(|kind| matches!(kind.as_str(), "lib" | "rlib" | "dylib" | "cdylib" | "staticlib"))
  {
    CompilationTargetKind::Library
  } else {
    CompilationTargetKind::Other(kinds.iter().cloned().collect::<Vec<_>>().join(","))
  }
}

fn raw_target_kind(crate_types: &BTreeSet<String>, test_mode: bool) -> CompilationTargetKind {
  if crate_types.contains("proc-macro") {
    CompilationTargetKind::ProcMacro
  } else if test_mode {
    CompilationTargetKind::Test
  } else if crate_types.contains("bin") {
    CompilationTargetKind::Binary
  } else if !crate_types.is_empty() {
    CompilationTargetKind::Library
  } else {
    CompilationTargetKind::Other("unknown".to_string())
  }
}

fn platform_identity() -> String {
  format!(
    "{}-{}-{}",
    std::env::consts::FAMILY,
    std::env::consts::OS,
    std::env::consts::ARCH
  )
}

/// Capture argv-declared inputs before invoking the compiler.
pub(crate) fn begin_invocation(
  directory: &Path,
  source_root: &Path,
  _rustc: &OsStr,
  arguments: &[OsString],
) -> RailResult<InvocationRecorder> {
  let current_dir = std::env::current_dir()
    .map_err(|error| RailError::message(format!("failed to capture compiler working directory: {error}")))?;
  begin_compiler_invocation(directory, source_root, &current_dir, arguments, CompilerMode::Rustc)
}

/// Capture argv-declared inputs using the exact working directory rustc will receive.
pub(crate) fn begin_invocation_in(
  directory: &Path,
  source_root: &Path,
  current_dir: &Path,
  _rustc: &OsStr,
  arguments: &[OsString],
) -> RailResult<InvocationRecorder> {
  begin_compiler_invocation(directory, source_root, current_dir, arguments, CompilerMode::Rustc)
}

/// Capture one rustdoc invocation through the transparent observation proxy.
pub(crate) fn begin_rustdoc_invocation(
  directory: &Path,
  source_root: &Path,
  arguments: &[OsString],
) -> RailResult<InvocationRecorder> {
  let current_dir = std::env::current_dir()
    .map_err(|error| RailError::message(format!("failed to capture compiler working directory: {error}")))?;
  begin_compiler_invocation(directory, source_root, &current_dir, arguments, CompilerMode::Rustdoc)
}

fn begin_compiler_invocation(
  directory: &Path,
  source_root: &Path,
  physical_current_dir: &Path,
  arguments: &[OsString],
  mode: CompilerMode,
) -> RailResult<InvocationRecorder> {
  let canonical_source_root = crate::utils::canonicalize_existing(source_root).map_err(|error| {
    RailError::message(format!(
      "failed to resolve compiler observation source root '{}': {error}",
      source_root.display()
    ))
  })?;
  let canonical_current_dir = crate::utils::canonicalize_existing(physical_current_dir).map_err(|error| {
    RailError::message(format!(
      "failed to resolve compiler working directory '{}': {error}",
      physical_current_dir.display()
    ))
  })?;
  let current_dir = canonical_current_dir
    .strip_prefix(&canonical_source_root)
    .map(|relative| source_root.join(relative))
    .unwrap_or(canonical_current_dir);
  let mut bypasses = BTreeSet::new();
  let argument_text = arguments
    .iter()
    .map(|argument| {
      argument.to_str().map(str::to_string).ok_or_else(|| {
        bypasses.insert("non_utf8_compiler_argument".to_string());
        RailError::message("compiler argument is not valid UTF-8")
      })
    })
    .collect::<RailResult<Vec<_>>>()?;
  let parsed = ParsedArguments::parse(&argument_text, &current_dir, mode, &mut bypasses);
  if mode == CompilerMode::Rustdoc {
    if !parsed.emit_modes.contains("dep-info") {
      bypasses.insert("rustdoc_dep_info_unavailable".to_string());
    }
    if parsed.emit_modes.is_empty() || parsed.emit_modes.iter().any(|emit| emit != "dep-info") {
      bypasses.insert("rustdoc_output_tree_unavailable".to_string());
    }
  }
  let mut declared_inputs = Vec::new();
  for path in &parsed.declared_input_paths {
    capture_file(
      path,
      &current_dir,
      source_root,
      &mut declared_inputs,
      &mut bypasses,
      "declared_input",
    );
  }
  let dependency_artifacts = parsed
    .dependency_paths
    .into_iter()
    .filter_map(|(name, path)| {
      capture_one_file(&path, &current_dir, source_root, &mut bypasses, "dependency_artifact").map(|file| (name, file))
    })
    .collect();

  Ok(InvocationRecorder {
    directory: directory.to_path_buf(),
    source_root: source_root.to_path_buf(),
    current_dir,
    dep_info_paths: parsed.dep_info_paths,
    metadata_paths: parsed.metadata_paths,
    rlib_paths: parsed.rlib_paths,
    output_paths: parsed.output_paths,
    raw: RawCompilerInvocation {
      version: RAW_INVOCATION_VERSION,
      mode,
      crate_name: parsed.crate_name,
      crate_types: parsed.crate_types,
      target_argument: parsed.target_argument,
      cfg: parsed.cfg,
      emit_modes: parsed.emit_modes,
      test_mode: parsed.test_mode,
      compiler_arguments: portable_compiler_arguments(&argument_text, source_root, &canonical_source_root),
      declared_inputs,
      observed_reads: Vec::new(),
      dependency_artifacts,
      emitted_outputs: Vec::new(),
      environment_reads: BTreeSet::new(),
      compiler: None,
      wrappers: Vec::new(),
      cache_wrapper: crate::compiler::native_cache::metadata_from_environment(),
      success: false,
      bypasses,
    },
  })
}

impl InvocationRecorder {
  pub(crate) fn observation(&self) -> &RawCompilerInvocation {
    &self.raw
  }

  pub(crate) fn native_output_paths(&self) -> Option<NativeOutputPaths> {
    let [dep_info] = self.dep_info_paths.as_slice() else {
      return None;
    };
    if self.metadata_paths.len() > 1 || self.rlib_paths.len() > 1 {
      return None;
    }
    let mut artifacts = Vec::new();
    if let [metadata] = self.metadata_paths.as_slice() {
      artifacts.push(NativeOutputArtifact {
        role: NativeOutputRole::Metadata,
        path: metadata.clone(),
      });
    }
    if let [rlib] = self.rlib_paths.as_slice() {
      artifacts.push(NativeOutputArtifact {
        role: NativeOutputRole::Rlib,
        path: rlib.clone(),
      });
    }
    let known = self
      .dep_info_paths
      .iter()
      .chain(&self.metadata_paths)
      .chain(&self.rlib_paths)
      .collect::<BTreeSet<_>>();
    let linked = self
      .output_paths
      .iter()
      .filter(|path| !known.contains(path))
      .collect::<Vec<_>>();
    if !linked.is_empty() {
      let [linked] = linked.as_slice() else {
        return None;
      };
      let role = if self.raw.crate_types == BTreeSet::from(["bin".to_string()]) {
        NativeOutputRole::Executable
      } else if self.raw.crate_types == BTreeSet::from(["proc-macro".to_string()]) {
        NativeOutputRole::ProcMacro
      } else if self.raw.crate_types == BTreeSet::from(["dylib".to_string()]) {
        NativeOutputRole::Dylib
      } else if self.raw.crate_types == BTreeSet::from(["cdylib".to_string()]) {
        NativeOutputRole::Cdylib
      } else if self.raw.crate_types == BTreeSet::from(["staticlib".to_string()]) {
        NativeOutputRole::Staticlib
      } else {
        return None;
      };
      artifacts.push(NativeOutputArtifact {
        role,
        path: (*linked).clone(),
      });
    }
    if artifacts.is_empty() {
      return None;
    }
    Some(NativeOutputPaths {
      dep_info: dep_info.clone(),
      artifacts,
    })
  }

  pub(crate) fn set_cache_wrapper(&mut self, metadata: CompilerCacheWrapperMetadata) {
    self.raw.cache_wrapper = Some(metadata);
  }

  /// Capture dep-info and emitted bytes after the compiler exits, then atomically publish raw evidence.
  pub(crate) fn finish(self, success: bool) -> RailResult<()> {
    let directory = self.directory.clone();
    let raw = self.complete(success)?;
    publish_raw(&directory, &raw)
  }

  pub(crate) fn complete(mut self, success: bool) -> RailResult<RawCompilerInvocation> {
    self.raw.success = success;
    for dep_info in &self.dep_info_paths {
      match parse_dep_info(dep_info, &self.current_dir, &self.source_root) {
        Ok((reads, environment)) => {
          self.raw.observed_reads.extend(reads);
          self.raw.environment_reads.extend(environment);
        }
        Err(_) => {
          self.raw.bypasses.insert("dep_info_unavailable".to_string());
        }
      }
      capture_file(
        dep_info,
        &self.current_dir,
        &self.source_root,
        &mut self.raw.emitted_outputs,
        &mut self.raw.bypasses,
        "dep_info_output",
      );
    }
    for output in &self.output_paths {
      capture_file(
        output,
        &self.current_dir,
        &self.source_root,
        &mut self.raw.emitted_outputs,
        &mut self.raw.bypasses,
        "emitted_output",
      );
    }
    sort_and_deduplicate_files(&mut self.raw.observed_reads);
    sort_and_deduplicate_files(&mut self.raw.emitted_outputs);
    Ok(self.raw)
  }
}

pub(crate) fn publish_raw(directory: &Path, raw: &RawCompilerInvocation) -> RailResult<()> {
  publish_prepared_raw(prepare_raw_publication(directory, raw)?)
}

/// One encoded content-addressed observation prepared exactly once for a restore transaction.
pub(crate) struct PreparedRawPublication {
  directory: PathBuf,
  destination: PathBuf,
  encoded: Vec<u8>,
  content_digest: String,
}

impl PreparedRawPublication {
  pub(crate) fn destination(&self) -> &Path {
    &self.destination
  }

  pub(crate) fn encoded(&self) -> &[u8] {
    &self.encoded
  }

  pub(crate) fn content_digest(&self) -> &str {
    &self.content_digest
  }
}

/// Encode and bind one restored observation before transaction authorization.
pub(crate) fn prepare_raw_publication(
  directory: &Path,
  raw: &RawCompilerInvocation,
) -> RailResult<PreparedRawPublication> {
  if raw.version != RAW_INVOCATION_VERSION {
    return Err(RailError::message(
      "refusing to publish a compiler observation with an incompatible schema",
    ));
  }
  fs::create_dir_all(directory)?;
  let compiler = match raw.mode {
    CompilerMode::Rustc => "rustc",
    CompilerMode::Rustdoc => "rustdoc",
    CompilerMode::Unknown => "compiler",
  };
  let encoded = serde_json::to_vec(raw)?;
  let identity = ContentDigest::sha256(&encoded);
  Ok(PreparedRawPublication {
    directory: directory.to_path_buf(),
    destination: raw_publication_path(directory, compiler, identity),
    encoded,
    content_digest: format!("sha256:{identity}"),
  })
}

/// Publish one prepared, regenerable compiler observation.
pub(crate) fn publish_prepared_raw(publication: PreparedRawPublication) -> RailResult<()> {
  let PreparedRawPublication {
    directory,
    destination,
    encoded,
    content_digest: _,
  } = publication;
  // The parent owns this private temporary directory and reads it only after
  // Cargo has joined every wrapper. Publish immutable content-addressed bytes,
  // but do not pay durable mutation fsyncs for regenerable evidence.
  fs::create_dir_all(&directory)?;
  let mut temporary = tempfile::Builder::new()
    .prefix(".cargo-rail-observation-")
    .suffix(".tmp")
    .tempfile_in(&directory)?;
  temporary.write_all(&encoded)?;
  match temporary.persist_noclobber(&destination) {
    Ok(_) => Ok(()),
    Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
      let existing = fs::read(&destination)?;
      if existing == encoded {
        Ok(())
      } else {
        Err(RailError::message(format!(
          "compiler observation content identity collided at '{}'",
          destination.display()
        )))
      }
    }
    Err(error) => Err(RailError::message(format!(
      "failed to publish compiler observation '{}': {}",
      destination.display(),
      error.error
    ))),
  }
}

fn raw_publication_path(directory: &Path, compiler: &str, identity: ContentDigest) -> PathBuf {
  directory.join(format!("{compiler}-sha256-{identity}.json"))
}

/// Load all complete wrapper records from one private invocation directory.
pub(crate) fn load_raw(directory: &Path) -> RailResult<Vec<RawCompilerInvocation>> {
  let mut paths = fs::read_dir(directory)
    .map_err(|error| RailError::message(format!("failed to read compiler observation directory: {error}")))?
    .filter_map(Result::ok)
    .map(|entry| entry.path())
    .filter(|path| path.extension().is_some_and(|extension| extension == "json"))
    .collect::<Vec<_>>();
  paths.sort();
  paths
    .into_iter()
    .map(|path| {
      let bytes = fs::read(&path)?;
      let raw: RawCompilerInvocation = serde_json::from_slice(&bytes)?;
      if raw.version != RAW_INVOCATION_VERSION {
        return Err(RailError::message(format!(
          "compiler observation '{}' has unsupported version {}",
          path.display(),
          raw.version
        )));
      }
      Ok(raw)
    })
    .collect()
}

#[derive(Default)]
struct ParsedArguments {
  crate_name: Option<String>,
  crate_types: BTreeSet<String>,
  target_argument: Option<String>,
  cfg: BTreeSet<String>,
  emit_modes: BTreeSet<String>,
  test_mode: bool,
  declared_input_paths: Vec<PathBuf>,
  dependency_paths: Vec<(String, PathBuf)>,
  dep_info_paths: Vec<PathBuf>,
  metadata_paths: Vec<PathBuf>,
  rlib_paths: Vec<PathBuf>,
  explicit_link_paths: Vec<PathBuf>,
  output_paths: Vec<PathBuf>,
  out_dir: Option<PathBuf>,
  extra_filename: String,
}

impl ParsedArguments {
  fn parse(
    arguments: &[String],
    current_dir: &Path,
    compiler_mode: CompilerMode,
    bypasses: &mut BTreeSet<String>,
  ) -> Self {
    let mut parsed = Self::default();
    let mut index = 0usize;
    while index < arguments.len() {
      let argument = &arguments[index];
      let next = || arguments.get(index + 1).map(String::as_str);
      match argument.as_str() {
        "--crate-name" => parsed.crate_name = next().map(str::to_string),
        "--crate-type" => {
          if let Some(value) = next() {
            parsed.crate_types.extend(value.split(',').map(str::to_string));
          }
        }
        "--target" => parsed.target_argument = next().map(str::to_string),
        "--cfg" => {
          if let Some(value) = next() {
            parsed.cfg.insert(value.to_string());
          }
        }
        "--emit" => {
          if let Some(value) = next() {
            parsed.capture_emit(value, current_dir, compiler_mode, bypasses);
          }
        }
        "--extern" => {
          if let Some(value) = next() {
            parsed.capture_extern(value, current_dir, bypasses);
          }
        }
        "-o" => {
          if let Some(value) = next() {
            let path = resolve_argument_path(value, current_dir);
            if compiler_mode == CompilerMode::Rustdoc {
              parsed.out_dir = Some(path);
            } else {
              parsed.output_paths.push(path);
            }
          }
        }
        "--out-dir" | "--output" => {
          parsed.out_dir = next().map(|value| resolve_argument_path(value, current_dir));
        }
        option if compiler_mode == CompilerMode::Rustdoc && is_rustdoc_declared_input_option(option) => {
          if let Some(value) = next() {
            parsed
              .declared_input_paths
              .push(resolve_argument_path(value, current_dir));
          }
        }
        option if compiler_mode == CompilerMode::Rustdoc && is_rustdoc_executable_option(option) => {
          if let Some(value) = next() {
            parsed
              .declared_input_paths
              .push(resolve_argument_path(value, current_dir));
          }
          bypasses.insert("rustdoc_external_tool_identity_unavailable".to_string());
        }
        "-C" => {
          if let Some(value) = next() {
            parsed.capture_codegen_option(value);
          }
        }
        "--test" => parsed.test_mode = true,
        _ if argument.starts_with("--crate-name=") => {
          parsed.crate_name = argument.split_once('=').map(|(_, value)| value.to_string());
        }
        _ if argument.starts_with("--crate-type=") => {
          parsed.crate_types.extend(
            argument
              .trim_start_matches("--crate-type=")
              .split(',')
              .map(str::to_string),
          );
        }
        _ if argument.starts_with("--target=") => {
          parsed.target_argument = Some(argument.trim_start_matches("--target=").to_string());
        }
        _ if argument.starts_with("--cfg=") => {
          parsed.cfg.insert(argument.trim_start_matches("--cfg=").to_string());
        }
        _ if argument.starts_with("--emit=") => {
          parsed.capture_emit(
            argument.trim_start_matches("--emit="),
            current_dir,
            compiler_mode,
            bypasses,
          );
        }
        _ if argument.starts_with("--extern=") => {
          parsed.capture_extern(argument.trim_start_matches("--extern="), current_dir, bypasses);
        }
        _ if argument.starts_with("--out-dir=") => {
          parsed.out_dir = Some(resolve_argument_path(
            argument.trim_start_matches("--out-dir="),
            current_dir,
          ));
        }
        _ if argument.starts_with("--output=") => {
          parsed.out_dir = Some(resolve_argument_path(
            argument.trim_start_matches("--output="),
            current_dir,
          ));
        }
        _ if compiler_mode == CompilerMode::Rustdoc
          && rustdoc_option_value(argument, is_rustdoc_declared_input_option).is_some() =>
        {
          if let Some(value) = rustdoc_option_value(argument, is_rustdoc_declared_input_option) {
            parsed
              .declared_input_paths
              .push(resolve_argument_path(value, current_dir));
          }
        }
        _ if compiler_mode == CompilerMode::Rustdoc
          && rustdoc_option_value(argument, is_rustdoc_executable_option).is_some() =>
        {
          if let Some(value) = rustdoc_option_value(argument, is_rustdoc_executable_option) {
            parsed
              .declared_input_paths
              .push(resolve_argument_path(value, current_dir));
          }
          bypasses.insert("rustdoc_external_tool_identity_unavailable".to_string());
        }
        _ if argument.starts_with("-C") => parsed.capture_codegen_option(argument.trim_start_matches("-C")),
        _ if argument.starts_with('@') => {
          parsed
            .declared_input_paths
            .push(resolve_argument_path(argument.trim_start_matches('@'), current_dir));
          bypasses.insert("response_file_expansion_unavailable".to_string());
        }
        _ if !argument.starts_with('-') && argument.ends_with(".rs") => {
          parsed
            .declared_input_paths
            .push(resolve_argument_path(argument, current_dir));
        }
        _ => {}
      }
      index += usize::from(option_consumes_next(argument)) + 1;
    }
    if parsed.emit_modes.contains("dep-info") && parsed.dep_info_paths.is_empty() {
      if let (Some(out_dir), Some(crate_name)) = (&parsed.out_dir, &parsed.crate_name) {
        let path = out_dir.join(format!("{crate_name}{}.d", parsed.extra_filename));
        parsed.dep_info_paths.push(path.clone());
        parsed.output_paths.push(path);
      } else {
        bypasses.insert("dep_info_path_unavailable".to_string());
      }
    }
    if parsed.emit_modes.contains("metadata")
      && parsed.metadata_paths.is_empty()
      && matches!(
        parsed.crate_types.iter().next().map(String::as_str),
        Some("lib" | "proc-macro")
      )
      && parsed.crate_types.len() == 1
      && let (Some(out_dir), Some(crate_name)) = (&parsed.out_dir, &parsed.crate_name)
    {
      let path = out_dir.join(format!("lib{crate_name}{}.rmeta", parsed.extra_filename));
      parsed.metadata_paths.push(path.clone());
      parsed.output_paths.push(path);
    }
    if parsed.crate_types == BTreeSet::from(["lib".to_string()]) {
      parsed.rlib_paths.append(&mut parsed.explicit_link_paths);
    }
    if parsed.emit_modes.contains("link")
      && parsed.rlib_paths.is_empty()
      && parsed.crate_types == BTreeSet::from(["lib".to_string()])
      && let (Some(out_dir), Some(crate_name)) = (&parsed.out_dir, &parsed.crate_name)
    {
      let path = out_dir.join(format!("lib{crate_name}{}.rlib", parsed.extra_filename));
      parsed.rlib_paths.push(path.clone());
      parsed.output_paths.push(path);
    }
    if parsed.emit_modes.contains("link")
      && parsed.explicit_link_paths.is_empty()
      && let (Some(out_dir), Some(crate_name)) = (&parsed.out_dir, &parsed.crate_name)
      && let Some(file_name) = implicit_native_link_output(&parsed.crate_types, crate_name, &parsed.extra_filename)
    {
      let known_outputs = parsed
        .dep_info_paths
        .iter()
        .chain(&parsed.metadata_paths)
        .chain(&parsed.rlib_paths)
        .collect::<BTreeSet<_>>();
      if !parsed.output_paths.iter().any(|path| !known_outputs.contains(path)) {
        parsed.output_paths.push(out_dir.join(file_name));
      }
    }
    parsed
  }

  fn capture_emit(
    &mut self,
    value: &str,
    current_dir: &Path,
    compiler_mode: CompilerMode,
    bypasses: &mut BTreeSet<String>,
  ) {
    for emit in value.split(',') {
      let (mode, path) = emit
        .split_once('=')
        .map_or((emit, None), |(mode, path)| (mode, Some(path)));
      self.emit_modes.insert(mode.to_string());
      if let Some(path) = path {
        let path = resolve_argument_path(path, current_dir);
        if mode == "dep-info" {
          self.dep_info_paths.push(path.clone());
        } else if mode == "metadata" {
          self.metadata_paths.push(path.clone());
        } else if mode == "link" {
          self.explicit_link_paths.push(path.clone());
        }
        self.output_paths.push(path);
      } else if compiler_mode != CompilerMode::Rustdoc && !matches!(mode, "dep-info" | "link" | "metadata") {
        bypasses.insert(format!("{mode}_output_path_unavailable"));
      }
    }
  }

  fn capture_extern(&mut self, value: &str, current_dir: &Path, bypasses: &mut BTreeSet<String>) {
    let Some((name, path)) = value.split_once('=') else {
      if value != "proc_macro" {
        bypasses.insert("dependency_artifact_path_unavailable".to_string());
      }
      return;
    };
    self
      .dependency_paths
      .push((name.to_string(), resolve_argument_path(path, current_dir)));
  }

  fn capture_codegen_option(&mut self, value: &str) {
    if let Some(extra_filename) = value.strip_prefix("extra-filename=") {
      self.extra_filename = extra_filename.to_string();
    }
  }
}

fn implicit_native_link_output(
  crate_types: &BTreeSet<String>,
  crate_name: &str,
  extra_filename: &str,
) -> Option<String> {
  if !cfg!(any(target_os = "macos", target_os = "linux")) || crate_types.len() != 1 {
    return None;
  }
  match crate_types.iter().next().map(String::as_str) {
    Some("bin") => Some(format!("{crate_name}{extra_filename}")),
    #[cfg(target_os = "macos")]
    Some("proc-macro" | "dylib" | "cdylib") => Some(format!("lib{crate_name}{extra_filename}.dylib")),
    #[cfg(target_os = "linux")]
    Some("proc-macro" | "dylib" | "cdylib") => Some(format!("lib{crate_name}{extra_filename}.so")),
    Some("staticlib") => Some(format!("lib{crate_name}{extra_filename}.a")),
    _ => None,
  }
}

fn option_consumes_next(argument: &str) -> bool {
  matches!(
    argument,
    "--crate-name"
      | "--crate-type"
      | "--target"
      | "--cfg"
      | "--emit"
      | "--extern"
      | "--out-dir"
      | "--output"
      | "-C"
      | "-o"
  ) || is_rustdoc_declared_input_option(argument)
    || is_rustdoc_executable_option(argument)
}

fn is_rustdoc_declared_input_option(argument: &str) -> bool {
  matches!(
    argument,
    "--markdown-css"
      | "--html-in-header"
      | "--html-before-content"
      | "--html-after-content"
      | "--markdown-before-content"
      | "--markdown-after-content"
      | "--extend-css"
      | "-e"
      | "--theme"
      | "--check-theme"
      | "--index-page"
  )
}

fn is_rustdoc_executable_option(argument: &str) -> bool {
  matches!(argument, "--test-builder" | "--test-builder-wrapper" | "--test-runtool")
}

fn rustdoc_option_value(argument: &str, predicate: fn(&str) -> bool) -> Option<&str> {
  let (option, value) = argument.split_once('=')?;
  predicate(option).then_some(value)
}

fn resolve_argument_path(path: &str, current_dir: &Path) -> PathBuf {
  let path = Path::new(path);
  if path.is_absolute() {
    path.to_path_buf()
  } else {
    current_dir.join(path)
  }
}

fn capture_file(
  path: &Path,
  current_dir: &Path,
  source_root: &Path,
  output: &mut Vec<FileObservation>,
  bypasses: &mut BTreeSet<String>,
  role: &str,
) {
  if let Some(file) = capture_one_file(path, current_dir, source_root, bypasses, role) {
    output.push(file);
  }
}

fn capture_one_file(
  path: &Path,
  current_dir: &Path,
  source_root: &Path,
  bypasses: &mut BTreeSet<String>,
  role: &str,
) -> Option<FileObservation> {
  match FileObservation::capture(path, current_dir, source_root) {
    Ok(file) => {
      if file.symlink_target.is_some() {
        bypasses.insert(format!("{role}_symlink_unavailable"));
      }
      Some(file)
    }
    Err(_) => {
      bypasses.insert(format!("{role}_bytes_unavailable"));
      None
    }
  }
}

fn parse_dep_info(
  path: &Path,
  current_dir: &Path,
  source_root: &Path,
) -> RailResult<(Vec<FileObservation>, BTreeSet<EnvironmentObservation>)> {
  let (logical, _, dependencies) = makefile_dependency_rule(path, current_dir)?;
  let mut reads = Vec::new();
  for dependency in dependencies {
    if let Ok(file) = FileObservation::capture(&dependency, current_dir, source_root) {
      reads.push(file);
    } else {
      return Err(RailError::message(format!(
        "dep-info input '{}' cannot be re-digested",
        dependency.display()
      )));
    }
  }
  sort_and_deduplicate_files(&mut reads);
  let mut environment = BTreeSet::new();
  for line in logical.lines().filter_map(|line| line.strip_prefix("# env-dep:")) {
    let (name, value) = line
      .split_once('=')
      .map_or((line, None), |(name, value)| (name, Some(value)));
    if name.is_empty() {
      return Err(RailError::message("dep-info contains an empty environment dependency"));
    }
    let secret = is_secret_name(name);
    let value_digest = if secret {
      None
    } else {
      value
        .map(decode_makefile_value)
        .transpose()?
        .map(|value| format!("sha256:{}", ContentDigest::sha256(value.as_bytes())))
    };
    environment.insert(EnvironmentObservation {
      name: name.to_string(),
      value_digest,
      secret_capability: secret,
    });
  }
  Ok((reads, environment))
}

/// Parse the first Make dependency rule without assigning cache authority to it.
///
/// Rustc dep-info and ELF linker dependency files use the same escaped path
/// grammar. Callers must still validate the target and capture every returned
/// path under their own authority boundary.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn makefile_dependency_paths(path: &Path, current_dir: &Path) -> RailResult<(PathBuf, Vec<PathBuf>)> {
  let (_, target, dependencies) = makefile_dependency_rule(path, current_dir)?;
  Ok((target, dependencies))
}

fn makefile_dependency_rule(path: &Path, current_dir: &Path) -> RailResult<(String, PathBuf, Vec<PathBuf>)> {
  let text = fs::read_to_string(path)
    .map_err(|error| RailError::message(format!("failed to read dep-info '{}': {error}", path.display())))?;
  let logical = text.replace("\\\r\n", "").replace("\\\n", "");
  let dependency_line = logical
    .lines()
    .find(|line| !line.starts_with('#') && line.contains(": "))
    .ok_or_else(|| RailError::message(format!("dep-info '{}' has no dependency rule", path.display())))?;
  let (target, dependencies) = dependency_line
    .split_once(": ")
    .ok_or_else(|| RailError::message(format!("dep-info '{}' has an invalid dependency rule", path.display())))?;
  let mut targets = makefile_words(target)?.into_iter();
  let target = targets
    .next()
    .ok_or_else(|| RailError::message(format!("dep-info '{}' has no dependency target", path.display())))?;
  if targets.next().is_some() {
    return Err(RailError::message(format!(
      "dep-info '{}' has multiple dependency targets",
      path.display()
    )));
  }
  let target = resolve_argument_path(&target, current_dir);
  let dependencies = makefile_words(dependencies)?
    .into_iter()
    .map(|dependency| resolve_argument_path(&dependency, current_dir))
    .collect();
  Ok((logical, target, dependencies))
}

fn decode_makefile_value(input: &str) -> RailResult<String> {
  if input.is_empty() {
    return Ok(String::new());
  }
  let mut words = makefile_words(input)?.into_iter();
  let Some(value) = words.next() else {
    return Err(RailError::message("dep-info environment value is missing"));
  };
  if words.next().is_some() {
    return Err(RailError::message(
      "dep-info environment value contains an unescaped separator",
    ));
  }
  Ok(value)
}

fn makefile_words(input: &str) -> RailResult<Vec<String>> {
  let mut words = Vec::new();
  let mut word = String::new();
  let mut characters = input.chars().peekable();
  while let Some(character) = characters.next() {
    match character {
      '\\' => match characters.peek().copied() {
        Some(next) if next.is_ascii_whitespace() || matches!(next, '\\' | '#' | ':') => {
          word.push(next);
          characters.next();
        }
        Some(_) => word.push('\\'),
        None => return Err(RailError::message("dep-info ends with an incomplete escape")),
      },
      whitespace if whitespace.is_ascii_whitespace() => {
        if !word.is_empty() {
          words.push(std::mem::take(&mut word));
        }
      }
      _ => word.push(character),
    }
  }
  if !word.is_empty() {
    words.push(word);
  }
  Ok(words)
}

fn sort_and_deduplicate_files(files: &mut Vec<FileObservation>) {
  files.sort();
  files.dedup();
}

#[cfg(test)]
fn portable_argument(argument: &str, source_root: &Path, canonical_source_root: &Path) -> String {
  let roots = portable_argument_roots(source_root, canonical_source_root);
  portable_argument_with_roots(argument, &roots, false)
}

fn portable_compiler_arguments(arguments: &[String], source_root: &Path, canonical_source_root: &Path) -> Vec<String> {
  let roots = portable_argument_roots(source_root, canonical_source_root);
  let mut reviewed_value = false;
  arguments
    .iter()
    .map(|argument| {
      let portable = portable_argument_with_roots(argument, &roots, reviewed_value);
      reviewed_value = matches!(
        argument.as_str(),
        "--emit" | "--extern" | "--out-dir" | "--remap-path-prefix" | "-L"
      );
      portable
    })
    .collect()
}

fn portable_argument_roots(source_root: &Path, canonical_source_root: &Path) -> [String; 4] {
  let mut roots = [
    source_root.to_string_lossy().into_owned(),
    canonical_source_root.to_string_lossy().into_owned(),
    crate::utils::path_to_git_format(source_root),
    crate::utils::path_to_git_format(canonical_source_root),
  ];
  roots.sort_unstable_by_key(|root| std::cmp::Reverse(root.len()));
  roots
}

fn portable_argument_with_roots(argument: &str, roots: &[String], reviewed_value: bool) -> String {
  let reviewed_path = reviewed_value && roots.iter().any(|root| argument.contains(root))
    || roots.iter().any(|root| {
      argument == root
        || argument
          .strip_prefix(root)
          .is_some_and(|suffix| suffix.starts_with(['/', '\\']))
        || [
          "--emit=",
          "--extern=",
          "--out-dir=",
          "--remap-path-prefix=",
          "-Ldependency=",
        ]
        .iter()
        .any(|prefix| argument.starts_with(prefix) && argument.contains(root))
    });
  if !reviewed_path {
    return argument.to_string();
  }
  roots.iter().fold(argument.to_string(), |portable, root| {
    if portable.contains(root) {
      portable.replace(root, "repository:")
    } else {
      portable
    }
  })
}

pub(crate) fn is_secret_name(name: &str) -> bool {
  let normalized = name.to_ascii_lowercase().replace('_', "-");
  normalized == "token"
    || normalized.ends_with("-token")
    || normalized == "api-key"
    || normalized.ends_with("-api-key")
    || normalized == "access-key"
    || normalized.ends_with("-access-key")
    || normalized.contains("access-key-id")
    || normalized.contains("password")
    || normalized.contains("secret")
    || normalized.contains("credential")
    || normalized.contains("private-key")
}

fn is_cargo_provided_environment(name: &str) -> bool {
  name.starts_with("CARGO_PKG_")
    || matches!(
      name,
      "CARGO_BIN_NAME" | "CARGO_CRATE_NAME" | "CARGO_MANIFEST_DIR" | "CARGO_PRIMARY_PACKAGE"
    )
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
  use std::os::unix::fs::PermissionsExt as _;

  metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
  false
}

#[cfg(test)]
mod tests {
  use super::*;

  #[cfg(unix)]
  #[test]
  fn missing_repository_path_resolves_through_an_alternate_root_spelling() {
    use std::os::unix::fs::symlink;

    let source_root = tempfile::tempdir().expect("source root");
    let alias_root = tempfile::tempdir().expect("alias root");
    let output_parent = source_root.path().join("build-two/debug/deps");
    fs::create_dir_all(&output_parent).expect("output parent");
    let alias = alias_root.path().join("workspace");
    symlink(source_root.path(), &alias).expect("source-root alias");
    let output = alias.join("build-two/debug/deps/libfixture.rmeta");

    assert_eq!(
      ObservationPath::capture(&output, source_root.path(), source_root.path()),
      ObservationPath::Repository("build-two/debug/deps/libfixture.rmeta".to_string())
    );
    assert!(!output.exists(), "the restore destination must still be missing");
  }

  fn base_unit() -> CompilationUnit {
    CompilationUnit {
      version: COMPILATION_UNIT_VERSION,
      package: "local:crates/example/Cargo.toml#example@0.1.0".to_string(),
      target_kind: CompilationTargetKind::Library,
      cargo_target_kinds: BTreeSet::from(["lib".to_string()]),
      target_name: "example".to_string(),
      crate_types: BTreeSet::from(["lib".to_string()]),
      role: CompilationRole::Target,
      mode: CompilerMode::Rustc,
      platform: "x86_64-unknown-linux-gnu".to_string(),
      target_specification: "sha256:target".to_string(),
      profile: CompilationProfile {
        opt_level: "0".to_string(),
        debuginfo: "2".to_string(),
        debug_assertions: true,
        overflow_checks: true,
        test: false,
      },
      features: BTreeSet::from(["default".to_string()]),
      cfg: BTreeSet::from(["feature=\"default\"".to_string()]),
      emit_modes: BTreeSet::from(["metadata".to_string()]),
      linker_responsibility: LinkerResponsibility::None,
      compiler_arguments: vec!["--edition=2024".to_string()],
      dependencies: vec![CompilationDependencyEdge {
        extern_name: "dep".to_string(),
        artifact_digest: "sha256:dep".to_string(),
        producer_unit: Some("v1-sha256-dep".to_string()),
      }],
      build_script_results: BTreeSet::from([BuildScriptResultDependency {
        producer_action: "build-script-v1-sha256-action".to_string(),
        result_digest: "build-script-result-v1-sha256-result".to_string(),
      }]),
    }
  }

  fn manifest_for_build_script_propagation(
    package: &str,
    target_kind: CompilationTargetKind,
  ) -> CompilationObservationManifest {
    let mut unit = base_unit();
    unit.package = package.to_string();
    unit.target_kind = target_kind;
    unit.target_name = package.to_string();
    unit.dependencies.clear();
    unit.build_script_results.clear();
    let unit_identity = unit.identity().expect("unit identity");
    CompilationObservationManifest {
      version: COMPILATION_OBSERVATION_VERSION,
      cargo_artifact_identity: None,
      unit,
      unit_identity,
      declared_inputs: Vec::new(),
      observed_reads: Vec::new(),
      dependency_artifacts: Vec::new(),
      emitted_outputs: Vec::new(),
      executable_output: None,
      execution: CompilationExecutionMetadata {
        compiler: None,
        wrappers: Vec::new(),
        cache_wrapper: None,
        platform_identity: "test-platform".to_string(),
        environment_reads: BTreeSet::new(),
        success: true,
        cargo_fresh: false,
      },
      bypasses: BTreeSet::new(),
      build_script_action_key: None,
      build_script_result: None,
    }
  }

  #[test]
  fn compilation_unit_identity_mutates_for_every_modeled_field() {
    type UnitMutation = Box<dyn Fn(&mut CompilationUnit)>;

    let baseline = base_unit();
    let baseline_identity = baseline.identity().expect("baseline identity");
    let mutations: Vec<UnitMutation> = vec![
      Box::new(|unit| unit.version += 1),
      Box::new(|unit| unit.package.push_str("-changed")),
      Box::new(|unit| unit.target_kind = CompilationTargetKind::Binary),
      Box::new(|unit| {
        unit.cargo_target_kinds.insert("rlib".to_string());
      }),
      Box::new(|unit| unit.target_name.push_str("-changed")),
      Box::new(|unit| {
        unit.crate_types.insert("rlib".to_string());
      }),
      Box::new(|unit| unit.role = CompilationRole::Host),
      Box::new(|unit| unit.mode = CompilerMode::Rustdoc),
      Box::new(|unit| unit.platform.push_str("-changed")),
      Box::new(|unit| unit.target_specification.push_str("-changed")),
      Box::new(|unit| unit.profile.opt_level = "3".to_string()),
      Box::new(|unit| unit.profile.debuginfo = "0".to_string()),
      Box::new(|unit| unit.profile.debug_assertions = false),
      Box::new(|unit| unit.profile.overflow_checks = false),
      Box::new(|unit| unit.profile.test = true),
      Box::new(|unit| {
        unit.features.insert("extra".to_string());
      }),
      Box::new(|unit| {
        unit.cfg.insert("unix".to_string());
      }),
      Box::new(|unit| {
        unit.emit_modes.insert("link".to_string());
      }),
      Box::new(|unit| unit.linker_responsibility = LinkerResponsibility::RustcDriver),
      Box::new(|unit| unit.compiler_arguments.push("-Copt-level=1".to_string())),
      Box::new(|unit| unit.dependencies[0].extern_name.push_str("-changed")),
      Box::new(|unit| unit.dependencies[0].artifact_digest.push_str("-changed")),
      Box::new(|unit| unit.dependencies[0].producer_unit = Some("changed".to_string())),
      Box::new(|unit| {
        unit.build_script_results.insert(BuildScriptResultDependency {
          producer_action: "build-script-v1-sha256-other".to_string(),
          result_digest: "build-script-result-v1-sha256-other".to_string(),
        });
      }),
    ];

    for mutate in mutations {
      let mut changed = baseline.clone();
      mutate(&mut changed);
      assert_ne!(changed.identity().expect("changed identity"), baseline_identity);
    }
  }

  #[test]
  fn build_script_result_rekeys_every_transitive_consumer_without_a_result_cycle() {
    let mut manifests = vec![
      manifest_for_build_script_propagation("script", CompilationTargetKind::BuildScript),
      manifest_for_build_script_propagation("script", CompilationTargetKind::Library),
      manifest_for_build_script_propagation("direct", CompilationTargetKind::BuildScript),
      manifest_for_build_script_propagation("direct", CompilationTargetKind::Library),
      manifest_for_build_script_propagation("transitive", CompilationTargetKind::Binary),
      manifest_for_build_script_propagation("unrelated", CompilationTargetKind::Library),
    ];
    let package_dependencies = HashMap::from([
      ("script".to_string(), BTreeSet::new()),
      ("direct".to_string(), BTreeSet::from(["script".to_string()])),
      ("transitive".to_string(), BTreeSet::from(["direct".to_string()])),
      ("unrelated".to_string(), BTreeSet::new()),
    ]);
    let baseline = manifests
      .iter()
      .map(|manifest| {
        (
          (manifest.unit.package.clone(), manifest.unit.target_kind.clone()),
          manifest.unit_identity.clone(),
        )
      })
      .collect::<BTreeMap<_, _>>();
    let binding = BuildScriptResultBinding {
      package: "script".to_string(),
      action_key: Some("build-script-v1-sha256-action".to_string()),
      result_digest: Some("build-script-result-v1-sha256-first".to_string()),
    };
    attach_build_script_result_dependencies(&mut manifests, &package_dependencies, std::slice::from_ref(&binding))
      .expect("bind build-script result");

    for manifest in &manifests {
      let is_producer =
        manifest.unit.package == "script" && manifest.unit.target_kind == CompilationTargetKind::BuildScript;
      let is_unrelated = manifest.unit.package == "unrelated";
      if is_producer || is_unrelated {
        assert!(manifest.unit.build_script_results.is_empty());
        assert_eq!(
          manifest.unit_identity,
          baseline[&(manifest.unit.package.clone(), manifest.unit.target_kind.clone())]
        );
      } else {
        assert_eq!(
          manifest.unit.build_script_results,
          BTreeSet::from([BuildScriptResultDependency {
            producer_action: "build-script-v1-sha256-action".to_string(),
            result_digest: "build-script-result-v1-sha256-first".to_string(),
          }])
        );
        assert_ne!(
          manifest.unit_identity,
          baseline[&(manifest.unit.package.clone(), manifest.unit.target_kind.clone())]
        );
      }
    }

    let first = manifests
      .iter()
      .map(|manifest| {
        (
          (manifest.unit.package.clone(), manifest.unit.target_kind.clone()),
          manifest.unit_identity.clone(),
        )
      })
      .collect::<BTreeMap<_, _>>();
    let changed = BuildScriptResultBinding {
      result_digest: Some("build-script-result-v1-sha256-second".to_string()),
      ..binding
    };
    attach_build_script_result_dependencies(&mut manifests, &package_dependencies, &[changed])
      .expect("rebind changed build-script result");
    for manifest in &manifests {
      let key = (manifest.unit.package.clone(), manifest.unit.target_kind.clone());
      let affected = manifest.unit.package != "unrelated"
        && !(manifest.unit.package == "script" && manifest.unit.target_kind == CompilationTargetKind::BuildScript);
      if affected {
        assert_ne!(manifest.unit_identity, first[&key]);
      } else {
        assert_eq!(manifest.unit_identity, first[&key]);
      }
    }
  }

  #[test]
  fn incomplete_build_script_result_makes_only_semantic_consumers_non_reusable() {
    let mut manifests = vec![
      manifest_for_build_script_propagation("script", CompilationTargetKind::BuildScript),
      manifest_for_build_script_propagation("script", CompilationTargetKind::Library),
      manifest_for_build_script_propagation("consumer", CompilationTargetKind::Library),
      manifest_for_build_script_propagation("unrelated", CompilationTargetKind::Library),
    ];
    let package_dependencies = HashMap::from([
      ("script".to_string(), BTreeSet::new()),
      ("consumer".to_string(), BTreeSet::from(["script".to_string()])),
      ("unrelated".to_string(), BTreeSet::new()),
    ]);
    let binding = BuildScriptResultBinding {
      package: "script".to_string(),
      action_key: None,
      result_digest: None,
    };
    attach_build_script_result_dependencies(&mut manifests, &package_dependencies, &[binding])
      .expect("bind incomplete result");

    for manifest in &manifests {
      let affected = matches!(manifest.unit.package.as_str(), "script" | "consumer")
        && !(manifest.unit.package == "script" && manifest.unit.target_kind == CompilationTargetKind::BuildScript);
      assert_eq!(manifest.bypasses.contains("build_script_result_unavailable"), affected);
      assert!(manifest.unit.build_script_results.is_empty());
    }
  }

  #[test]
  fn target_kinds_cover_supported_and_explicitly_unsupported_domains() {
    let kinds = [
      CompilationTargetKind::Library,
      CompilationTargetKind::Binary,
      CompilationTargetKind::Test,
      CompilationTargetKind::Example,
      CompilationTargetKind::Benchmark,
      CompilationTargetKind::Documentation,
      CompilationTargetKind::ProcMacro,
      CompilationTargetKind::BuildScript,
    ];
    let identities = kinds
      .into_iter()
      .map(|kind| {
        let mut unit = base_unit();
        unit.target_kind = kind;
        unit.identity().expect("typed identity")
      })
      .collect::<BTreeSet<_>>();
    assert_eq!(identities.len(), 8);
  }

  #[test]
  fn native_metadata_path_inference_does_not_reject_other_observation_classes() {
    let mut bypasses = BTreeSet::new();
    let arguments = [
      "--crate-name".to_string(),
      "example".to_string(),
      "--crate-type".to_string(),
      "bin".to_string(),
      "--emit".to_string(),
      "metadata".to_string(),
      "--out-dir".to_string(),
      "/tmp/target".to_string(),
    ];

    let parsed = ParsedArguments::parse(&arguments, Path::new("/workspace"), CompilerMode::Rustc, &mut bypasses);

    assert!(parsed.metadata_paths.is_empty());
    assert!(bypasses.is_empty());
  }

  #[cfg(any(target_os = "macos", target_os = "linux"))]
  #[test]
  fn native_link_path_inference_matches_platform_rustc_names() {
    #[cfg(target_os = "macos")]
    let dynamic_suffix = "dylib";
    #[cfg(target_os = "linux")]
    let dynamic_suffix = "so";
    for (crate_type, crate_name, expected) in [
      ("bin", "build_script_build", "build_script_build-1234".to_string()),
      (
        "proc-macro",
        "fixture_macros",
        format!("libfixture_macros-1234.{dynamic_suffix}"),
      ),
      (
        "dylib",
        "fixture_dynamic",
        format!("libfixture_dynamic-1234.{dynamic_suffix}"),
      ),
      ("cdylib", "fixture_c", format!("libfixture_c-1234.{dynamic_suffix}")),
      ("staticlib", "fixture_static", "libfixture_static-1234.a".to_string()),
    ] {
      let mut bypasses = BTreeSet::new();
      let arguments = [
        "--crate-name".to_string(),
        crate_name.to_string(),
        "--crate-type".to_string(),
        crate_type.to_string(),
        "--emit=dep-info,link".to_string(),
        "-C".to_string(),
        "extra-filename=-1234".to_string(),
        "--out-dir".to_string(),
        "/tmp/target".to_string(),
      ];

      let parsed = ParsedArguments::parse(&arguments, Path::new("/workspace"), CompilerMode::Rustc, &mut bypasses);

      assert!(
        parsed
          .output_paths
          .contains(&PathBuf::from("/tmp/target").join(expected))
      );
      assert!(bypasses.is_empty());
    }
  }

  #[cfg(target_os = "macos")]
  #[test]
  fn explicit_native_link_path_is_authoritative() {
    let mut bypasses = BTreeSet::new();
    let arguments = [
      "--crate-name=fixture_cli".to_string(),
      "--crate-type=bin".to_string(),
      "--emit=dep-info,link=/tmp/explicit-output".to_string(),
      "-Cextra-filename=-ignored".to_string(),
      "--out-dir=/tmp/target".to_string(),
    ];

    let parsed = ParsedArguments::parse(&arguments, Path::new("/workspace"), CompilerMode::Rustc, &mut bypasses);

    assert!(parsed.output_paths.contains(&PathBuf::from("/tmp/explicit-output")));
    assert!(
      !parsed
        .output_paths
        .contains(&PathBuf::from("/tmp/target/fixture_cli-ignored"))
    );
    assert!(bypasses.is_empty());
  }

  #[test]
  fn cargo_fresh_is_result_metadata_not_artifact_identity() {
    let directory = tempfile::tempdir().expect("tempdir");
    let output_path = directory.path().join("unit.rmeta");
    fs::write(&output_path, b"artifact").expect("artifact");
    let output = FileObservation::capture(&output_path, directory.path(), directory.path()).expect("output identity");
    let artifact = CargoArtifactObservation {
      package: "local:Cargo.toml#unit@0.1.0".to_string(),
      target_kinds: BTreeSet::from(["lib".to_string()]),
      target_name: "unit".to_string(),
      crate_types: BTreeSet::from(["lib".to_string()]),
      source: ObservationPath::Repository("src/lib.rs".to_string()),
      profile: base_unit().profile,
      features: BTreeSet::new(),
      outputs: vec![output],
      executable: None,
      fresh: false,
      bypasses: BTreeSet::new(),
    };
    let baseline = cargo_artifact_identity(&artifact).expect("artifact identity");
    let mut fresh = CargoArtifactObservation {
      fresh: true,
      ..artifact
    };
    assert_eq!(cargo_artifact_identity(&fresh).expect("fresh identity"), baseline);

    fresh.target_name.push_str("-changed");
    assert_ne!(cargo_artifact_identity(&fresh).expect("changed identity"), baseline);
  }

  #[test]
  fn explicit_target_argv_distinguishes_host_and_target_units() {
    let target = "aarch64-unknown-linux-gnu";
    let context = CompilationObservationContext {
      source_root: PathBuf::from("/workspace"),
      host_target: target.to_string(),
      targets: vec![ObservedTargetIdentity {
        selectors: BTreeSet::from([target.to_string()]),
        platform: target.to_string(),
        identity: "sha256:target".to_string(),
        cfg: BTreeSet::from(["target_arch=\"aarch64\"".to_string()]),
      }],
    };
    let mut raw = raw_invocation();
    raw.target_argument = Some(target.to_string());
    let target_domain =
      compilation_domain(Some(&raw), &CompilationTargetKind::Library, &context, target).expect("target domain");
    raw.target_argument = None;
    let host_domain =
      compilation_domain(Some(&raw), &CompilationTargetKind::ProcMacro, &context, target).expect("host domain");

    assert_eq!(target_domain.role, CompilationRole::Target);
    assert_eq!(host_domain.role, CompilationRole::Host);
    assert_ne!(target_domain.role, host_domain.role);
  }

  #[test]
  fn cargo_target_classification_is_typed_before_support_decisions() {
    for (raw, expected) in [
      (&["lib"][..], CompilationTargetKind::Library),
      (&["bin"][..], CompilationTargetKind::Binary),
      (&["test"][..], CompilationTargetKind::Test),
      (&["example"][..], CompilationTargetKind::Example),
      (&["bench"][..], CompilationTargetKind::Benchmark),
      (&["proc-macro"][..], CompilationTargetKind::ProcMacro),
      (&["custom-build"][..], CompilationTargetKind::BuildScript),
    ] {
      let kinds = raw.iter().map(|kind| (*kind).to_string()).collect();
      assert_eq!(classify_target_kind(&kinds), expected);
    }
  }

  #[test]
  fn file_observation_revalidation_uses_exact_bytes_not_file_size() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("input");
    fs::write(&path, b"first").expect("first bytes");
    let observed = FileObservation::capture(&path, directory.path(), directory.path()).expect("observation");
    fs::write(&path, b"other").expect("same-size mutation");

    assert!(!observed.revalidate(directory.path()));

    fs::remove_file(&path).expect("remove input");
    assert!(!observed.revalidate(directory.path()));
  }

  #[test]
  fn dep_info_parser_digests_reads_and_redacts_secret_values() {
    let directory = tempfile::tempdir().expect("tempdir");
    fs::write(directory.path().join("source file.rs"), "fn main() {}\n").expect("source");
    fs::write(
      directory.path().join("unit.d"),
      "unit: source\\ file.rs\n# env-dep:VISIBLE=value\n# env-dep:WINDOWS_PATH=C:\\\\work\\\\fixture\\ root\n# env-dep:API_TOKEN=never-store-this\n# env-dep:API_KEY=also-never-store-this\n",
    )
    .expect("dep info");

    let (reads, environment) =
      parse_dep_info(&directory.path().join("unit.d"), directory.path(), directory.path()).expect("parse dep info");

    assert_eq!(reads.len(), 1);
    assert!(
      environment
        .iter()
        .any(|entry| entry.name == "VISIBLE" && entry.value_digest.is_some())
    );
    assert!(environment.iter().any(|entry| {
      entry.name == "WINDOWS_PATH"
        && entry.value_digest.as_deref()
          == Some(format!("sha256:{}", ContentDigest::sha256(br"C:\work\fixture root")).as_str())
    }));
    assert!(
      environment
        .iter()
        .any(|entry| entry.name == "API_TOKEN" && entry.secret_capability && entry.value_digest.is_none())
    );
    assert!(
      environment
        .iter()
        .any(|entry| entry.name == "API_KEY" && entry.secret_capability && entry.value_digest.is_none())
    );
    let encoded = serde_json::to_string(&environment).expect("serialize environment");
    assert!(!encoded.contains("never-store-this"));
    assert!(!encoded.contains("also-never-store-this"));
  }

  #[test]
  fn dep_info_words_preserve_windows_separators_and_decode_make_escapes() {
    assert_eq!(
      makefile_words(r"C:\work\crate\src\lib.rs C:\work\source\ file.rs").expect("parse dep-info words"),
      [r"C:\work\crate\src\lib.rs", r"C:\work\source file.rs"]
    );
    assert_eq!(
      decode_makefile_value(r"C:\\work\\fixture\ root").expect("decode dep-info environment value"),
      r"C:\work\fixture root"
    );
    assert_eq!(decode_makefile_value("").expect("decode empty value"), "");
  }

  #[test]
  fn makefile_dependency_rule_selects_one_exact_target_and_continued_inputs() {
    let directory = tempfile::tempdir().expect("tempdir");
    let dep_info = directory.path().join("link.d");
    fs::write(
      &dep_info,
      "target\\ output: first\\ input.o \\\n second.a\n# ignored comment\n",
    )
    .expect("dependency file");

    let (target, dependencies) =
      makefile_dependency_paths(&dep_info, directory.path()).expect("parse make dependency rule");
    assert_eq!(target, directory.path().join("target output"));
    assert_eq!(
      dependencies,
      [
        directory.path().join("first input.o"),
        directory.path().join("second.a")
      ]
    );
  }

  #[test]
  fn portable_compiler_arguments_replace_native_and_canonical_root_aliases() {
    let source = Path::new("/var/workspace");
    let canonical = Path::new("/private/var/workspace");
    assert_eq!(
      portable_argument("--out-dir=/var/workspace/target", source, canonical),
      "--out-dir=repository:/target"
    );
    assert_eq!(
      portable_argument("--out-dir=/private/var/workspace/target", source, canonical),
      "--out-dir=repository:/target"
    );

    let windows = Path::new(r"C:\work\workspace");
    assert_eq!(
      portable_argument(r"--out-dir=C:\work\workspace\target", windows, windows),
      r"--out-dir=repository:\target"
    );
    assert_eq!(
      portable_argument("--extern=dep=/var/workspace/target/libdep.rmeta", source, canonical),
      "--extern=dep=repository:/target/libdep.rmeta"
    );
    assert_eq!(
      portable_argument("metadata=/var/workspace", source, canonical),
      "metadata=/var/workspace"
    );
    assert_eq!(
      portable_argument("cfg(path=\"/var/workspace\")", source, canonical),
      "cfg(path=\"/var/workspace\")"
    );
    assert_eq!(
      portable_compiler_arguments(
        &[
          "-L".to_string(),
          "dependency=/var/workspace/target/debug/deps".to_string(),
          "--extern".to_string(),
          "dep=/private/var/workspace/target/debug/deps/libdep.rmeta".to_string(),
        ],
        source,
        canonical,
      ),
      [
        "-L",
        "dependency=repository:/target/debug/deps",
        "--extern",
        "dep=repository:/target/debug/deps/libdep.rmeta",
      ]
    );
    assert_eq!(
      portable_compiler_arguments(
        &["-C".to_string(), "metadata=/var/workspace".to_string()],
        source,
        canonical,
      ),
      ["-C", "metadata=/var/workspace"]
    );
  }

  #[test]
  fn compiler_arguments_classify_only_proc_macro_as_a_toolchain_owned_pathless_extern() {
    let mut bypasses = BTreeSet::new();
    let parsed = ParsedArguments::parse(
      &["--extern".to_string(), "proc_macro".to_string()],
      Path::new("/workspace"),
      CompilerMode::Rustc,
      &mut bypasses,
    );
    assert!(parsed.dependency_paths.is_empty());
    assert!(bypasses.is_empty());

    ParsedArguments::parse(
      &["--extern=dependency_without_path".to_string()],
      Path::new("/workspace"),
      CompilerMode::Rustc,
      &mut bypasses,
    );
    assert_eq!(
      bypasses,
      BTreeSet::from(["dependency_artifact_path_unavailable".to_string()])
    );
  }

  #[test]
  fn metadata_only_proc_macro_has_one_typed_rmeta_output() {
    let mut bypasses = BTreeSet::new();
    let parsed = ParsedArguments::parse(
      &[
        "--crate-name=fixture_macros".to_string(),
        "--crate-type=proc-macro".to_string(),
        "--emit=dep-info,metadata".to_string(),
        "-Cextra-filename=-known".to_string(),
        "--out-dir=/tmp/target".to_string(),
        "--extern=proc_macro".to_string(),
        "src/lib.rs".to_string(),
      ],
      Path::new("/workspace"),
      CompilerMode::Rustc,
      &mut bypasses,
    );

    assert_eq!(
      parsed.metadata_paths,
      [PathBuf::from("/tmp/target/libfixture_macros-known.rmeta")]
    );
    assert!(bypasses.is_empty());
  }

  #[test]
  fn rustdoc_invocation_correlates_exact_dep_info_and_cargo_artifact() {
    let directory = tempfile::tempdir().expect("tempdir");
    let source_root = directory.path();
    let source_dir = source_root.join("src");
    let doc_dir = source_root.join("target/doc");
    let dependency_dir = source_root.join("target/debug/deps");
    fs::create_dir_all(&source_dir).expect("source directory");
    fs::create_dir_all(doc_dir.join("docs_unit")).expect("documentation directory");
    fs::create_dir_all(&dependency_dir).expect("dependency directory");
    let source = source_dir.join("lib.rs");
    let nested = source_dir.join("nested.rs");
    let dependency = dependency_dir.join("libdep.rmeta");
    let index = doc_dir.join("docs_unit/index.html");
    let dep_info = doc_dir.join("docs_unit.d");
    fs::write(&source, "mod nested;\n").expect("crate root");
    fs::write(&nested, "pub fn value() {}\n").expect("nested source");
    fs::write(&dependency, "dependency artifact").expect("dependency artifact");

    let arguments = vec![
      "--crate-name".into(),
      "docs_unit".into(),
      "--crate-type".into(),
      "lib".into(),
      source.as_os_str().to_owned(),
      "-o".into(),
      doc_dir.as_os_str().to_owned(),
      "--extern".into(),
      format!("dep={}", dependency.display()).into(),
      "--emit=html-static-files,html-non-static-files,dep-info".into(),
    ];
    let raw_directory = source_root.join("observations");
    let recorder =
      begin_rustdoc_invocation(&raw_directory, source_root, &arguments).expect("begin rustdoc observation");
    fs::write(&index, "<html>docs</html>").expect("documentation index");
    fs::write(
      &dep_info,
      format!("{}: {} {}\n", dep_info.display(), source.display(), nested.display()),
    )
    .expect("rustdoc dep-info");
    recorder.finish(true).expect("finish rustdoc observation");
    let raw = load_raw(&raw_directory).expect("load rustdoc observation");
    assert_eq!(raw.len(), 1);
    assert_eq!(raw[0].mode, CompilerMode::Rustdoc);

    let artifact_output = FileObservation::capture(&index, source_root, source_root).expect("artifact output");
    let artifact = CargoArtifactObservation {
      package: "local:Cargo.toml#docs-unit@0.1.0".to_string(),
      target_kinds: BTreeSet::from(["lib".to_string()]),
      target_name: "docs_unit".to_string(),
      crate_types: BTreeSet::from(["lib".to_string()]),
      source: ObservationPath::capture(&source, source_root, source_root),
      profile: base_unit().profile,
      features: BTreeSet::new(),
      outputs: vec![artifact_output],
      executable: None,
      fresh: false,
      bypasses: BTreeSet::new(),
    };
    let target = "test-target";
    let context = CompilationObservationContext {
      source_root: source_root.to_path_buf(),
      host_target: target.to_string(),
      targets: vec![ObservedTargetIdentity {
        selectors: BTreeSet::from([target.to_string()]),
        platform: target.to_string(),
        identity: "sha256:target".to_string(),
        cfg: BTreeSet::new(),
      }],
    };
    let mut manifests = build_manifests(raw, vec![artifact], &context, "default", CompilerMode::Rustdoc)
      .expect("correlate rustdoc observation");
    assert_eq!(manifests.len(), 1);
    manifests[0].execution.cache_wrapper = Some(CompilerCacheWrapperMetadata::new(
      CompilerCacheWrapperStatus::Bypassed,
      "rustdoc_not_graduated",
    ));
    let manifest = &manifests[0];
    assert_eq!(manifest.unit.mode, CompilerMode::Rustdoc);
    assert_eq!(manifest.unit.target_kind, CompilationTargetKind::Documentation);
    assert_eq!(manifest.declared_inputs.len(), 1);
    assert_eq!(manifest.observed_reads.len(), 2);
    assert_eq!(manifest.dependency_artifacts.len(), 1);
    assert_eq!(manifest.emitted_outputs.len(), 2);
    assert!(manifest.bypasses.contains("rustdoc_output_tree_unavailable"));
    assert!(!manifest.bypasses.contains("rustdoc_dep_info_unavailable"));
    assert_eq!(manifest.revalidation_reason(source_root), None);

    fs::write(&nested, "pub fn other() {}\n").expect("same-size nested mutation");
    assert_eq!(
      manifest.revalidation_reason(source_root),
      Some("observed_compiler_read_changed")
    );
  }

  fn raw_invocation() -> RawCompilerInvocation {
    RawCompilerInvocation {
      version: RAW_INVOCATION_VERSION,
      mode: CompilerMode::Rustc,
      crate_name: Some("unit".to_string()),
      crate_types: BTreeSet::from(["lib".to_string()]),
      target_argument: None,
      cfg: BTreeSet::new(),
      emit_modes: BTreeSet::from(["metadata".to_string()]),
      test_mode: false,
      compiler_arguments: Vec::new(),
      declared_inputs: Vec::new(),
      observed_reads: Vec::new(),
      dependency_artifacts: Vec::new(),
      emitted_outputs: Vec::new(),
      environment_reads: BTreeSet::new(),
      compiler: None,
      wrappers: Vec::new(),
      cache_wrapper: None,
      success: true,
      bypasses: BTreeSet::new(),
    }
  }

  #[test]
  fn raw_publication_survives_process_identifier_reuse() {
    let directory = tempfile::tempdir().expect("observation directory");
    let first = raw_invocation();
    publish_raw(directory.path(), &first).expect("first observation");

    let mut second = first.clone();
    second.crate_name = Some("other_unit".to_string());
    publish_raw(directory.path(), &second).expect("second observation");
    publish_raw(directory.path(), &first).expect("idempotent first observation");

    let loaded = load_raw(directory.path()).expect("complete observations");
    assert_eq!(loaded.len(), 2);
    assert!(loaded.contains(&first));
    assert!(loaded.contains(&second));
  }
}
