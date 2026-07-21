//! Versioned compilation-unit identities and exact post-execution evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{RailError, RailResult};
use crate::executable::ExecutableIdentity;
use crate::source::ContentDigest;
use crate::workspace::WorkspaceSnapshot;

pub(crate) const COMPILATION_OBSERVATION_VERSION: u32 = 1;
const COMPILATION_UNIT_VERSION: u32 = 1;
const RAW_INVOCATION_VERSION: u32 = 1;

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
    absolute
      .strip_prefix(source_root)
      .map(|relative| Self::Repository(crate::utils::path_to_git_format(relative)))
      .unwrap_or_else(|_| Self::Host(crate::utils::path_to_git_format(&absolute)))
  }

  pub(crate) fn resolve(&self, source_root: &Path) -> PathBuf {
    match self {
      Self::Repository(path) => source_root.join(path),
      Self::Host(path) => PathBuf::from(path),
    }
  }
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
    let bytes = fs::read(&absolute).map_err(|error| {
      RailError::message(format!(
        "failed to read observed file '{}': {error}",
        absolute.display()
      ))
    })?;
    Ok(Self {
      path: ObservationPath::capture(&absolute, current_dir, source_root),
      content_digest: format!("sha256:{}", ContentDigest::sha256(&bytes)),
      executable: is_executable(&metadata),
      symlink_target,
    })
  }

  pub(crate) fn revalidate(&self, source_root: &Path) -> bool {
    let path = self.path.resolve(source_root);
    Self::capture(&path, source_root, source_root).is_ok_and(|current| current == *self)
  }
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
  pub(crate) wrappers: Vec<ExecutableIdentity>,
  pub(crate) platform_identity: String,
  pub(crate) environment_reads: BTreeSet<EnvironmentObservation>,
  pub(crate) success: bool,
  pub(crate) cargo_fresh: bool,
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
  pub(crate) execution: CompilationExecutionMetadata,
  pub(crate) bypasses: BTreeSet<String>,
}

impl CompilationObservationManifest {
  /// Re-digest every file that supplied result evidence.
  ///
  /// Executables are re-digested once by the enclosing compiler-cache identity;
  /// repeating that work for every unit would hash the same toolchain many times.
  pub(crate) fn revalidation_reason(&self, source_root: &Path) -> Option<&'static str> {
    if self.version != COMPILATION_OBSERVATION_VERSION {
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
  output_paths: Vec<PathBuf>,
}

/// Wrapper evidence before Cargo compiler-artifact correlation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RawCompilerInvocation {
  version: u32,
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
  pub(crate) wrappers: Vec<ExecutableIdentity>,
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
    manifests.push(manifest_for_artifact(artifact, raw, context, requested_target)?);
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
  wrappers: &[ExecutableIdentity],
) {
  for manifest in manifests {
    if manifest.execution.compiler.is_none() {
      manifest.execution.compiler = Some(compiler.clone());
    }
    if manifest.execution.wrappers.is_empty() {
      manifest.execution.wrappers = wrappers.to_vec();
    }
  }
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
) -> RailResult<CompilationObservationManifest> {
  let cargo_artifact_identity = cargo_artifact_identity(&artifact)?;
  let target_kind = classify_target_kind(&artifact.target_kinds);
  let domain = compilation_domain(raw.as_ref(), &target_kind, context, requested_target)?;
  let mut bypasses = raw
    .as_ref()
    .map(|raw| raw.bypasses.clone())
    .unwrap_or_else(|| BTreeSet::from(["rustc_invocation_unavailable".to_string()]));
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
  let linker_responsibility = raw.as_ref().map_or(LinkerResponsibility::Unknown, |_| {
    if emit_modes.contains("link") {
      LinkerResponsibility::RustcDriver
    } else {
      LinkerResponsibility::None
    }
  });
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
    mode: CompilerMode::Rustc,
    platform: domain.platform,
    target_specification: domain.target_specification,
    profile: artifact.profile,
    features: artifact.features,
    cfg,
    emit_modes,
    linker_responsibility,
    compiler_arguments: raw.as_ref().map_or_else(Vec::new, |raw| raw.compiler_arguments.clone()),
    dependencies,
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
    execution,
    bypasses,
  })
}

fn manifest_without_artifact(
  raw: RawCompilerInvocation,
  context: &CompilationObservationContext,
  requested_target: &str,
) -> RailResult<CompilationObservationManifest> {
  let target_kind = raw_target_kind(&raw.crate_types, raw.test_mode);
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
    mode: CompilerMode::Rustc,
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
    execution: CompilationExecutionMetadata {
      compiler: raw.compiler,
      wrappers: raw.wrappers,
      platform_identity: platform_identity(),
      environment_reads: raw.environment_reads,
      success: raw.success,
      cargo_fresh: false,
    },
    bypasses,
  })
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
  let parsed = ParsedArguments::parse(&argument_text, &current_dir, source_root, &mut bypasses);
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
    output_paths: parsed.output_paths,
    raw: RawCompilerInvocation {
      version: RAW_INVOCATION_VERSION,
      crate_name: parsed.crate_name,
      crate_types: parsed.crate_types,
      target_argument: parsed.target_argument,
      cfg: parsed.cfg,
      emit_modes: parsed.emit_modes,
      test_mode: parsed.test_mode,
      compiler_arguments: argument_text
        .iter()
        .map(|argument| portable_argument(argument, source_root))
        .collect(),
      declared_inputs,
      observed_reads: Vec::new(),
      dependency_artifacts,
      emitted_outputs: Vec::new(),
      environment_reads: BTreeSet::new(),
      compiler: None,
      wrappers: Vec::new(),
      success: false,
      bypasses,
    },
  })
}

impl InvocationRecorder {
  /// Capture dep-info and emitted bytes after the compiler exits, then atomically publish raw evidence.
  pub(crate) fn finish(mut self, success: bool) -> RailResult<()> {
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
    fs::create_dir_all(&self.directory)?;
    let path = self.directory.join(format!("rustc-{}.json", std::process::id()));
    crate::utils::write_file_atomic(&path, &serde_json::to_vec(&self.raw)?)
  }
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
  output_paths: Vec<PathBuf>,
  out_dir: Option<PathBuf>,
  extra_filename: String,
}

impl ParsedArguments {
  fn parse(arguments: &[String], current_dir: &Path, _source_root: &Path, bypasses: &mut BTreeSet<String>) -> Self {
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
            parsed.capture_emit(value, current_dir, bypasses);
          }
        }
        "--extern" => {
          if let Some(value) = next() {
            parsed.capture_extern(value, current_dir, bypasses);
          }
        }
        "-o" => {
          if let Some(value) = next() {
            parsed.output_paths.push(resolve_argument_path(value, current_dir));
          }
        }
        "--out-dir" => {
          parsed.out_dir = next().map(|value| resolve_argument_path(value, current_dir));
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
          parsed.capture_emit(argument.trim_start_matches("--emit="), current_dir, bypasses);
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
    parsed
  }

  fn capture_emit(&mut self, value: &str, current_dir: &Path, bypasses: &mut BTreeSet<String>) {
    for emit in value.split(',') {
      let (mode, path) = emit
        .split_once('=')
        .map_or((emit, None), |(mode, path)| (mode, Some(path)));
      self.emit_modes.insert(mode.to_string());
      if let Some(path) = path {
        let path = resolve_argument_path(path, current_dir);
        if mode == "dep-info" {
          self.dep_info_paths.push(path.clone());
        }
        self.output_paths.push(path);
      } else if !matches!(mode, "dep-info" | "link" | "metadata") {
        bypasses.insert(format!("{mode}_output_path_unavailable"));
      }
    }
  }

  fn capture_extern(&mut self, value: &str, current_dir: &Path, bypasses: &mut BTreeSet<String>) {
    let Some((name, path)) = value.split_once('=') else {
      bypasses.insert("dependency_artifact_path_unavailable".to_string());
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

fn option_consumes_next(argument: &str) -> bool {
  matches!(
    argument,
    "--crate-name" | "--crate-type" | "--target" | "--cfg" | "--emit" | "--extern" | "--out-dir" | "-C" | "-o"
  )
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
  let text = fs::read_to_string(path)
    .map_err(|error| RailError::message(format!("failed to read dep-info '{}': {error}", path.display())))?;
  let logical = text.replace("\\\n", "");
  let dependency_line = logical
    .lines()
    .find(|line| !line.starts_with('#') && line.contains(": "))
    .ok_or_else(|| RailError::message(format!("dep-info '{}' has no dependency rule", path.display())))?;
  let (_, dependencies) = dependency_line
    .split_once(": ")
    .ok_or_else(|| RailError::message(format!("dep-info '{}' has an invalid dependency rule", path.display())))?;
  let mut reads = Vec::new();
  for dependency in makefile_words(dependencies)? {
    let dependency = resolve_argument_path(&dependency, current_dir);
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
    environment.insert(EnvironmentObservation {
      name: name.to_string(),
      value_digest: value
        .filter(|_| !secret)
        .map(|value| format!("sha256:{}", ContentDigest::sha256(value.as_bytes()))),
      secret_capability: secret,
    });
  }
  Ok((reads, environment))
}

fn makefile_words(input: &str) -> RailResult<Vec<String>> {
  let mut words = Vec::new();
  let mut word = String::new();
  let mut escaped = false;
  for character in input.chars() {
    if escaped {
      word.push(character);
      escaped = false;
    } else if character == '\\' {
      escaped = true;
    } else if character.is_ascii_whitespace() {
      if !word.is_empty() {
        words.push(std::mem::take(&mut word));
      }
    } else {
      word.push(character);
    }
  }
  if escaped {
    return Err(RailError::message("dep-info ends with an incomplete escape"));
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

fn portable_argument(argument: &str, source_root: &Path) -> String {
  let root = crate::utils::path_to_git_format(source_root);
  argument.replace(&root, "repository:")
}

fn is_secret_name(name: &str) -> bool {
  let normalized = name.to_ascii_lowercase().replace('_', "-");
  normalized == "token"
    || normalized.ends_with("-token")
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
  true
}

#[cfg(test)]
mod tests {
  use super::*;

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
    ];

    for mutate in mutations {
      let mut changed = baseline.clone();
      mutate(&mut changed);
      assert_ne!(changed.identity().expect("changed identity"), baseline_identity);
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
      "unit: source\\ file.rs\n# env-dep:VISIBLE=value\n# env-dep:API_TOKEN=never-store-this\n",
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
    assert!(
      environment
        .iter()
        .any(|entry| entry.name == "API_TOKEN" && entry.secret_capability && entry.value_digest.is_none())
    );
    let encoded = serde_json::to_string(&environment).expect("serialize environment");
    assert!(!encoded.contains("never-store-this"));
  }

  fn raw_invocation() -> RawCompilerInvocation {
    RawCompilerInvocation {
      version: RAW_INVOCATION_VERSION,
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
      success: true,
      bypasses: BTreeSet::new(),
    }
  }
}
