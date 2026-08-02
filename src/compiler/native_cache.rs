//! Native rustc-result reuse for one explicitly graduated invocation class.
//!
//! A candidate identity is only an index. Reuse requires revalidating the
//! complete stored observation, deriving its final action identity again, and
//! restoring the locally bound result through the verified CAS.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::compiler::observation::{
  CompilerCacheWrapperMetadata, CompilerCacheWrapperStatus, CompilerMode, FileObservation, InvocationRecorder,
  NativeOutputPaths, ObservationPath, RawCompilerInvocation,
};
use crate::compiler::wrapper::CacheWrapperPlan;
use crate::error::{RailError, RailResult};
use crate::hermetic::cas::NativeCacheLookup;
use crate::hermetic::cas::{LocalCas, NativeCacheCandidates, NativeStoreRequest};
use crate::instrumentation::{
  NativeCacheWrapperDiagnostics, NativeCacheWrapperEventDiagnostics, NativeCacheWrapperPhase, NativeCacheWrapperTrace,
  NativeCacheWrapperTraceSnapshot, NativeCacheWrapperWork,
};
use crate::source::ContentDigest;

pub(crate) const CANDIDATE_KEY_PREFIX: &str = "compiler-candidate-v3-sha256-";
pub(crate) const ACTION_KEY_PREFIX: &str = "compiler-action-v3-sha256-";
pub(crate) const SESSION_ENV: &str = "CARGO_RAIL_NATIVE_COMPILER_CACHE_SESSION";
pub(crate) const DISPOSITION_ENV: &str = "CARGO_RAIL_NATIVE_COMPILER_CACHE_DISPOSITION";
pub(crate) const DIAGNOSTIC_EXECUTION_CONTRACT: &str = "diagnostic-workspace-wrapper-v3";
pub(crate) const DIRECT_EXECUTION_CONTRACT: &str = "direct-global-wrapper-v3";
const SESSION_FILE: &str = "native-compiler-cache-session-v3.json";
const DIRECT_CONTEXT_FILE: &str = "native-compiler-cache-context-v2.json";
const UNIT_EVIDENCE_DIRECTORY: &str = "native-cache-unit-evidence";
#[cfg(not(windows))]
const DIRECT_WRAPPER_NAME: &str = "cargo-rail-native-rustc-wrapper";
#[cfg(windows)]
const DIRECT_WRAPPER_NAME: &str = "cargo-rail-native-rustc-wrapper.exe";
const GRADUATED_NATIVE_CACHE_CLASS: &str = "library_metadata_rlib";
const NATIVE_CACHE_CAPABILITY_SCHEMA_VERSION: u32 = 2;
const NATIVE_CACHE_IDENTITY_CONTRACT_VERSION: u32 = 3;
const NATIVE_CACHE_EVENT_EVIDENCE_VERSION: u32 = 4;
const NATIVE_CACHE_RUN_EVENT_VERSION: u32 = 3;
const MAX_SESSION_BYTES: u64 = 64 * 1024;
const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;
const DEP_INFO_SLOT: &str = "target/outputs/dep-info";
const METADATA_SLOT: &str = "target/outputs/metadata";
const RLIB_SLOT: &str = "target/outputs/rlib";
const STDOUT_SLOT: &str = "target/streams/stdout";
const STDERR_SLOT: &str = "target/streams/stderr";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeCompilerSession {
  version: u32,
  identity: String,
  /// Physical binding for this session file only. It never enters a reusable key.
  source_root_identity: String,
  class: NativeCompilerClass,
  capability_identity: String,
  toolchain_identity: String,
  compiler_process_environment_identity: String,
  execution_contract: String,
}

/// Exact result class and toolchain boundary for one native reuse session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeCompilerClass {
  name: String,
  platform: String,
  host_target: String,
  rustc_release: String,
  cargo_release: String,
}

impl NativeCompilerClass {
  fn capture(rustc_verbose_version: &str, cargo_verbose_version: &str) -> Self {
    Self {
      name: GRADUATED_NATIVE_CACHE_CLASS.to_string(),
      platform: format!(
        "{}-{}-{}",
        std::env::consts::FAMILY,
        std::env::consts::OS,
        std::env::consts::ARCH
      ),
      host_target: rustc_host_from_verbose(rustc_verbose_version),
      rustc_release: release_from_verbose(rustc_verbose_version, "rustc"),
      cargo_release: release_from_verbose(cargo_verbose_version, "cargo"),
    }
  }

  fn is_valid(&self) -> bool {
    self.name == GRADUATED_NATIVE_CACHE_CLASS
      && !self.platform.is_empty()
      && self.host_target != "unknown"
      && self.rustc_release != "unknown"
      && self.cargo_release != "unknown"
  }
}

/// Exact snapshot inputs needed to enable native reuse for an ordinary Cargo action.
pub(crate) struct DirectNativeCacheIdentity<'a> {
  pub(crate) source_root: &'a Path,
  pub(crate) rustc_version: &'a str,
  pub(crate) cargo_version: &'a str,
  pub(crate) toolchain_fingerprint: &'a str,
  pub(crate) capability_identity: &'a str,
  pub(crate) compiler_process_env_fingerprint: &'a str,
  pub(crate) wrapper_plan: CacheWrapperPlan,
  pub(crate) setup_bytes_hashed: u64,
  pub(crate) retain_event_evidence: bool,
}

/// Activation result for one ordinary Cargo action.
pub(crate) enum DirectNativeCacheSetup {
  Active(DirectNativeCacheRun),
  Bypassed(DirectCacheBypass),
}

/// Stable action-level reason that prevents cargo-rail from installing its compiler cache wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirectCacheBypass {
  DisabledByRequest,
  CargoCliConfiguration,
  ActionCompilerWrapper,
  ActionEnvironment,
  ForcedIncremental,
  ExplicitIncremental,
  CustomCargoProfile,
  ActiveCargoProfile,
  IdentityUnavailable,
  SccacheWrapper,
  ExistingCompilerWrapper,
  CustomSysroot,
  ConfiguredLinker,
  CargoConfiguration,
  BuildScriptObservations,
  ProcMacroObservations,
  ExternalSourceDigest,
  NoEligibleLibraryUnits,
  CapabilityUnavailable,
  ObservationDirectoryUnavailable,
  SessionUnavailable,
  WrapperExecutableUnavailable,
  SourceRootUnavailable,
}

impl DirectCacheBypass {
  pub(crate) const fn as_str(self) -> &'static str {
    match self {
      Self::DisabledByRequest => "native_cache_disabled_by_request",
      Self::CargoCliConfiguration => "cargo_cli_configuration_not_graduated",
      Self::ActionCompilerWrapper => "action_compiler_wrapper_preserved",
      Self::ActionEnvironment => "action_environment_not_graduated",
      Self::ForcedIncremental => "forced_incremental_compilation_preserved",
      Self::ExplicitIncremental => "explicit_incremental_compilation_preserved",
      Self::CustomCargoProfile => "custom_cargo_profile_preserved",
      Self::ActiveCargoProfile => "active_cargo_profile_preferred",
      Self::IdentityUnavailable => "native_cache_identity_unavailable",
      Self::SccacheWrapper => "sccache_wrapper_preserved",
      Self::ExistingCompilerWrapper => "existing_compiler_wrapper_preserved",
      Self::CustomSysroot => "custom_sysroot_not_graduated",
      Self::ConfiguredLinker => "configured_linker_not_graduated",
      Self::CargoConfiguration => "cargo_configuration_unmodeled",
      Self::BuildScriptObservations => "build_script_observations_unavailable",
      Self::ProcMacroObservations => "proc_macro_observations_unavailable",
      Self::ExternalSourceDigest => "external_source_digest_unavailable",
      Self::NoEligibleLibraryUnits => "native_cache_no_eligible_library_units",
      Self::CapabilityUnavailable => "native_cache_capability_unavailable",
      Self::ObservationDirectoryUnavailable => "native_cache_observation_directory_unavailable",
      Self::SessionUnavailable => "native_cache_session_unavailable",
      Self::WrapperExecutableUnavailable => "compiler_wrapper_executable_unavailable",
      Self::SourceRootUnavailable => "native_cache_source_root_unavailable",
    }
  }
}

/// Keeps the private session and its per-invocation evidence alive for one Cargo process.
pub(crate) struct DirectNativeCacheRun {
  observations: tempfile::TempDir,
  cargo_config: OsString,
  setup_bytes_hashed: u64,
}

#[derive(Clone)]
pub(crate) struct NativeCacheContext {
  session: PathBuf,
  source_root: PathBuf,
  observation_directory: PathBuf,
  retain_event_evidence: bool,
  capture_wrapper_diagnostics: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectNativeCacheContext {
  version: u32,
  source_root: String,
  retain_event_evidence: bool,
  capture_wrapper_diagnostics: bool,
}

static ACTIVE_CONTEXT: OnceLock<NativeCacheContext> = OnceLock::new();

pub(crate) const fn native_cache_class() -> &'static str {
  GRADUATED_NATIVE_CACHE_CLASS
}

pub(crate) const fn native_cache_execution_contract() -> &'static str {
  DIRECT_EXECUTION_CONTRACT
}

pub(crate) const fn native_cache_capability_schema_version() -> u32 {
  NATIVE_CACHE_CAPABILITY_SCHEMA_VERSION
}

/// Aggregate evidence emitted by one direct Cargo run.
#[derive(Debug, Default)]
pub(crate) struct DirectNativeCacheReport {
  pub(crate) hits: u64,
  pub(crate) misses: u64,
  pub(crate) bypasses: u64,
  pub(crate) setup_bytes_hashed: u64,
  pub(crate) bytes_hashed: u64,
  pub(crate) cache_bytes_read: u64,
  pub(crate) cache_bytes_written: u64,
  pub(crate) bytes_restored: u64,
  pub(crate) reasons: std::collections::BTreeMap<String, u64>,
  pub(crate) events: Vec<NativeCacheEventIdentity>,
  pub(crate) wrapper_diagnostics: Option<NativeCacheWrapperDiagnostics>,
}

#[derive(Clone, Copy, Debug, Default)]
struct NativeCacheMetrics {
  bytes_hashed: u64,
  cache_bytes_read: u64,
  cache_bytes_written: u64,
  bytes_restored: u64,
}

/// Stable per-unit evidence retained from one native-cache compiler event.
#[derive(Debug, Serialize)]
pub(crate) struct NativeCacheEventIdentity {
  schema_version: u32,
  unit_identity: Option<String>,
  outcome: CompilerCacheWrapperStatus,
  reason: String,
  action_key: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  unit: Option<NativeCacheUnitEvidence>,
}

/// Stable descriptor and exact candidate inputs for one compiler unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct NativeCacheUnitEvidence {
  descriptor: NativeCacheUnitDescriptor,
  identity_inputs: NativeCacheIdentityInputs,
  output_paths: Vec<ObservationPath>,
  observed_outputs: Vec<FileObservation>,
  #[serde(skip_serializing_if = "Option::is_none")]
  claimed_outputs: Option<Vec<FileObservation>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct NativeCacheUnitDescriptor {
  mode: CompilerMode,
  crate_name: Option<String>,
  crate_types: BTreeSet<String>,
  target_argument: Option<String>,
  emit_modes: BTreeSet<String>,
  test_mode: bool,
  crate_root: Option<ObservationPath>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct NativeCacheIdentityInputs {
  cfg: BTreeSet<String>,
  compiler_arguments: Vec<String>,
  declared_inputs: Vec<FileObservation>,
  dependency_artifacts: Vec<NativeDependencyArtifactIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct NativeDependencyArtifactIdentity {
  extern_name: String,
  artifact_name: String,
  content_digest: String,
  executable: bool,
  symlink_target: Option<String>,
}

#[derive(Serialize)]
struct NativeCacheKeyInputs<'a> {
  cfg: &'a BTreeSet<String>,
  compiler_arguments: Vec<String>,
  declared_inputs: &'a [FileObservation],
  dependency_artifacts: Vec<NativeDependencyArtifactKey<'a>>,
}

#[derive(Serialize)]
struct NativeDependencyArtifactKey<'a> {
  extern_name: &'a str,
  artifact_name: &'a str,
  content_digest: &'a str,
  executable: bool,
  symlink_target: &'a Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedNativeCacheUnitEvidence {
  schema_version: u32,
  unit_identity: String,
  unit: NativeCacheUnitEvidence,
}

impl DirectNativeCacheSetup {
  pub(crate) fn report(&self) -> Option<DirectNativeCacheReport> {
    match self {
      Self::Active(run) => Some(run.report()),
      Self::Bypassed(_) => None,
    }
  }

  pub(crate) fn bypass_reason(&self) -> Option<&'static str> {
    match self {
      Self::Active(_) => None,
      Self::Bypassed(reason) => Some(reason.as_str()),
    }
  }

  pub(crate) fn cargo_config_argument(&self) -> Option<&OsStr> {
    match self {
      Self::Active(run) => Some(&run.cargo_config),
      Self::Bypassed(_) => None,
    }
  }
}

impl DirectNativeCacheRun {
  fn report(&self) -> DirectNativeCacheReport {
    let mut report = DirectNativeCacheReport {
      setup_bytes_hashed: self.setup_bytes_hashed,
      ..DirectNativeCacheReport::default()
    };
    let units = native_cache_unit_evidence(self.observations.path());
    let mut wrapper_events = Vec::new();
    let directory = self.observations.path().join("native-cache-events");
    let Ok(entries) = fs::read_dir(directory) else {
      return report;
    };
    for entry in entries.filter_map(Result::ok) {
      let Ok(bytes) = fs::read(entry.path()) else {
        continue;
      };
      let Ok(event) = serde_json::from_slice::<OwnedNativeCacheEvent>(&bytes) else {
        continue;
      };
      if event.version != NATIVE_CACHE_RUN_EVENT_VERSION {
        continue;
      }
      match event.status {
        CompilerCacheWrapperStatus::Hit => report.hits = report.hits.saturating_add(1),
        CompilerCacheWrapperStatus::Miss => report.misses = report.misses.saturating_add(1),
        CompilerCacheWrapperStatus::Bypassed | CompilerCacheWrapperStatus::Disabled => {
          report.bypasses = report.bypasses.saturating_add(1);
        }
      }
      report.bytes_hashed = report.bytes_hashed.saturating_add(event.bytes_hashed);
      report.cache_bytes_read = report.cache_bytes_read.saturating_add(event.cache_bytes_read);
      report.cache_bytes_written = report.cache_bytes_written.saturating_add(event.cache_bytes_written);
      report.bytes_restored = report.bytes_restored.saturating_add(event.bytes_restored);
      *report.reasons.entry(event.reason.clone()).or_default() += 1;
      if let Some(trace) = event.wrapper_trace.clone() {
        wrapper_events.push(NativeCacheWrapperEventDiagnostics::new(
          event.candidate_key.clone(),
          event.status.as_str(),
          event.reason.clone(),
          trace,
        ));
      }
      let unit = event
        .candidate_key
        .as_ref()
        .and_then(|candidate| units.get(candidate))
        .map(|unit| {
          let mut unit = unit.clone();
          if !matches!(
            event.status,
            CompilerCacheWrapperStatus::Hit | CompilerCacheWrapperStatus::Miss
          ) {
            unit.claimed_outputs = None;
          }
          unit
        });
      report.events.push(NativeCacheEventIdentity {
        schema_version: NATIVE_CACHE_EVENT_EVIDENCE_VERSION,
        unit_identity: event.candidate_key,
        outcome: event.status,
        reason: event.reason,
        action_key: event.action_key,
        unit,
      });
    }
    report.events.sort_by(|left, right| {
      (&left.unit_identity, &left.action_key, &left.outcome, &left.reason).cmp(&(
        &right.unit_identity,
        &right.action_key,
        &right.outcome,
        &right.reason,
      ))
    });
    report.wrapper_diagnostics = NativeCacheWrapperDiagnostics::from_events(wrapper_events);
    report
  }
}

fn native_cache_unit_evidence(directory: &Path) -> BTreeMap<String, NativeCacheUnitEvidence> {
  let mut units = BTreeMap::new();
  let mut ambiguous = BTreeSet::new();
  let Ok(entries) = fs::read_dir(directory) else {
    return units;
  };
  for entry in entries.filter_map(Result::ok) {
    let path = entry.path();
    let Some(name) = path.file_name().and_then(OsStr::to_str) else {
      continue;
    };
    if !name.starts_with("rustc-") || path.extension() != Some(OsStr::new("json")) {
      continue;
    }
    let Some(raw) = fs::read(&path)
      .ok()
      .and_then(|bytes| serde_json::from_slice::<RawCompilerInvocation>(&bytes).ok())
    else {
      continue;
    };
    let Some(candidate) = raw
      .cache_wrapper
      .as_ref()
      .and_then(CompilerCacheWrapperMetadata::candidate_key)
      .map(str::to_string)
    else {
      continue;
    };
    if ambiguous.contains(&candidate) {
      continue;
    }
    let Ok(identity_inputs) = native_cache_identity_inputs(&raw) else {
      continue;
    };
    let evidence = NativeCacheUnitEvidence {
      descriptor: NativeCacheUnitDescriptor {
        mode: raw.mode,
        crate_name: raw.crate_name,
        crate_types: raw.crate_types,
        target_argument: raw.target_argument,
        emit_modes: raw.emit_modes,
        test_mode: raw.test_mode,
        crate_root: raw.declared_inputs.first().map(|input| input.path.clone()),
      },
      identity_inputs,
      output_paths: raw.emitted_outputs.iter().map(|output| output.path.clone()).collect(),
      observed_outputs: raw.emitted_outputs.clone(),
      claimed_outputs: Some(raw.emitted_outputs),
    };
    if units
      .insert(candidate.clone(), evidence.clone())
      .is_some_and(|previous| previous != evidence)
    {
      units.remove(&candidate);
      ambiguous.insert(candidate);
    }
  }
  let evidence_directory = directory.join(UNIT_EVIDENCE_DIRECTORY);
  let Ok(entries) = fs::read_dir(evidence_directory) else {
    return units;
  };
  for entry in entries.filter_map(Result::ok) {
    let Some(persisted) = fs::read(entry.path())
      .ok()
      .and_then(|bytes| serde_json::from_slice::<PersistedNativeCacheUnitEvidence>(&bytes).ok())
    else {
      continue;
    };
    if persisted.schema_version == NATIVE_CACHE_EVENT_EVIDENCE_VERSION
      && validate_candidate_key(&persisted.unit_identity).is_ok()
      && !ambiguous.contains(&persisted.unit_identity)
    {
      units.entry(persisted.unit_identity).or_insert(persisted.unit);
    }
  }
  units
}

fn retain_pre_execution_unit_evidence(
  directory: &Path,
  unit_identity: &str,
  observation: &RawCompilerInvocation,
  outputs: Option<&NativeOutputPaths>,
  source_root: &Path,
) {
  let Ok(identity_inputs) = native_cache_identity_inputs(observation) else {
    return;
  };
  let output_paths = outputs
    .into_iter()
    .flat_map(native_output_bindings)
    .map(|(_, _, path)| ObservationPath::capture(path, source_root, source_root))
    .collect::<Vec<_>>();
  let persisted = PersistedNativeCacheUnitEvidence {
    schema_version: NATIVE_CACHE_EVENT_EVIDENCE_VERSION,
    unit_identity: unit_identity.to_string(),
    unit: NativeCacheUnitEvidence {
      descriptor: NativeCacheUnitDescriptor {
        mode: observation.mode,
        crate_name: observation.crate_name.clone(),
        crate_types: observation.crate_types.clone(),
        target_argument: observation.target_argument.clone(),
        emit_modes: observation.emit_modes.clone(),
        test_mode: observation.test_mode,
        crate_root: observation.declared_inputs.first().map(|input| input.path.clone()),
      },
      identity_inputs,
      output_paths,
      observed_outputs: Vec::new(),
      claimed_outputs: None,
    },
  };
  let Ok(bytes) = serde_json::to_vec(&persisted) else {
    return;
  };
  let evidence_directory = directory.join(UNIT_EVIDENCE_DIRECTORY);
  if fs::create_dir_all(&evidence_directory).is_err() {
    return;
  }
  let name = format!("{}.json", ContentDigest::sha256(unit_identity.as_bytes()));
  let _ = crate::utils::write_file_atomic(&evidence_directory.join(name), &bytes);
}

/// Prepare an argument-scoped global wrapper for one ordinary Cargo check/build action.
pub(crate) fn prepare_direct_cargo_cache(identity: DirectNativeCacheIdentity<'_>) -> DirectNativeCacheSetup {
  if let Some(reason) = direct_cache_bypass_reason(identity.wrapper_plan) {
    return DirectNativeCacheSetup::Bypassed(reason);
  }
  let observations = match tempfile::Builder::new().prefix("cargo-rail-native-cargo-").tempdir() {
    Ok(directory) => directory,
    Err(_) => return DirectNativeCacheSetup::Bypassed(DirectCacheBypass::ObservationDirectoryUnavailable),
  };
  if NativeCompilerSession::write(
    observations.path(),
    identity.source_root,
    identity.rustc_version,
    identity.cargo_version,
    identity.capability_identity,
    identity.toolchain_fingerprint,
    identity.compiler_process_env_fingerprint,
    DIRECT_EXECUTION_CONTRACT,
  )
  .is_err()
  {
    return DirectNativeCacheSetup::Bypassed(DirectCacheBypass::SessionUnavailable);
  }
  let executable = match std::env::current_exe() {
    Ok(wrapper) => wrapper,
    Err(_) => return DirectNativeCacheSetup::Bypassed(DirectCacheBypass::WrapperExecutableUnavailable),
  };
  let wrapper = observations.path().join(DIRECT_WRAPPER_NAME);
  if create_direct_wrapper(&executable, &wrapper).is_err() {
    return DirectNativeCacheSetup::Bypassed(DirectCacheBypass::WrapperExecutableUnavailable);
  }
  let source_root = match crate::utils::canonicalize_existing(identity.source_root)
    .ok()
    .and_then(|root| root.to_str().map(str::to_string))
  {
    Some(root) => root,
    None => return DirectNativeCacheSetup::Bypassed(DirectCacheBypass::SourceRootUnavailable),
  };
  let context = DirectNativeCacheContext {
    version: 3,
    source_root,
    retain_event_evidence: identity.retain_event_evidence,
    capture_wrapper_diagnostics: crate::instrumentation::enabled(),
  };
  if serde_json::to_vec(&context)
    .ok()
    .and_then(|bytes| crate::utils::write_file_atomic(&observations.path().join(DIRECT_CONTEXT_FILE), &bytes).ok())
    .is_none()
  {
    return DirectNativeCacheSetup::Bypassed(DirectCacheBypass::SessionUnavailable);
  }
  let wrapper = match wrapper.to_str().and_then(|path| serde_json::to_string(path).ok()) {
    Some(wrapper) => wrapper,
    None => return DirectNativeCacheSetup::Bypassed(DirectCacheBypass::WrapperExecutableUnavailable),
  };
  DirectNativeCacheSetup::Active(DirectNativeCacheRun {
    observations,
    cargo_config: format!("build.rustc-wrapper={wrapper}").into(),
    setup_bytes_hashed: identity.setup_bytes_hashed,
  })
}

pub(crate) fn direct_cache_bypass_reason(wrapper_plan: CacheWrapperPlan) -> Option<DirectCacheBypass> {
  if !wrapper_plan.installs_cargo_rail() {
    return Some(match wrapper_plan {
      CacheWrapperPlan::PreserveSccache => DirectCacheBypass::SccacheWrapper,
      CacheWrapperPlan::PreserveExisting => DirectCacheBypass::ExistingCompilerWrapper,
      CacheWrapperPlan::DisabledPassThrough => return None,
    });
  }
  None
}

/// Return the stable action-level bypass for toolchain roots that are not part
/// of the captured compiler identity.
pub(crate) fn direct_target_configuration_bypass_reason(
  targets: &[crate::cargo::TargetIdentity],
) -> Option<DirectCacheBypass> {
  for target in targets.iter().filter(|target| target.is_build_target()) {
    if target.linker().is_some() {
      return Some(DirectCacheBypass::ConfiguredLinker);
    }
    if long_option_selected(target.rustflags(), "--sysroot") {
      return Some(DirectCacheBypass::CustomSysroot);
    }
  }
  None
}

fn long_option_selected(flags: &[String], name: &str) -> bool {
  flags
    .iter()
    .any(|flag| flag == name || flag.strip_prefix(name).is_some_and(|suffix| suffix.starts_with('=')))
}

#[cfg(unix)]
fn create_direct_wrapper(executable: &Path, wrapper: &Path) -> std::io::Result<()> {
  std::os::unix::fs::symlink(executable, wrapper)
}

#[cfg(windows)]
fn create_direct_wrapper(executable: &Path, wrapper: &Path) -> std::io::Result<()> {
  fs::copy(executable, wrapper).map(|_| ())
}

#[cfg(not(any(unix, windows)))]
fn create_direct_wrapper(_executable: &Path, _wrapper: &Path) -> std::io::Result<()> {
  Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "unsupported host"))
}

impl NativeCacheContext {
  pub(crate) fn activate(self) -> RailResult<()> {
    ACTIVE_CONTEXT
      .set(self)
      .map_err(|_| RailError::message("native compiler cache context was activated twice"))
  }

  pub(crate) fn from_environment() -> Option<Self> {
    Some(Self {
      session: std::env::var_os(SESSION_ENV).map(PathBuf::from)?,
      source_root: std::env::var_os(crate::compiler::wrapper::OBSERVATION_SOURCE_ROOT_ENV).map(PathBuf::from)?,
      observation_directory: std::env::var_os(crate::compiler::wrapper::OBSERVATION_DIRECTORY_ENV)
        .map(PathBuf::from)?,
      retain_event_evidence: false,
      capture_wrapper_diagnostics: false,
    })
  }

  pub(crate) fn from_direct_invocation()
  -> Option<(RailResult<Self>, crate::instrumentation::NativeCacheWrapperProcessStart)> {
    let invoked = PathBuf::from(std::env::args_os().next()?);
    if invoked.file_name() != Some(OsStr::new(DIRECT_WRAPPER_NAME)) {
      return None;
    }
    let started = crate::instrumentation::NativeCacheWrapperProcessStart::capture();
    Some((Self::load_direct(&invoked), started))
  }

  fn load_direct(invoked: &Path) -> RailResult<Self> {
    let invoked = if invoked.is_absolute() {
      invoked.to_path_buf()
    } else {
      std::env::current_dir()?.join(invoked)
    };
    let directory = invoked
      .parent()
      .ok_or_else(|| RailError::message("native compiler wrapper has no session directory"))?;
    let directory = crate::utils::canonicalize_existing(directory)?;
    let context_path = directory.join(DIRECT_CONTEXT_FILE);
    let metadata = fs::symlink_metadata(&context_path)?;
    if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) || metadata.len() > MAX_SESSION_BYTES {
      return Err(RailError::message(
        "native compiler cache context is not a bounded regular file",
      ));
    }
    let context: DirectNativeCacheContext = serde_json::from_slice(&fs::read(context_path)?)?;
    if context.version != 3 {
      return Err(RailError::message(
        "native compiler cache context has an incompatible schema",
      ));
    }
    let source_root = PathBuf::from(context.source_root);
    if !source_root.is_absolute() || source_root.as_os_str().as_encoded_bytes().contains(&0) {
      return Err(RailError::message(
        "native compiler cache context has an invalid source root",
      ));
    }
    Ok(Self {
      session: directory.join("native-cache-session").join(SESSION_FILE),
      source_root,
      observation_directory: directory,
      retain_event_evidence: context.retain_event_evidence,
      capture_wrapper_diagnostics: context.capture_wrapper_diagnostics,
    })
  }

  pub(crate) fn captures_wrapper_diagnostics(&self) -> bool {
    self.capture_wrapper_diagnostics
  }
}

fn active_context() -> Option<&'static NativeCacheContext> {
  ACTIVE_CONTEXT.get()
}

impl NativeCompilerSession {
  // The identity fields intentionally mirror the serialized session schema at
  // both call sites; grouping them would add an otherwise unused construction type.
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn write(
    directory: &Path,
    source_root: &Path,
    rustc_verbose_version: &str,
    cargo_verbose_version: &str,
    capability_identity: &str,
    toolchain_identity: &str,
    compiler_process_environment_identity: &str,
    execution_contract: &str,
  ) -> RailResult<PathBuf> {
    let source_root = crate::utils::canonicalize_existing(source_root)?;
    let source_root_identity = path_identity(&source_root)?;
    let class = NativeCompilerClass::capture(rustc_verbose_version, cargo_verbose_version);
    let identity = session_identity(
      &class,
      capability_identity,
      toolchain_identity,
      compiler_process_environment_identity,
      execution_contract,
    )?;
    let session = Self {
      version: 4,
      identity,
      source_root_identity,
      class,
      capability_identity: capability_identity.to_string(),
      toolchain_identity: toolchain_identity.to_string(),
      compiler_process_environment_identity: compiler_process_environment_identity.to_string(),
      execution_contract: execution_contract.to_string(),
    };
    session.validate_object()?;
    if session.class.is_valid() {
      LocalCas::open()?;
    }
    let session_directory = directory.join("native-cache-session");
    fs::create_dir(&session_directory)?;
    let path = session_directory.join(SESSION_FILE);
    crate::utils::write_file_atomic(&path, &serde_json::to_vec(&session)?)?;
    Ok(path)
  }

  fn load(path: &Path, source_root: &Path) -> RailResult<Self> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) || metadata.len() > MAX_SESSION_BYTES {
      return Err(RailError::message(
        "native compiler cache session is not a bounded regular file",
      ));
    }
    let session: Self = serde_json::from_slice(&fs::read(path)?)?;
    session.validate_object()?;
    if session.source_root_identity != path_identity(source_root)? {
      return Err(RailError::message("native compiler cache session source root changed"));
    }
    Ok(session)
  }

  fn validate_object(&self) -> RailResult<()> {
    if self.version != 4 {
      return Err(RailError::message(
        "native compiler cache session has an incompatible schema",
      ));
    }
    for digest in [
      &self.identity,
      &self.source_root_identity,
      &self.capability_identity,
      &self.toolchain_identity,
      &self.compiler_process_environment_identity,
    ] {
      validate_sha256(digest)?;
    }
    if !matches!(
      self.execution_contract.as_str(),
      DIAGNOSTIC_EXECUTION_CONTRACT | DIRECT_EXECUTION_CONTRACT
    ) {
      return Err(RailError::message(
        "native compiler cache session has an unsupported execution contract",
      ));
    }
    let expected = session_identity(
      &self.class,
      &self.capability_identity,
      &self.toolchain_identity,
      &self.compiler_process_environment_identity,
      &self.execution_contract,
    )?;
    if self.identity != expected {
      return Err(RailError::message(
        "native compiler cache session identity does not match its inputs",
      ));
    }
    if !self.class.is_valid() {
      return Err(RailError::message(
        "native compiler cache session has an invalid compiler class",
      ));
    }
    Ok(())
  }
}

/// One output slot bound to rustc's current invocation paths only after CAS verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeCompilerOutput {
  role: String,
  slot: String,
  content_digest: String,
  bytes: u64,
}

/// Post-compile evidence retained behind a non-authorizing candidate index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeCompilerValidation {
  version: u32,
  candidate_key: String,
  action_key: String,
  session_identity: String,
  class: NativeCompilerClass,
  observation: RawCompilerInvocation,
  outputs: Vec<NativeCompilerOutput>,
  stdout_digest: String,
  stderr_digest: String,
}

impl NativeCompilerValidation {
  fn new(
    session: &NativeCompilerSession,
    observation: RawCompilerInvocation,
    outputs: Vec<NativeCompilerOutput>,
    stdout_digest: String,
    stderr_digest: String,
  ) -> RailResult<Self> {
    let candidate_key = candidate_key(&session.identity, &session.class, &observation)?;
    let action_key = action_key(&session.identity, &session.class, &observation)?;
    let validation = Self {
      version: 3,
      candidate_key,
      action_key,
      session_identity: session.identity.clone(),
      class: session.class.clone(),
      observation,
      outputs,
      stdout_digest,
      stderr_digest,
    };
    validation.validate_object()?;
    Ok(validation)
  }

  pub(crate) fn candidate_key(&self) -> &str {
    &self.candidate_key
  }

  pub(crate) fn action_key(&self) -> &str {
    &self.action_key
  }

  pub(crate) fn result_digest(&self, output_manifest: &str) -> String {
    result_digest(&self.action_key, output_manifest)
  }

  pub(crate) fn cas_output_bindings(&self) -> impl Iterator<Item = (&str, &str, u64)> {
    self
      .outputs
      .iter()
      .map(|output| (output.slot.as_str(), output.content_digest.as_str(), output.bytes))
  }

  pub(crate) fn cas_stream_bindings(&self) -> [(&str, &str); 2] {
    [
      (STDOUT_SLOT, self.stdout_digest.as_str()),
      (STDERR_SLOT, self.stderr_digest.as_str()),
    ]
  }

  pub(crate) fn validate_object(&self) -> RailResult<()> {
    if self.version != 3 {
      return Err(RailError::message(
        "native compiler observation has an incompatible schema",
      ));
    }
    validate_identity(&self.candidate_key, CANDIDATE_KEY_PREFIX)?;
    validate_identity(&self.action_key, ACTION_KEY_PREFIX)?;
    for digest in [&self.session_identity, &self.stdout_digest, &self.stderr_digest] {
      validate_sha256(digest)?;
    }
    if !self.class.is_valid()
      || self.observation.version != 4
      || !self.observation.success
      || self.observation.mode != CompilerMode::Rustc
      || self.observation.compiler_arguments.is_empty()
      || invocation_bypass_reason(&self.observation, true, &self.class.host_target).is_some()
      || !output_contract_matches(&self.outputs, self.observation.emit_modes.contains("link"))
      || self
        .outputs
        .iter()
        .any(|output| validate_sha256(&output.content_digest).is_err())
      || !complete_library_observation(&self.observation)
      || !outputs_match_observation(&self.outputs, &self.observation.emitted_outputs)
    {
      return Err(RailError::message(
        "native compiler observation is outside the graduated class",
      ));
    }
    for output in &self.outputs {
      if output.bytes == 0 {
        return Err(RailError::message(
          "native compiler observation contains an empty compiler output",
        ));
      }
    }
    for file in self
      .observation
      .declared_inputs
      .iter()
      .chain(&self.observation.observed_reads)
      .chain(self.observation.dependency_artifacts.iter().map(|(_, file)| file))
      .chain(&self.observation.emitted_outputs)
    {
      validate_file_observation(file)?;
    }
    for environment in &self.observation.environment_reads {
      if environment.name.is_empty()
        || environment.name.as_bytes().contains(&0)
        || environment.secret_capability
        || environment
          .value_digest
          .as_deref()
          .is_some_and(|digest| validate_sha256(digest).is_err())
      {
        return Err(RailError::message(
          "native compiler observation contains an unsupported environment read",
        ));
      }
    }
    if candidate_key(&self.session_identity, &self.class, &self.observation)? != self.candidate_key
      || action_key(&self.session_identity, &self.class, &self.observation)? != self.action_key
    {
      return Err(RailError::message(
        "native compiler observation identity does not match its inputs",
      ));
    }
    Ok(())
  }
}

pub(crate) fn result_digest(action_key: &str, output_manifest: &str) -> String {
  let mut framed = Vec::from(&b"cargo-rail-native-compiler-result\0"[..]);
  append_frame(&mut framed, b"version", &1_u32.to_le_bytes());
  append_frame(&mut framed, b"action", action_key.as_bytes());
  append_frame(&mut framed, b"outputs", output_manifest.as_bytes());
  crate::instrumentation::record_hash(framed.len());
  format!("compiler-result-v1-sha256-{}", ContentDigest::sha256(&framed))
}

fn session_identity(
  class: &NativeCompilerClass,
  capability_identity: &str,
  toolchain_identity: &str,
  compiler_process_environment_identity: &str,
  execution_contract: &str,
) -> RailResult<String> {
  let class = serde_json::to_vec(class)?;
  Ok(sha256_identity(
    "sha256:",
    b"cargo-rail-native-compiler-session\0",
    &[
      (b"version", &4_u32.to_le_bytes()),
      (
        b"toolchain-capability-contract",
        &NATIVE_CACHE_IDENTITY_CONTRACT_VERSION.to_le_bytes(),
      ),
      (b"class", &class),
      (b"capability", capability_identity.as_bytes()),
      (b"toolchain", toolchain_identity.as_bytes()),
      (
        b"compiler-process-environment",
        compiler_process_environment_identity.as_bytes(),
      ),
      (b"execution-contract", execution_contract.as_bytes()),
    ],
  ))
}

fn candidate_key(
  session_identity: &str,
  class: &NativeCompilerClass,
  observation: &RawCompilerInvocation,
) -> RailResult<String> {
  let class = serde_json::to_vec(class)?;
  let identity_inputs = native_cache_key_inputs(observation)?;
  let pre_execution = serde_json::to_vec(&(
    &observation.mode,
    &observation.crate_name,
    &observation.crate_types,
    &observation.target_argument,
    &observation.emit_modes,
    observation.test_mode,
    &identity_inputs,
  ))?;
  Ok(sha256_identity(
    CANDIDATE_KEY_PREFIX,
    b"cargo-rail-native-compiler-candidate\0",
    &[
      (b"version", &3_u32.to_le_bytes()),
      (b"session", session_identity.as_bytes()),
      (b"class", &class),
      (b"pre-execution", &pre_execution),
    ],
  ))
}

fn action_key(
  session_identity: &str,
  class: &NativeCompilerClass,
  observation: &RawCompilerInvocation,
) -> RailResult<String> {
  let candidate = candidate_key(session_identity, class, observation)?;
  let discovered = serde_json::to_vec(&(&observation.observed_reads, &observation.environment_reads))?;
  Ok(sha256_identity(
    ACTION_KEY_PREFIX,
    b"cargo-rail-native-compiler-action\0",
    &[
      (b"version", &3_u32.to_le_bytes()),
      (b"candidate", candidate.as_bytes()),
      (b"discovered-inputs", &discovered),
    ],
  ))
}

fn native_cache_identity_inputs(observation: &RawCompilerInvocation) -> RailResult<NativeCacheIdentityInputs> {
  let identity = native_cache_key_inputs(observation)?;
  Ok(NativeCacheIdentityInputs {
    cfg: identity.cfg.clone(),
    compiler_arguments: identity.compiler_arguments,
    declared_inputs: identity.declared_inputs.to_vec(),
    dependency_artifacts: identity
      .dependency_artifacts
      .into_iter()
      .map(|artifact| NativeDependencyArtifactIdentity {
        extern_name: artifact.extern_name.to_string(),
        artifact_name: artifact.artifact_name.to_string(),
        content_digest: artifact.content_digest.to_string(),
        executable: artifact.executable,
        symlink_target: artifact.symlink_target.clone(),
      })
      .collect(),
  })
}

fn native_cache_key_inputs(observation: &RawCompilerInvocation) -> RailResult<NativeCacheKeyInputs<'_>> {
  let dependency_artifacts = observation
    .dependency_artifacts
    .iter()
    .map(|(extern_name, artifact)| {
      Ok(NativeDependencyArtifactKey {
        extern_name,
        artifact_name: observation_path_basename(&artifact.path)
          .ok_or_else(|| RailError::message("native dependency artifact has no file name"))?,
        content_digest: &artifact.content_digest,
        executable: artifact.executable,
        symlink_target: &artifact.symlink_target,
      })
    })
    .collect::<RailResult<Vec<_>>>()?;
  let compiler_arguments = cache_key_compiler_arguments(&observation.compiler_arguments, &dependency_artifacts)?;
  Ok(NativeCacheKeyInputs {
    cfg: &observation.cfg,
    compiler_arguments,
    declared_inputs: &observation.declared_inputs,
    dependency_artifacts,
  })
}

fn cache_key_compiler_arguments(
  arguments: &[String],
  dependencies: &[NativeDependencyArtifactKey<'_>],
) -> RailResult<Vec<String>> {
  let mut dependency_names = BTreeMap::new();
  for dependency in dependencies {
    if dependency_names
      .insert(dependency.extern_name, dependency.artifact_name)
      .is_some()
    {
      return Err(RailError::message("native compiler invocation repeats an extern name"));
    }
  }
  let relocatable_directories = cache_key_relocatable_directories(arguments);

  let mut identity = Vec::with_capacity(arguments.len());
  let mut index = 0usize;
  while index < arguments.len() {
    let argument = &arguments[index];
    let next = arguments.get(index + 1);
    match argument.as_str() {
      "--out-dir" | "--output" => {
        next.ok_or_else(|| RailError::message("native compiler output directory is missing"))?;
        identity.push(argument.clone());
        identity.push("\0cargo-rail-native-output-directory".to_string());
        index += 2;
      }
      "--emit" => {
        let value = next.ok_or_else(|| RailError::message("native compiler emit value is missing"))?;
        identity.push(argument.clone());
        identity.push(cache_key_emit_argument(value)?);
        index += 2;
      }
      "--extern" => {
        let value = next.ok_or_else(|| RailError::message("native compiler extern value is missing"))?;
        identity.push(argument.clone());
        identity.push(cache_key_extern_argument(value, &dependency_names)?);
        index += 2;
      }
      "-L" => {
        let value = next.ok_or_else(|| RailError::message("native compiler library search value is missing"))?;
        identity.push(argument.clone());
        identity.push(cache_key_library_search_argument(value, &relocatable_directories));
        index += 2;
      }
      _ if argument.starts_with("--out-dir=") || argument.starts_with("--output=") => {
        let (option, _) = argument
          .split_once('=')
          .ok_or_else(|| RailError::message("native compiler output directory is invalid"))?;
        identity.push(format!("{option}=\0cargo-rail-native-output-directory"));
        index += 1;
      }
      _ if argument.starts_with("--emit=") => {
        identity.push(format!(
          "--emit={}",
          cache_key_emit_argument(argument.trim_start_matches("--emit="))?
        ));
        index += 1;
      }
      _ if argument.starts_with("--extern=") => {
        identity.push(format!(
          "--extern={}",
          cache_key_extern_argument(argument.trim_start_matches("--extern="), &dependency_names)?
        ));
        index += 1;
      }
      _ if argument.starts_with("-Ldependency=") => {
        identity.push(format!(
          "-L{}",
          cache_key_library_search_argument(argument.trim_start_matches("-L"), &relocatable_directories)
        ));
        index += 1;
      }
      _ => {
        identity.push(argument.clone());
        index += 1;
      }
    }
  }
  Ok(identity)
}

fn cache_key_relocatable_directories(arguments: &[String]) -> BTreeSet<String> {
  let mut directories = BTreeSet::new();
  let mut index = 0usize;
  while index < arguments.len() {
    match arguments[index].as_str() {
      "--out-dir" | "--output" => {
        if let Some(path) = arguments.get(index + 1) {
          directories.insert(portable_directory_identity(path));
        }
        index += 2;
      }
      argument if argument.starts_with("--out-dir=") || argument.starts_with("--output=") => {
        if let Some((_, path)) = argument.split_once('=') {
          directories.insert(portable_directory_identity(path));
        }
        index += 1;
      }
      _ => index += 1,
    }
  }
  directories
}

fn cache_key_emit_argument(value: &str) -> RailResult<String> {
  value
    .split(',')
    .map(|emit| {
      let Some((mode, path)) = emit.split_once('=') else {
        return Ok(emit.to_string());
      };
      let file_name =
        portable_path_basename(path).ok_or_else(|| RailError::message("native compiler output has no file name"))?;
      Ok(format!("{mode}=\0cargo-rail-native-output:{file_name}"))
    })
    .collect::<RailResult<Vec<_>>>()
    .map(|emits| emits.join(","))
}

fn cache_key_extern_argument(value: &str, dependencies: &BTreeMap<&str, &str>) -> RailResult<String> {
  let Some((name, path)) = value.split_once('=') else {
    return Ok(value.to_string());
  };
  let artifact_name = dependencies
    .get(name)
    .ok_or_else(|| RailError::message("native compiler extern is missing exact artifact evidence"))?;
  if portable_path_basename(path) != Some(*artifact_name) {
    return Err(RailError::message(
      "native compiler extern path disagrees with its exact artifact evidence",
    ));
  }
  Ok(format!("{name}=\0cargo-rail-native-dependency:{artifact_name}"))
}

fn cache_key_library_search_argument(value: &str, relocatable_directories: &BTreeSet<String>) -> String {
  if value
    .strip_prefix("dependency=")
    .is_some_and(|path| relocatable_directories.contains(&portable_directory_identity(path)))
  {
    "dependency=\0cargo-rail-native-dependency-search".to_string()
  } else {
    value.to_string()
  }
}

fn portable_directory_identity(path: &str) -> String {
  path.replace('\\', "/").trim_end_matches('/').to_string()
}

fn observation_path_basename(path: &ObservationPath) -> Option<&str> {
  match path {
    ObservationPath::Repository(path) | ObservationPath::Host(path) => portable_path_basename(path),
  }
}

fn portable_path_basename(path: &str) -> Option<&str> {
  path.rsplit(['/', '\\']).find(|component| !component.is_empty())
}

fn sha256_identity(prefix: &str, domain: &[u8], frames: &[(&[u8], &[u8])]) -> String {
  let mut framed = Vec::from(domain);
  for (tag, value) in frames {
    append_frame(&mut framed, tag, value);
  }
  crate::instrumentation::record_hash(framed.len());
  format!("{prefix}{}", ContentDigest::sha256(&framed))
}

fn path_identity(path: &Path) -> RailResult<String> {
  let path = crate::utils::canonicalize_existing(path)?;
  Ok(sha256_identity(
    "sha256:",
    b"cargo-rail-native-compiler-source-root\0",
    &[(b"path", path.as_os_str().as_encoded_bytes())],
  ))
}

fn release_from_verbose(verbose: &str, program: &str) -> String {
  verbose
    .lines()
    .next()
    .and_then(|line| line.strip_prefix(program))
    .map(str::trim)
    .and_then(|rest| rest.split_ascii_whitespace().next())
    .unwrap_or("unknown")
    .to_string()
}

fn host_from_verbose(verbose: &str) -> String {
  verbose
    .lines()
    .find_map(|line| line.strip_prefix("host:"))
    .map(str::trim)
    .filter(|host| !host.is_empty())
    .unwrap_or("unknown")
    .to_string()
}

fn rustc_host_from_verbose(verbose: &str) -> String {
  host_from_verbose(verbose)
}

fn validate_file_observation(file: &FileObservation) -> RailResult<()> {
  validate_sha256(&file.content_digest)?;
  if file.symlink_target.is_some() {
    return Err(RailError::message(
      "native compiler observation contains a symlink input or output",
    ));
  }
  match &file.path {
    ObservationPath::Repository(path) => {
      crate::source::RepositoryPath::new(Path::new(path))?;
    }
    ObservationPath::Host(path) => {
      if !Path::new(path).is_absolute() || path.as_bytes().contains(&0) {
        return Err(RailError::message(
          "native compiler observation contains an invalid host path",
        ));
      }
    }
  }
  Ok(())
}

fn complete_library_observation(observation: &RawCompilerInvocation) -> bool {
  let [declared] = observation.declared_inputs.as_slice() else {
    return false;
  };
  observation.observed_reads.iter().any(|observed| observed == declared)
    && observation
      .dependency_artifacts
      .iter()
      .all(|(_, artifact)| matches!(&artifact.path, ObservationPath::Repository(_)))
}

fn output_contract_matches(outputs: &[NativeCompilerOutput], includes_rlib: bool) -> bool {
  let expected = if includes_rlib {
    &[
      ("dep_info", DEP_INFO_SLOT),
      ("metadata", METADATA_SLOT),
      ("rlib", RLIB_SLOT),
    ][..]
  } else {
    &[("dep_info", DEP_INFO_SLOT), ("metadata", METADATA_SLOT)][..]
  };
  outputs.len() == expected.len()
    && outputs
      .iter()
      .zip(expected)
      .all(|(output, (role, slot))| output.role == *role && output.slot == *slot)
}

fn outputs_match_observation(outputs: &[NativeCompilerOutput], observed: &[FileObservation]) -> bool {
  if !matches!(outputs.len(), 2 | 3) || observed.len() != outputs.len() {
    return false;
  }
  let mut matched = BTreeSet::new();
  for output in observed {
    if output.executable || output.symlink_target.is_some() {
      return false;
    }
    let Some((index, _)) = outputs.iter().enumerate().find(|(index, expected)| {
      !matched.contains(index)
        && match expected.role.as_str() {
          "dep_info" => output.path.resolve(Path::new("/")).extension() == Some(OsStr::new("d")),
          "metadata" => output.path.resolve(Path::new("/")).extension() == Some(OsStr::new("rmeta")),
          "rlib" => output.path.resolve(Path::new("/")).extension() == Some(OsStr::new("rlib")),
          _ => false,
        }
        && output.content_digest == expected.content_digest
    }) else {
      return false;
    };
    matched.insert(index);
  }
  matched.len() == outputs.len()
}

fn invocation_bypass_reason(
  observation: &RawCompilerInvocation,
  complete: bool,
  host_target: &str,
) -> Option<&'static str> {
  if observation.mode != CompilerMode::Rustc {
    return Some("rustdoc_not_graduated");
  }
  if observation
    .target_argument
    .as_deref()
    .is_some_and(|target| target != host_target)
  {
    return Some("cross_target_not_graduated");
  }
  if observation.test_mode {
    return Some("test_compilation_not_graduated");
  }
  if observation.crate_types.contains("proc-macro") {
    return Some("proc_macro_not_graduated");
  }
  if observation
    .crate_types
    .iter()
    .any(|kind| matches!(kind.as_str(), "dylib" | "cdylib" | "staticlib"))
  {
    return Some("linker_producing_crate_type_not_graduated");
  }
  if observation.crate_types.contains("bin") {
    return Some(if observation.crate_name.as_deref() == Some("build_script_build") {
      "build_script_not_graduated"
    } else {
      "binary_not_graduated"
    });
  }
  if observation.crate_types != BTreeSet::from(["lib".to_string()]) {
    return Some("compiler_crate_type_not_graduated");
  }
  let metadata = BTreeSet::from(["dep-info".to_string(), "metadata".to_string()]);
  let metadata_and_rlib = BTreeSet::from(["dep-info".to_string(), "link".to_string(), "metadata".to_string()]);
  if observation.emit_modes != metadata && observation.emit_modes != metadata_and_rlib {
    return Some("compiler_emit_mode_not_graduated");
  }
  if observation.compiler_arguments.iter().any(|argument| argument == "-") {
    return Some("compiler_stdin_not_graduated");
  }
  if observation.compiler_arguments.iter().any(|argument| {
    argument == "-l"
      || argument.starts_with("-l")
      || argument.starts_with("-Lnative")
      || argument.starts_with("-L") && argument.contains("native=")
      || argument.contains("linker=")
      || argument.contains("link-arg=")
      || argument.contains("link-args=")
  }) || observation.compiler_arguments.windows(2).any(|pair| {
    pair[0] == "-L" && pair[1].starts_with("native=")
      || pair[0] == "-C"
        && matches!(
          pair[1].split_once('=').map(|(name, _)| name),
          Some("linker" | "link-arg" | "link-args")
        )
  }) {
    return Some("native_linking_not_graduated");
  }
  if observation
    .compiler_arguments
    .iter()
    .any(|argument| argument.contains("incremental="))
  {
    return Some("incremental_compilation_not_graduated");
  }
  let remap_count = observation
    .compiler_arguments
    .iter()
    .filter(|argument| argument.as_str() == "--remap-path-prefix=repository:=/cargo-rail/workspace")
    .count();
  let maximum_remap_count = if cfg!(windows) { 4 } else { 2 };
  if !(1..=maximum_remap_count).contains(&remap_count) {
    return Some("compiler_path_remap_contract_not_graduated");
  }
  if unsupported_compiler_argument(&observation.compiler_arguments) {
    return Some("compiler_flag_not_graduated");
  }
  if observation.dependency_artifacts.iter().any(|(_, artifact)| {
    !matches!(
      artifact
        .path
        .resolve(Path::new("/"))
        .extension()
        .and_then(OsStr::to_str),
      Some("rmeta" | "rlib")
    )
  }) {
    return Some("dependency_artifact_class_not_graduated");
  }
  if observation
    .environment_reads
    .iter()
    .any(|environment| environment.secret_capability)
  {
    return Some("secret_compiler_environment");
  }
  if !observation.bypasses.is_empty() {
    return Some("compiler_inputs_incomplete");
  }
  if observation.declared_inputs.is_empty() {
    return Some("declared_compiler_inputs_unavailable");
  }
  let expected_outputs = 2 + usize::from(observation.emit_modes.contains("link"));
  if complete && (observation.observed_reads.is_empty() || observation.emitted_outputs.len() != expected_outputs) {
    return Some("complete_compiler_observation_unavailable");
  }
  None
}

fn unsupported_compiler_argument(arguments: &[String]) -> bool {
  let mut index = 0usize;
  let mut source_inputs = 0usize;
  while index < arguments.len() {
    let argument = arguments[index].as_str();
    let next = arguments.get(index + 1).map(String::as_str);
    let consumes_next = match argument {
      "--crate-name" | "--crate-type" | "--emit" | "--out-dir" | "--target" | "--edition" | "--error-format"
      | "--json" | "--cfg" | "--check-cfg" | "--cap-lints" | "--color" | "--diagnostic-width" | "--allow"
      | "--warn" | "--deny" | "--forbid" => next.is_some(),
      "--extern" => next.is_some_and(|value| value.contains('=')),
      "-L" => next.is_some_and(|value| value.starts_with("dependency=")),
      "-C" => next.is_some_and(supported_codegen_option),
      "-Z" => next.is_some_and(supported_unstable_option),
      "-A" | "-W" | "-D" | "-F" => next.is_some(),
      _ if argument.starts_with("--crate-name=")
        || argument == "--crate-type=lib"
        || argument.starts_with("--emit=")
        || argument.starts_with("--out-dir=")
        || argument.starts_with("--target=")
        || argument.starts_with("--edition=")
        || argument.starts_with("--error-format=")
        || argument.starts_with("--json=")
        || argument.starts_with("--cfg=")
        || argument.starts_with("--check-cfg=")
        || argument.starts_with("--cap-lints=")
        || argument.starts_with("--color=")
        || argument.starts_with("--diagnostic-width=")
        || argument.starts_with("--allow=")
        || argument.starts_with("--warn=")
        || argument.starts_with("--deny=")
        || argument.starts_with("--forbid=")
        || argument == "--remap-path-prefix=repository:=/cargo-rail/workspace"
        || argument.starts_with("--extern=") && argument.contains('=')
        || argument.starts_with("-Ldependency=")
        || argument.starts_with("-A") && argument.len() > 2
        || argument.starts_with("-W") && argument.len() > 2
        || argument.starts_with("-D") && argument.len() > 2
        || argument.starts_with("-F") && argument.len() > 2 =>
      {
        false
      }
      _ if argument.starts_with("-C") && argument.len() > 2 => {
        if !supported_codegen_option(argument.trim_start_matches("-C")) {
          return true;
        }
        false
      }
      _ if argument.starts_with("-Z") && argument.len() > 2 => {
        if !supported_unstable_option(argument.trim_start_matches("-Z")) {
          return true;
        }
        false
      }
      _ if !argument.starts_with('-') && argument.ends_with(".rs") => {
        source_inputs += 1;
        false
      }
      _ => return true,
    };
    if consumes_next && next.is_none() {
      return true;
    }
    index += usize::from(consumes_next) + 1;
  }
  source_inputs != 1
}

fn supported_unstable_option(option: &str) -> bool {
  let Some(("codegen-backend", backend)) = option.split_once('=') else {
    return false;
  };
  !backend.is_empty()
    && !backend.contains('/')
    && !backend.contains('\\')
    && !backend.contains(':')
    && backend
      .as_bytes()
      .iter()
      .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_' || *byte == b'-')
}

fn supported_codegen_option(option: &str) -> bool {
  matches!(
    option.split_once('=').map_or(option, |(name, _)| name),
    "metadata"
      | "extra-filename"
      | "embed-bitcode"
      | "debuginfo"
      | "split-debuginfo"
      | "opt-level"
      | "debug-assertions"
      | "overflow-checks"
      | "panic"
      | "codegen-units"
      | "linker-plugin-lto"
      | "strip"
  )
}

pub(crate) fn metadata_from_environment() -> Option<CompilerCacheWrapperMetadata> {
  let encoded = std::env::var_os(DISPOSITION_ENV)?;
  let encoded = encoded.to_str()?;
  (encoded.len() <= MAX_SESSION_BYTES as usize)
    .then(|| serde_json::from_str(encoded).ok())
    .flatten()
}

/// Result of one outer-wrapper cache decision.
///
/// This value is immediately matched once. Keeping the recorder inline avoids
/// adding a heap allocation to every eligible cold compiler invocation.
#[allow(clippy::large_enum_variant)]
pub(crate) enum OuterCacheAction {
  /// Verified compiler outputs and streams were restored without running rustc.
  Hit(i32),
  /// Run the compiler once and store only its verified exact outputs.
  Store {
    recorder: InvocationRecorder,
    cache_bytes_read: u64,
  },
  /// Execute the original invocation unchanged.
  Execute,
}

/// Attempt native reuse and configure the cold child without changing Cargo's wrapper order.
///
/// `arguments` starts with the rustc executable because `program` is Cargo's
/// workspace-wrapper slot. A returned code means verified outputs and streams
/// were already restored; `Execute` preserves the ordinary child execution.
pub(crate) fn configure_outer(
  program: &OsStr,
  arguments: &[OsString],
  command: &mut Command,
  trace: &mut NativeCacheWrapperTrace,
) -> OuterCacheAction {
  command.env_remove(DISPOSITION_ENV);
  let Some(context) = active_context() else {
    return OuterCacheAction::Execute;
  };

  let diagnostic_wrapper = is_diagnostic_workspace_wrapper(program);
  if std::env::var_os("RUSTC_FORCE_INCREMENTAL").is_some() {
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      "forced_incremental_compilation_not_graduated",
      None,
      0,
      diagnostic_wrapper,
      trace,
    );
    return OuterCacheAction::Execute;
  }
  let invocation = if diagnostic_wrapper {
    arguments
      .split_first()
      .map(|(rustc, compiler_arguments)| (rustc.as_os_str(), compiler_arguments))
  } else {
    Some((program, arguments))
  };
  let Some((rustc, compiler_arguments)) = invocation else {
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      "compiler_argv_unavailable",
      None,
      0,
      diagnostic_wrapper,
      trace,
    );
    return OuterCacheAction::Execute;
  };
  let source_root = &context.source_root;
  let observation_directory = &context.observation_directory;
  let session_phase = trace.start(NativeCacheWrapperPhase::SessionLoad);
  let session = NativeCompilerSession::load(&context.session, source_root);
  trace.finish(session_phase, NativeCacheWrapperWork::default());
  let session = match session {
    Ok(session) => session,
    Err(_) => {
      configure_cold(
        command,
        CompilerCacheWrapperStatus::Bypassed,
        "native_cache_session_unavailable",
        None,
        0,
        diagnostic_wrapper,
        trace,
      );
      return OuterCacheAction::Execute;
    }
  };
  let input_capture_phase = trace.start(NativeCacheWrapperPhase::ArgumentNormalizationInputCapture);
  let original_current_dir = match std::env::current_dir() {
    Ok(directory) => directory,
    Err(_) => {
      configure_cold(
        command,
        CompilerCacheWrapperStatus::Bypassed,
        "compiler_working_directory_unavailable",
        None,
        0,
        diagnostic_wrapper,
        trace,
      );
      return OuterCacheAction::Execute;
    }
  };
  let portable_arguments = match portable_compiler_arguments(compiler_arguments, &original_current_dir, source_root) {
    Ok(arguments) => arguments,
    Err(reason) => {
      configure_cold(
        command,
        CompilerCacheWrapperStatus::Bypassed,
        reason,
        None,
        0,
        diagnostic_wrapper,
        trace,
      );
      return OuterCacheAction::Execute;
    }
  };
  let recorder = match crate::compiler::observation::begin_invocation_in(
    observation_directory,
    source_root,
    source_root,
    rustc,
    &portable_arguments,
  ) {
    Ok(recorder) => recorder,
    Err(_) => {
      configure_cold(
        command,
        CompilerCacheWrapperStatus::Bypassed,
        "compiler_invocation_observation_unavailable",
        None,
        0,
        diagnostic_wrapper,
        trace,
      );
      return OuterCacheAction::Execute;
    }
  };
  let observation = recorder.observation();
  let initial_input_bytes = estimated_input_bytes(observation, source_root);
  trace.finish(
    input_capture_phase,
    NativeCacheWrapperWork {
      bytes_hashed: initial_input_bytes,
      ..NativeCacheWrapperWork::default()
    },
  );
  let classification_phase = trace.start(NativeCacheWrapperPhase::BypassClassification);
  let bypass_reason = invocation_bypass_reason(observation, false, &session.class.host_target);
  trace.finish(classification_phase, NativeCacheWrapperWork::default());
  if let Some(reason) = bypass_reason {
    let candidate_phase = trace.start(NativeCacheWrapperPhase::CandidateKeyConstruction);
    let unit_identity = candidate_key(&session.identity, &session.class, observation).ok();
    trace.finish(candidate_phase, NativeCacheWrapperWork::default());
    if context.retain_event_evidence
      && let Some(unit_identity) = &unit_identity
    {
      let output_paths = recorder.native_output_paths();
      retain_pre_execution_unit_evidence(
        observation_directory,
        unit_identity,
        observation,
        output_paths.as_ref(),
        source_root,
      );
    }
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      reason,
      unit_identity,
      initial_input_bytes,
      diagnostic_wrapper,
      trace,
    );
    #[cfg(target_os = "macos")]
    let portable_bypass_arguments =
      portable_macos_bypass_arguments(reason, &portable_arguments, observation).unwrap_or(portable_arguments);
    #[cfg(not(target_os = "macos"))]
    let portable_bypass_arguments = portable_arguments;
    configure_portable_child(
      command,
      program,
      rustc,
      &portable_bypass_arguments,
      diagnostic_wrapper,
      source_root,
    );
    return OuterCacheAction::Execute;
  }
  let Some(output_paths) = recorder.native_output_paths() else {
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      "compiler_output_paths_unavailable",
      None,
      estimated_input_bytes(observation, source_root),
      diagnostic_wrapper,
      trace,
    );
    return OuterCacheAction::Execute;
  };
  if validated_output_parent(&output_paths, source_root).is_err() {
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      "compiler_output_root_not_graduated",
      None,
      estimated_input_bytes(observation, source_root),
      diagnostic_wrapper,
      trace,
    );
    return OuterCacheAction::Execute;
  }
  let candidate_phase = trace.start(NativeCacheWrapperPhase::CandidateKeyConstruction);
  let candidate = candidate_key(&session.identity, &session.class, observation);
  trace.finish(candidate_phase, NativeCacheWrapperWork::default());
  let candidate = match candidate {
    Ok(candidate) => candidate,
    Err(_) => {
      configure_cold(
        command,
        CompilerCacheWrapperStatus::Bypassed,
        "candidate_key_unavailable",
        None,
        estimated_input_bytes(observation, source_root),
        diagnostic_wrapper,
        trace,
      );
      return OuterCacheAction::Execute;
    }
  };
  let mut metrics = NativeCacheMetrics {
    bytes_hashed: initial_input_bytes,
    ..NativeCacheMetrics::default()
  };
  let cas_open_phase = trace.start(NativeCacheWrapperPhase::CasOpen);
  let cas = LocalCas::open_initialized();
  trace.finish(cas_open_phase, NativeCacheWrapperWork::default());
  let cas = match cas {
    Ok(cas) => cas,
    Err(_) => {
      configure_cold(
        command,
        CompilerCacheWrapperStatus::Bypassed,
        "local_cache_unavailable",
        Some(candidate),
        metrics.bytes_hashed,
        diagnostic_wrapper,
        trace,
      );
      return OuterCacheAction::Execute;
    }
  };
  let candidate_lookup_phase = trace.start(NativeCacheWrapperPhase::CandidateLookup);
  let candidates = cas.native_candidates(&candidate);
  let lookup_bytes = candidates
    .as_ref()
    .map(|candidates| {
      candidates
        .iter()
        .fold(0u64, |bytes, candidate| bytes.saturating_add(candidate.bytes_read))
    })
    .unwrap_or_default();
  trace.finish(
    candidate_lookup_phase,
    NativeCacheWrapperWork {
      cache_bytes_read: lookup_bytes,
      ..NativeCacheWrapperWork::default()
    },
  );
  let candidates = match candidates {
    Ok(candidates) => candidates,
    Err(_) => {
      configure_cold(
        command,
        CompilerCacheWrapperStatus::Bypassed,
        "local_cache_candidate_corrupt",
        Some(candidate),
        metrics.bytes_hashed,
        diagnostic_wrapper,
        trace,
      );
      return OuterCacheAction::Execute;
    }
  };
  let mut miss_reason = "candidate_not_found";
  for (candidate_index, cached) in candidates.iter().enumerate() {
    let _ = cached.objects_verified;
    metrics.cache_bytes_read = metrics.cache_bytes_read.saturating_add(cached.bytes_read);
    let revalidation_phase = trace.start(NativeCacheWrapperPhase::InputRevalidationActionKey);
    let revalidated = revalidate_candidate(&cached.validation, &session, observation, source_root);
    let revalidated_bytes = match &revalidated {
      Ok(revalidated) => revalidated.bytes_hashed,
      Err((_, bytes)) => *bytes,
    };
    trace.finish(
      revalidation_phase,
      NativeCacheWrapperWork {
        bytes_hashed: revalidated_bytes,
        ..NativeCacheWrapperWork::default()
      },
    );
    let revalidated = match revalidated {
      Ok(revalidated) if revalidated.action_key == cached.action_key => {
        metrics.bytes_hashed = metrics.bytes_hashed.saturating_add(revalidated.bytes_hashed);
        revalidated
      }
      Ok(revalidated) => {
        metrics.bytes_hashed = metrics.bytes_hashed.saturating_add(revalidated.bytes_hashed);
        miss_reason = "candidate_action_binding_mismatch";
        continue;
      }
      Err((reason, hashed)) => {
        metrics.bytes_hashed = metrics.bytes_hashed.saturating_add(hashed);
        miss_reason = reason;
        continue;
      }
    };
    match restore_and_publish(
      &candidates,
      candidate_index,
      &cached.validation,
      &revalidated.observation,
      &output_paths,
      &mut metrics,
      trace,
    ) {
      Ok(()) => return OuterCacheAction::Hit(0),
      Err(_) => {
        miss_reason = "verified_result_materialization_failed";
      }
    }
  }
  let metadata = configure_cold(
    command,
    CompilerCacheWrapperStatus::Miss,
    miss_reason,
    Some(candidate),
    metrics.bytes_hashed,
    diagnostic_wrapper,
    trace,
  );
  configure_portable_child(
    command,
    program,
    rustc,
    &portable_arguments,
    diagnostic_wrapper,
    source_root,
  );
  let mut recorder = recorder;
  recorder.set_cache_wrapper(metadata);
  OuterCacheAction::Store {
    recorder,
    cache_bytes_read: metrics.cache_bytes_read,
  }
}

fn portable_compiler_arguments(
  arguments: &[OsString],
  original_current_dir: &Path,
  source_root: &Path,
) -> Result<Vec<OsString>, &'static str> {
  let canonical_root =
    crate::utils::canonicalize_existing(source_root).map_err(|_| "native_cache_source_root_unavailable")?;
  let remap_roots = remap_source_root_spellings(source_root, &canonical_root)?;
  let text = arguments
    .iter()
    .map(|argument| argument.to_str().ok_or("non_utf8_compiler_argument"))
    .collect::<Result<Vec<_>, _>>()?;
  let mut rewritten = Vec::with_capacity(arguments.len() + 2);
  let mut index = 0usize;
  while index < text.len() {
    let argument = text[index];
    let next = text.get(index + 1).copied();
    match argument {
      "--out-dir" => {
        let value = next.ok_or("compiler_output_paths_unavailable")?;
        rewritten.push(OsString::from(argument));
        rewritten.push(OsString::from(rewrite_compiler_path(
          value,
          original_current_dir,
          &canonical_root,
          true,
        )?));
        index += 2;
      }
      "--extern" => {
        let value = next.ok_or("dependency_artifact_path_unavailable")?;
        rewritten.push(OsString::from(argument));
        rewritten.push(if value == "proc_macro" {
          OsString::from(value)
        } else {
          OsString::from(rewrite_prefixed_path(
            value,
            '=',
            original_current_dir,
            &canonical_root,
            false,
          )?)
        });
        index += 2;
      }
      "-L" => {
        let value = next.ok_or("dependency_artifact_path_unavailable")?;
        rewritten.push(OsString::from(argument));
        rewritten.push(OsString::from(rewrite_prefixed_path(
          value,
          '=',
          original_current_dir,
          &canonical_root,
          false,
        )?));
        index += 2;
      }
      "--emit" => {
        let value = next.ok_or("compiler_output_paths_unavailable")?;
        rewritten.push(OsString::from(argument));
        rewritten.push(OsString::from(rewrite_emit_paths(
          value,
          original_current_dir,
          &canonical_root,
        )?));
        index += 2;
      }
      _ if argument.starts_with("--out-dir=") => {
        let value = argument.trim_start_matches("--out-dir=");
        let path = rewrite_compiler_path(value, original_current_dir, &canonical_root, true)?;
        rewritten.push(OsString::from(format!("--out-dir={path}")));
        index += 1;
      }
      _ if argument.starts_with("--extern=") => {
        let value = argument.trim_start_matches("--extern=");
        rewritten.push(if value == "proc_macro" {
          OsString::from(argument)
        } else {
          let value = rewrite_prefixed_path(value, '=', original_current_dir, &canonical_root, false)?;
          OsString::from(format!("--extern={value}"))
        });
        index += 1;
      }
      _ if argument.starts_with("-Ldependency=") => {
        let value = argument.trim_start_matches("-L");
        let value = rewrite_prefixed_path(value, '=', original_current_dir, &canonical_root, false)?;
        rewritten.push(OsString::from(format!("-L{value}")));
        index += 1;
      }
      _ if argument.starts_with("--emit=") => {
        let value = argument.trim_start_matches("--emit=");
        rewritten.push(OsString::from(format!(
          "--emit={}",
          rewrite_emit_paths(value, original_current_dir, &canonical_root)?
        )));
        index += 1;
      }
      _ if !argument.starts_with('-') && argument.ends_with(".rs") => {
        rewritten.push(OsString::from(rewrite_compiler_path(
          argument,
          original_current_dir,
          &canonical_root,
          false,
        )?));
        index += 1;
      }
      _ if argument.starts_with("incremental=") => return Err("incremental_compilation_not_graduated"),
      _ => {
        if remap_roots.iter().any(|root| argument.contains(root)) {
          return Err("compiler_argument_root_binding_not_graduated");
        }
        rewritten.push(OsString::from(argument));
        index += 1;
      }
    }
  }
  for root in remap_roots {
    rewritten.push(OsString::from(format!(
      "--remap-path-prefix={root}=/cargo-rail/workspace"
    )));
  }
  Ok(rewritten)
}

fn remap_source_root_spellings(source_root: &Path, canonical_root: &Path) -> Result<Vec<String>, &'static str> {
  let mut roots = BTreeSet::new();
  for root in [source_root, canonical_root] {
    let root = root
      .to_str()
      .filter(|root| !root.is_empty() && !root.contains('='))
      .ok_or("source_root_not_remappable")?;
    roots.insert(root.to_string());
    #[cfg(windows)]
    roots.insert(root.replace('\\', "/"));
  }
  Ok(roots.into_iter().collect())
}

#[cfg(target_os = "macos")]
fn portable_macos_bypass_arguments(
  reason: &str,
  arguments: &[OsString],
  observation: &RawCompilerInvocation,
) -> Option<Vec<OsString>> {
  match reason {
    "proc_macro_not_graduated" => portable_macos_proc_macro_arguments(arguments, observation),
    "dependency_artifact_class_not_graduated" if only_proc_macro_dylib_is_ungraduated(observation) => {
      Some(arguments.to_vec())
    }
    _ => None,
  }
}

#[cfg(target_os = "macos")]
fn only_proc_macro_dylib_is_ungraduated(observation: &RawCompilerInvocation) -> bool {
  let mut dylib = false;
  for (_, artifact) in &observation.dependency_artifacts {
    match artifact
      .path
      .resolve(Path::new("/"))
      .extension()
      .and_then(OsStr::to_str)
    {
      Some("rmeta" | "rlib") => {}
      Some("dylib") => dylib = true,
      _ => return false,
    }
  }
  dylib
}

#[cfg(target_os = "macos")]
fn portable_macos_proc_macro_arguments(
  arguments: &[OsString],
  observation: &RawCompilerInvocation,
) -> Option<Vec<OsString>> {
  let crate_name = observation.crate_name.as_deref()?;
  let mut extra_filename = None;
  let mut index = 0usize;
  while index < arguments.len() {
    let argument = arguments[index].to_str()?;
    let codegen = if argument == "-C" {
      index += 1;
      arguments.get(index)?.to_str()?
    } else {
      argument.strip_prefix("-C").unwrap_or_default()
    };
    let name = codegen.split_once('=').map_or(codegen, |(name, _)| name);
    if matches!(
      name,
      "linker" | "link-arg" | "link-args" | "linker-flavor" | "link-self-contained" | "default-linker-libraries"
    ) || argument == "-l"
      || argument.starts_with("-l")
      || argument.starts_with("-Lnative")
      || argument == "-L"
        && arguments
          .get(index + 1)
          .and_then(|value| value.to_str())
          .is_some_and(|value| value.starts_with("native="))
    {
      return None;
    }
    if let Some(value) = codegen.strip_prefix("extra-filename=") {
      extra_filename = Some(value);
    }
    index += 1;
  }
  let extra_filename = extra_filename?;
  if !crate_name
    .bytes()
    .chain(extra_filename.bytes())
    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
  {
    return None;
  }
  let install_name = format!("link-arg=-Wl,-install_name,@rpath/lib{crate_name}{extra_filename}.dylib");
  let mut portable = arguments.to_vec();
  portable.push(OsString::from("-C"));
  portable.push(OsString::from(install_name));
  Some(portable)
}

fn rewrite_emit_paths(value: &str, original_current_dir: &Path, source_root: &Path) -> Result<String, &'static str> {
  value
    .split(',')
    .map(|emit| {
      let Some((mode, path)) = emit.split_once('=') else {
        return Ok(emit.to_string());
      };
      rewrite_compiler_path(path, original_current_dir, source_root, true).map(|path| format!("{mode}={path}"))
    })
    .collect::<Result<Vec<_>, _>>()
    .map(|parts| parts.join(","))
}

fn rewrite_prefixed_path(
  value: &str,
  separator: char,
  original_current_dir: &Path,
  source_root: &Path,
  require_repository: bool,
) -> Result<String, &'static str> {
  let (prefix, path) = value
    .split_once(separator)
    .ok_or("dependency_artifact_path_unavailable")?;
  rewrite_compiler_path(path, original_current_dir, source_root, require_repository)
    .map(|path| format!("{prefix}{separator}{path}"))
}

fn rewrite_compiler_path(
  value: &str,
  original_current_dir: &Path,
  source_root: &Path,
  require_repository: bool,
) -> Result<String, &'static str> {
  let selected = Path::new(value);
  let absolute = if selected.is_absolute() {
    selected.to_path_buf()
  } else {
    original_current_dir.join(selected)
  };
  let resolved = if absolute.exists() {
    crate::utils::canonicalize_existing(&absolute).map_err(|_| "compiler_path_unavailable")?
  } else {
    let parent = absolute.parent().ok_or("compiler_path_unavailable")?;
    let parent = crate::utils::canonicalize_existing(parent).map_err(|_| "compiler_path_unavailable")?;
    parent.join(absolute.file_name().ok_or("compiler_path_unavailable")?)
  };
  if let Ok(relative) = resolved.strip_prefix(source_root) {
    return Ok(crate::utils::path_to_git_format(relative));
  }
  if require_repository {
    return Err("compiler_output_root_not_graduated");
  }
  resolved.to_str().map(str::to_string).ok_or("non_utf8_compiler_path")
}

fn configure_portable_child(
  command: &mut Command,
  program: &OsStr,
  rustc: &OsStr,
  compiler_arguments: &[OsString],
  diagnostic_wrapper: bool,
  source_root: &Path,
) {
  let mut portable = Command::new(program);
  if diagnostic_wrapper {
    portable.arg(rustc);
  }
  portable
    .args(compiler_arguments)
    .current_dir(source_root)
    .env_remove(crate::compiler::wrapper::CACHE_WRAPPER_MARKER)
    .env_remove(crate::compiler::wrapper::OBSERVATION_DIRECTORY_ENV)
    .env_remove(crate::compiler::wrapper::OBSERVATION_SOURCE_ROOT_ENV)
    .env_remove(SESSION_ENV)
    .env_remove(DISPOSITION_ENV);
  *command = portable;
}

fn configure_cold(
  command: &mut Command,
  status: CompilerCacheWrapperStatus,
  reason: &'static str,
  candidate_key: Option<String>,
  bytes_hashed: u64,
  propagate_metadata: bool,
  trace: &NativeCacheWrapperTrace,
) -> CompilerCacheWrapperMetadata {
  let metadata = CompilerCacheWrapperMetadata::native(status, reason, candidate_key.clone(), None, bytes_hashed, 0);
  if propagate_metadata && let Ok(encoded) = serde_json::to_string(&metadata) {
    command.env(DISPOSITION_ENV, encoded);
  }
  if status != CompilerCacheWrapperStatus::Miss {
    write_cache_event(
      status,
      reason,
      candidate_key.as_deref(),
      None,
      NativeCacheMetrics {
        bytes_hashed,
        ..NativeCacheMetrics::default()
      },
      trace,
    );
  }
  metadata
}

fn is_diagnostic_workspace_wrapper(program: &OsStr) -> bool {
  if std::env::var_os(crate::compiler::wrapper::WRAPPER_MARKER).is_none() {
    return false;
  }
  let Ok(current) = std::env::current_exe().and_then(fs::canonicalize) else {
    return false;
  };
  let selected = Path::new(program);
  let selected = if selected.is_absolute() {
    selected.to_path_buf()
  } else {
    match std::env::current_dir() {
      Ok(current_dir) => current_dir.join(selected),
      Err(_) => return false,
    }
  };
  fs::canonicalize(selected).is_ok_and(|selected| selected == current)
}

fn estimated_input_bytes(observation: &RawCompilerInvocation, source_root: &Path) -> u64 {
  observation
    .declared_inputs
    .iter()
    .chain(observation.dependency_artifacts.iter().map(|(_, file)| file))
    .filter_map(|file| fs::metadata(file.path.resolve(source_root)).ok())
    .fold(0u64, |total, metadata| total.saturating_add(metadata.len()))
}

fn revalidate_candidate(
  validation: &NativeCompilerValidation,
  session: &NativeCompilerSession,
  current: &RawCompilerInvocation,
  source_root: &Path,
) -> Result<RevalidatedNativeCandidate, (&'static str, u64)> {
  if validation.validate_object().is_err()
    || validation.session_identity != session.identity
    || validation.class != session.class
  {
    return Err(("candidate_observation_incompatible", 0));
  }
  let current_candidate =
    candidate_key(&session.identity, &session.class, current).map_err(|_| ("candidate_key_unavailable", 0))?;
  if current_candidate != validation.candidate_key {
    return Err(("candidate_pre_execution_inputs_changed", 0));
  }

  let mut bytes_hashed = 0u64;
  let mut revalidated = validation.observation.clone();
  revalidated.compiler_arguments = current.compiler_arguments.clone();
  revalidated.declared_inputs = current.declared_inputs.clone();
  revalidated.dependency_artifacts = current.dependency_artifacts.clone();
  revalidated.observed_reads = revalidate_observed_files(
    &validation.observation.observed_reads,
    &validation.observation.declared_inputs,
    &current.declared_inputs,
    source_root,
    &mut bytes_hashed,
  )?;
  for environment in &validation.observation.environment_reads {
    if environment.secret_capability {
      return Err(("secret_compiler_environment", bytes_hashed));
    }
    let current = std::env::var_os(&environment.name)
      .as_deref()
      .map(OsStr::as_encoded_bytes)
      .map(ContentDigest::sha256)
      .map(|digest| format!("sha256:{digest}"));
    if current != environment.value_digest {
      return Err(("compiler_environment_changed", bytes_hashed));
    }
  }
  let action = action_key(&session.identity, &session.class, &revalidated)
    .map_err(|_| ("compiler_action_key_unavailable", bytes_hashed))?;
  Ok(RevalidatedNativeCandidate {
    action_key: action,
    observation: revalidated,
    bytes_hashed,
  })
}

#[derive(Debug)]
struct RevalidatedNativeCandidate {
  action_key: String,
  observation: RawCompilerInvocation,
  bytes_hashed: u64,
}

fn revalidate_observed_files(
  observed: &[FileObservation],
  stored_declared: &[FileObservation],
  current_declared: &[FileObservation],
  source_root: &Path,
  bytes_hashed: &mut u64,
) -> Result<Vec<FileObservation>, (&'static str, u64)> {
  observed
    .iter()
    .map(|file| {
      if let Some(index) = stored_declared.iter().position(|declared| declared == file) {
        return current_declared
          .get(index)
          .cloned()
          .ok_or(("declared_compiler_input_changed", *bytes_hashed));
      }
      revalidate_file(file, source_root, bytes_hashed).map_err(|_| ("observed_compiler_read_changed", *bytes_hashed))
    })
    .collect()
}

fn revalidate_file(
  expected: &FileObservation,
  source_root: &Path,
  bytes_hashed: &mut u64,
) -> RailResult<FileObservation> {
  let path = expected.path.resolve(source_root);
  let (current, read) = FileObservation::capture_counted(&path, source_root, source_root)?;
  *bytes_hashed = bytes_hashed.saturating_add(read);
  if &current != expected {
    return Err(RailError::message("observed compiler input changed"));
  }
  Ok(current)
}

fn restore_and_publish(
  candidates: &NativeCacheCandidates<'_>,
  candidate_index: usize,
  validation: &NativeCompilerValidation,
  revalidated_observation: &RawCompilerInvocation,
  output_paths: &NativeOutputPaths,
  metrics: &mut NativeCacheMetrics,
  trace: &mut NativeCacheWrapperTrace,
) -> RailResult<()> {
  let context = active_context().ok_or_else(|| RailError::message("native compiler cache context disappeared"))?;
  let source_root = &context.source_root;
  let observation_directory = &context.observation_directory;
  let restore_phase = trace.start(NativeCacheWrapperPhase::ResultRestoreMaterialization);
  let cache_bytes_before = metrics.cache_bytes_read;
  validate_current_output_binding(validation, output_paths, source_root)?;
  let output_parent = validated_output_parent(output_paths, source_root)?;
  let temporary = tempfile::Builder::new()
    .prefix(".cargo-rail-native-cache-")
    .tempdir_in(&output_parent)?;
  let restored = temporary.path().join("verified");
  let hit = match candidates.restore(candidate_index, &restored) {
    NativeCacheLookup::Hit(hit) => {
      metrics.cache_bytes_read = metrics.cache_bytes_read.saturating_add(hit.bytes_read);
      hit
    }
    NativeCacheLookup::Miss(miss) => {
      let _ = miss.objects_verified;
      metrics.cache_bytes_read = metrics.cache_bytes_read.saturating_add(miss.bytes_read);
      return Err(RailError::message(format!(
        "native compiler cache restore rejected the result: {}",
        miss.reason
      )));
    }
  };

  let stdout = read_bounded(&restored.join(STDOUT_SLOT), MAX_STREAM_BYTES)?;
  let stderr = read_bounded(&restored.join(STDERR_SLOT), MAX_STREAM_BYTES)?;
  if digest(&stdout) != validation.stdout_digest || digest(&stderr) != validation.stderr_digest {
    return Err(RailError::message(
      "native compiler cache stream binding changed after restore",
    ));
  }
  let stdout = translate_output_binding_bytes(&stdout, validation, output_paths, source_root, false)?;
  let stderr = translate_output_binding_bytes(&stderr, validation, output_paths, source_root, false)?;
  trace.finish(
    restore_phase,
    NativeCacheWrapperWork {
      bytes_hashed: hit.bytes_restored,
      cache_bytes_read: metrics.cache_bytes_read.saturating_sub(cache_bytes_before),
      bytes_restored: hit.bytes_restored,
      ..NativeCacheWrapperWork::default()
    },
  );
  let publication_phase = trace.start(NativeCacheWrapperPhase::CargoOutputPublication);
  let bindings = native_output_bindings(output_paths);
  let mut emitted_outputs = Vec::with_capacity(bindings.len());
  let mut published_bytes = 0u64;
  for ((role, slot, destination), expected) in bindings.iter().zip(&validation.outputs) {
    if *role == "dep_info" {
      let source = restored.join(slot);
      let bytes = read_bounded(&source, usize::try_from(expected.bytes).unwrap_or(usize::MAX))?;
      if bytes.len() as u64 != expected.bytes || digest(&bytes) != expected.content_digest {
        return Err(RailError::message(
          "native compiler dep-info binding changed after restore",
        ));
      }
      let materialized = translate_dep_info_output_bindings(&bytes, validation, output_paths, source_root)?;
      let staged = temporary.path().join("materialized-dep-info");
      write_new_file(&staged, &materialized)?;
      let materialized_output = NativeCompilerOutput {
        role: expected.role.clone(),
        slot: expected.slot.clone(),
        content_digest: digest(&materialized),
        bytes: materialized.len() as u64,
      };
      publish_output(&staged, destination, &materialized_output)?;
      published_bytes = published_bytes.saturating_add(materialized_output.bytes);
    } else {
      publish_output(&restored.join(slot), destination, expected)?;
      published_bytes = published_bytes.saturating_add(expected.bytes);
    }
    emitted_outputs.push(FileObservation::capture(destination, source_root, source_root)?);
  }
  let mut raw = revalidated_observation.clone();
  raw.emitted_outputs = emitted_outputs;
  raw.emitted_outputs.sort();
  raw.success = true;
  raw.cache_wrapper = Some(CompilerCacheWrapperMetadata::native(
    CompilerCacheWrapperStatus::Hit,
    "verified_local_result",
    Some(validation.candidate_key.clone()),
    Some(validation.action_key.clone()),
    metrics.bytes_hashed,
    hit.bytes_restored,
  ));
  crate::compiler::observation::publish_raw(observation_directory, &raw)?;
  std::io::stdout().write_all(&stdout)?;
  std::io::stderr().write_all(&stderr)?;
  trace.finish(
    publication_phase,
    NativeCacheWrapperWork {
      bytes_hashed: published_bytes,
      ..NativeCacheWrapperWork::default()
    },
  );
  write_cache_event(
    CompilerCacheWrapperStatus::Hit,
    "verified_local_result",
    Some(&validation.candidate_key),
    Some(&validation.action_key),
    NativeCacheMetrics {
      bytes_restored: hit.bytes_restored,
      ..*metrics
    },
    trace,
  );
  let _ = (hit.action_result, hit.result_digest, hit.objects_verified);
  Ok(())
}

fn validated_output_parent(outputs: &NativeOutputPaths, source_root: &Path) -> RailResult<PathBuf> {
  let bindings = native_output_bindings(outputs);
  let output_parent = bindings[0]
    .2
    .parent()
    .ok_or_else(|| RailError::message("dep-info output has no parent"))?;
  if bindings
    .iter()
    .any(|(_, _, output)| output.parent() != Some(output_parent))
  {
    return Err(RailError::message(
      "native compiler outputs do not share one publication directory",
    ));
  }
  let metadata = fs::symlink_metadata(output_parent)?;
  if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
    return Err(RailError::message(
      "native compiler output parent is not a real directory",
    ));
  }
  let canonical_parent = crate::utils::canonicalize_existing(output_parent)?;
  let canonical_root = crate::utils::canonicalize_existing(source_root)?;
  if !canonical_parent.starts_with(&canonical_root)
    || bindings.iter().any(|(role, _, output)| {
      output.extension()
        != Some(OsStr::new(match *role {
          "dep_info" => "d",
          "metadata" => "rmeta",
          "rlib" => "rlib",
          _ => "",
        }))
    })
    || bindings
      .iter()
      .map(|(_, _, output)| *output)
      .collect::<BTreeSet<_>>()
      .len()
      != bindings.len()
  {
    return Err(RailError::message(
      "native compiler outputs are outside the graduated publication root",
    ));
  }
  Ok(canonical_parent)
}

fn native_output_bindings(outputs: &NativeOutputPaths) -> Vec<(&'static str, &'static str, &Path)> {
  let mut bindings = vec![
    ("dep_info", DEP_INFO_SLOT, outputs.dep_info.as_path()),
    ("metadata", METADATA_SLOT, outputs.metadata.as_path()),
  ];
  if let Some(rlib) = &outputs.rlib {
    bindings.push(("rlib", RLIB_SLOT, rlib));
  }
  bindings
}

fn validate_current_output_binding(
  validation: &NativeCompilerValidation,
  outputs: &NativeOutputPaths,
  source_root: &Path,
) -> RailResult<()> {
  let stored = validation
    .observation
    .emitted_outputs
    .iter()
    .filter_map(|output| output_binding_role(&output.path).map(|role| (role, observation_path_basename(&output.path))))
    .collect::<BTreeMap<_, _>>();
  let current = native_output_bindings(outputs)
    .into_iter()
    .map(|(role, _, output)| (role, output.file_name().and_then(OsStr::to_str)))
    .collect::<BTreeMap<_, _>>();
  if stored != current || stored.len() != validation.outputs.len() {
    return Err(RailError::message(
      "native compiler output file names do not match the verified invocation",
    ));
  }
  validated_output_parent(outputs, source_root)?;
  Ok(())
}

fn output_binding_role(path: &ObservationPath) -> Option<&'static str> {
  match portable_path_basename(match path {
    ObservationPath::Repository(path) | ObservationPath::Host(path) => path,
  })?
  .rsplit_once('.')?
  .1
  {
    "d" => Some("dep_info"),
    "rmeta" => Some("metadata"),
    "rlib" => Some("rlib"),
    _ => None,
  }
}

fn translate_dep_info_output_bindings(
  bytes: &[u8],
  validation: &NativeCompilerValidation,
  outputs: &NativeOutputPaths,
  source_root: &Path,
) -> RailResult<Vec<u8>> {
  translate_output_binding_bytes(bytes, validation, outputs, source_root, true)
}

fn portable_stream_output_bindings(
  bytes: &[u8],
  outputs: &NativeOutputPaths,
  source_root: &Path,
) -> RailResult<Vec<u8>> {
  portable_output_binding_bytes(bytes, outputs, source_root, false)
}

fn portable_dep_info_output_bindings(
  bytes: &[u8],
  outputs: &NativeOutputPaths,
  source_root: &Path,
) -> RailResult<Vec<u8>> {
  portable_output_binding_bytes(bytes, outputs, source_root, true)
}

fn portable_output_binding_bytes(
  bytes: &[u8],
  outputs: &NativeOutputPaths,
  source_root: &Path,
  require_replacement: bool,
) -> RailResult<Vec<u8>> {
  let mut replacements = native_output_bindings(outputs)
    .into_iter()
    .map(|(_, _, output)| {
      let relative = output
        .strip_prefix(source_root)
        .map_err(|_| RailError::message("native compiler output is outside the source root"))?;
      let relative = crate::utils::path_to_git_format(relative);
      let absolute = crate::utils::path_to_git_format(output);
      let portable = portable_output_binding(&relative)?;
      Ok(
        [
          canonical_output_path_replacements(&absolute, &portable),
          canonical_output_path_replacements(&relative, &portable),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>(),
      )
    })
    .collect::<RailResult<Vec<_>>>()?
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
  replacements.sort_unstable_by(|left, right| right.0.len().cmp(&left.0.len()).then_with(|| left.cmp(right)));
  replacements.dedup();

  let mut portable = bytes.to_vec();
  let mut replacement_count = 0usize;
  for (current, canonical) in replacements {
    let (next, count) = replace_bytes(&portable, &current, &canonical);
    portable = next;
    replacement_count = replacement_count.saturating_add(count);
  }

  let output = native_output_bindings(outputs)[0].2;
  let output_parent = output
    .parent()
    .ok_or_else(|| RailError::message("native compiler output has no parent"))?;
  let relative_parent = output_parent
    .strip_prefix(source_root)
    .map_err(|_| RailError::message("native compiler output parent is outside the source root"))?;
  if require_replacement && replacement_count == 0
    || [
      crate::utils::path_to_git_format(output_parent),
      crate::utils::path_to_git_format(relative_parent),
    ]
    .iter()
    .flat_map(|path| output_path_spellings(path))
    .any(|path| contains_path_prefix(&portable, &path))
  {
    return Err(RailError::message(
      "native compiler output contains an unmodeled output-directory binding",
    ));
  }
  Ok(portable)
}

fn portable_output_binding(path: &str) -> RailResult<String> {
  let file_name = portable_path_basename(path)
    .ok_or_else(|| RailError::message("native compiler output has no portable file name"))?;
  Ok(format!("/cargo-rail/native-output/{file_name}"))
}

fn translate_output_binding_bytes(
  bytes: &[u8],
  validation: &NativeCompilerValidation,
  outputs: &NativeOutputPaths,
  source_root: &Path,
  require_replacement: bool,
) -> RailResult<Vec<u8>> {
  let current = native_output_bindings(outputs)
    .into_iter()
    .map(|(role, _, path)| {
      let relative = path
        .strip_prefix(source_root)
        .map_err(|_| RailError::message("native compiler dep-info destination is outside the source root"))?;
      Ok((role, crate::utils::path_to_git_format(relative)))
    })
    .collect::<RailResult<BTreeMap<_, _>>>()?;
  let stored = validation
    .observation
    .emitted_outputs
    .iter()
    .map(|output| {
      let role = output_binding_role(&output.path)
        .ok_or_else(|| RailError::message("native compiler dep-info source has an unknown output role"))?;
      let ObservationPath::Repository(path) = &output.path else {
        return Err(RailError::message(
          "native compiler dep-info source is not repository-relative",
        ));
      };
      Ok((role, portable_output_binding(path)?))
    })
    .collect::<RailResult<BTreeMap<_, _>>>()?;
  if stored.keys().ne(current.keys()) {
    return Err(RailError::message(
      "native compiler dep-info output roles changed during materialization",
    ));
  }

  let stored_parent = common_output_parent(stored.values().map(String::as_str))?;

  let mut replacements = stored
    .iter()
    .map(|(role, stored)| (stored.as_bytes().to_vec(), current[role].as_bytes().to_vec()))
    .collect::<Vec<_>>();
  replacements.sort_unstable_by(|left, right| right.0.len().cmp(&left.0.len()).then_with(|| left.cmp(right)));
  replacements.dedup();
  let mut translated = bytes.to_vec();
  let mut replacement_count = 0usize;
  for (stored, current) in replacements {
    let (next, count) = replace_bytes(&translated, &stored, &current);
    translated = next;
    replacement_count = replacement_count.saturating_add(count);
  }
  if require_replacement && replacement_count == 0
    || output_path_spellings(&stored_parent)
      .iter()
      .any(|path| contains_path_prefix(&translated, path))
  {
    return Err(RailError::message(
      "native compiler cached data contains an unmodeled output-directory binding",
    ));
  }
  Ok(translated)
}

fn common_output_parent<'a>(paths: impl IntoIterator<Item = &'a str>) -> RailResult<String> {
  let mut parents = paths
    .into_iter()
    .map(|path| {
      path
        .rsplit_once(['/', '\\'])
        .map(|(parent, _)| parent.to_string())
        .ok_or_else(|| RailError::message("native compiler output has no portable parent"))
    })
    .collect::<RailResult<BTreeSet<_>>>()?;
  if parents.len() != 1 {
    return Err(RailError::message(
      "native compiler outputs do not share one portable parent",
    ));
  }
  parents
    .pop_first()
    .ok_or_else(|| RailError::message("native compiler output set is empty"))
}

fn canonical_output_path_replacements(current: &str, portable: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
  let portable = portable.as_bytes().to_vec();
  output_path_spellings(current)
    .into_iter()
    .map(|current| (current, portable.clone()))
    .collect()
}

fn output_path_spellings(path: &str) -> Vec<Vec<u8>> {
  let git = path.replace('\\', "/");
  let native = git.replace('/', "\\");
  let mut plain = vec![git.clone(), native.clone()];
  if let Some((parent, name)) = git.rsplit_once('/') {
    plain.push(format!("{parent}\\{name}"));
    plain.push(format!("{}/{name}", parent.replace('/', "\\")));
  }
  let mut spellings = Vec::new();
  for plain in plain.into_iter().map(String::into_bytes) {
    let escaped = escape_dep_info_path(&plain);
    let json = json_string_contents(&plain);
    spellings.extend([plain, escaped, json]);
  }
  spellings
}

fn json_string_contents(value: &[u8]) -> Vec<u8> {
  let Ok(value) = std::str::from_utf8(value) else {
    return value.to_vec();
  };
  match serde_json::to_vec(value) {
    Ok(encoded) if encoded.len() >= 2 => encoded[1..encoded.len() - 1].to_vec(),
    _ => value.as_bytes().to_vec(),
  }
}

fn escape_dep_info_path(path: &[u8]) -> Vec<u8> {
  let mut escaped = Vec::with_capacity(path.len());
  for byte in path {
    if byte.is_ascii_whitespace() || matches!(byte, b'\\' | b'#' | b':') {
      escaped.push(b'\\');
    }
    escaped.push(*byte);
  }
  escaped
}

fn replace_bytes(bytes: &[u8], from: &[u8], to: &[u8]) -> (Vec<u8>, usize) {
  if from.is_empty() {
    return (bytes.to_vec(), 0);
  }
  let mut output = Vec::with_capacity(bytes.len());
  let mut remaining = bytes;
  let mut count = 0usize;
  while let Some(index) = remaining.windows(from.len()).position(|window| window == from) {
    output.extend_from_slice(&remaining[..index]);
    output.extend_from_slice(to);
    remaining = &remaining[index + from.len()..];
    count += 1;
  }
  output.extend_from_slice(remaining);
  (output, count)
}

fn contains_path_prefix(bytes: &[u8], path: &[u8]) -> bool {
  !path.is_empty()
    && bytes.windows(path.len()).enumerate().any(|(index, window)| {
      if window != path {
        return false;
      }
      let before = index.checked_sub(1).and_then(|index| bytes.get(index));
      let after = bytes.get(index + path.len());
      before.is_none_or(|byte| byte.is_ascii_whitespace() || matches!(byte, b':' | b'=' | b'\'' | b'"'))
        && after.is_none_or(|byte| byte.is_ascii_whitespace() || matches!(byte, b'/' | b'\\' | b':' | b'\'' | b'"'))
    })
}

fn publish_output(source: &Path, destination: &Path, expected: &NativeCompilerOutput) -> RailResult<()> {
  if let Ok(metadata) = fs::symlink_metadata(destination) {
    if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) || !single_link(&metadata) {
      return Err(RailError::message(
        "native compiler output destination is prepositioned",
      ));
    }
    let source_metadata = fs::symlink_metadata(source)?;
    let bytes = read_bounded(destination, usize::try_from(expected.bytes).unwrap_or(usize::MAX))?;
    if bytes.len() as u64 == expected.bytes
      && digest(&bytes) == expected.content_digest
      && same_permissions(&metadata, &source_metadata)
    {
      return Ok(());
    }
    return Err(RailError::message(
      "native compiler output destination contains different bytes",
    ));
  }
  let parent = destination
    .parent()
    .ok_or_else(|| RailError::message("native compiler output has no parent"))?;
  let source_metadata = fs::symlink_metadata(source)?;
  if !source_metadata.is_file() || !single_link(&source_metadata) {
    return Err(RailError::message(
      "verified native compiler output is not a single-link regular file",
    ));
  }
  validate_publication_source(source, expected)?;
  match fs::hard_link(source, destination) {
    Ok(()) => {
      if let Err(error) = fs::remove_file(source) {
        let _ = fs::remove_file(destination);
        return Err(RailError::message(format!(
          "failed to release native compiler output staging link: {error}"
        )));
      }
      let published = fs::symlink_metadata(destination)?;
      if !published.is_file()
        || crate::utils::is_symlink_or_reparse(&published)
        || !single_link(&published)
        || !same_permissions(&published, &source_metadata)
      {
        let _ = fs::remove_file(destination);
        return Err(RailError::message(
          "native compiler output hard-link publication changed its file identity",
        ));
      }
      return Ok(());
    }
    Err(_) if destination.exists() => {
      let metadata = fs::symlink_metadata(destination)?;
      if !metadata.is_file()
        || crate::utils::is_symlink_or_reparse(&metadata)
        || !single_link(&metadata)
        || !same_permissions(&metadata, &source_metadata)
      {
        return Err(RailError::message(
          "concurrent native compiler output publication is not the verified file type",
        ));
      }
      let bytes = read_bounded(destination, usize::try_from(expected.bytes).unwrap_or(usize::MAX))?;
      if bytes.len() as u64 == expected.bytes && digest(&bytes) == expected.content_digest {
        return Ok(());
      }
      return Err(RailError::message(
        "concurrent native compiler output publication disagreed",
      ));
    }
    Err(_) => {}
  }

  let input = File::open(source)?;
  let mut input = input.take(expected.bytes.saturating_add(1));
  let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
  let mut hasher = Sha256::new();
  let mut copied = 0u64;
  let mut buffer = [0u8; 64 * 1024];
  loop {
    let read = input.read(&mut buffer)?;
    if read == 0 {
      break;
    }
    temporary.write_all(&buffer[..read])?;
    hasher.update(&buffer[..read]);
    copied = copied.saturating_add(read as u64);
  }
  crate::instrumentation::record_hash_operation();
  crate::instrumentation::record_hash_input_bytes(copied as usize);
  crate::instrumentation::record_hashed_file_bytes_read(copied as usize);
  let copied_digest = format!("sha256:{}", ContentDigest::from_sha256_bytes(hasher.finalize().into()));
  if copied != expected.bytes || copied_digest != expected.content_digest {
    return Err(RailError::message(
      "verified native compiler output changed during publication",
    ));
  }
  fs::set_permissions(temporary.path(), source_metadata.permissions())?;
  match temporary.persist_noclobber(destination) {
    Ok(_) => Ok(()),
    Err(error) if destination.is_file() => {
      let bytes = read_bounded(destination, usize::try_from(expected.bytes).unwrap_or(usize::MAX))?;
      if bytes.len() as u64 == expected.bytes && digest(&bytes) == expected.content_digest {
        Ok(())
      } else {
        Err(RailError::message(format!(
          "concurrent native compiler output publication disagreed: {}",
          error.error
        )))
      }
    }
    Err(error) => Err(RailError::message(format!(
      "failed to publish native compiler output '{}': {}",
      destination.display(),
      error.error
    ))),
  }
}

fn validate_publication_source(source: &Path, expected: &NativeCompilerOutput) -> RailResult<()> {
  let mut input = File::open(source)?;
  let opened = input.metadata()?;
  if !opened.is_file() || !single_link(&opened) || opened.len() != expected.bytes {
    return Err(RailError::message(
      "verified native compiler output changed before publication",
    ));
  }
  let mut hasher = Sha256::new();
  let mut read_bytes = 0u64;
  let mut buffer = [0u8; 64 * 1024];
  loop {
    let read = input.read(&mut buffer)?;
    if read == 0 {
      break;
    }
    hasher.update(&buffer[..read]);
    read_bytes = read_bytes.saturating_add(read as u64);
    if read_bytes > expected.bytes {
      return Err(RailError::message(
        "verified native compiler output changed before publication",
      ));
    }
  }
  crate::instrumentation::record_hash_operation();
  crate::instrumentation::record_hash_input_bytes(read_bytes as usize);
  crate::instrumentation::record_hashed_file_bytes_read(read_bytes as usize);
  let actual_digest = format!("sha256:{}", ContentDigest::from_sha256_bytes(hasher.finalize().into()));
  if read_bytes != expected.bytes || actual_digest != expected.content_digest {
    return Err(RailError::message(
      "verified native compiler output changed before publication",
    ));
  }
  Ok(())
}

#[cfg(unix)]
fn same_permissions(left: &fs::Metadata, right: &fs::Metadata) -> bool {
  use std::os::unix::fs::PermissionsExt as _;
  left.permissions().mode() == right.permissions().mode()
}

#[cfg(not(unix))]
fn same_permissions(left: &fs::Metadata, right: &fs::Metadata) -> bool {
  left.permissions().readonly() == right.permissions().readonly()
}

fn read_bounded(path: &Path, limit: usize) -> RailResult<Vec<u8>> {
  let metadata = fs::symlink_metadata(path)?;
  if !metadata.is_file()
    || crate::utils::is_symlink_or_reparse(&metadata)
    || !single_link(&metadata)
    || metadata.len() > limit as u64
  {
    return Err(RailError::message(format!(
      "native compiler cache file '{}' is not a bounded regular file",
      path.display()
    )));
  }
  let mut file = File::open(path)?;
  let opened = file.metadata()?;
  if !opened.is_file() || !single_link(&opened) || opened.len() != metadata.len() {
    return Err(RailError::message(format!(
      "native compiler cache file '{}' changed before it was read",
      path.display()
    )));
  }
  let mut bytes = Vec::with_capacity(metadata.len() as usize);
  std::io::Read::take(&mut file, metadata.len().saturating_add(1)).read_to_end(&mut bytes)?;
  if bytes.len() as u64 != metadata.len() {
    return Err(RailError::message(format!(
      "native compiler cache file '{}' changed while it was read",
      path.display()
    )));
  }
  Ok(bytes)
}

#[cfg(unix)]
fn single_link(metadata: &fs::Metadata) -> bool {
  use std::os::unix::fs::MetadataExt as _;
  metadata.nlink() == 1
}

#[cfg(not(unix))]
fn single_link(_metadata: &fs::Metadata) -> bool {
  true
}

fn digest(bytes: &[u8]) -> String {
  format!("sha256:{}", ContentDigest::sha256(bytes))
}

pub(crate) fn remove_private_environment(command: &mut Command) {
  command.env_remove(SESSION_ENV).env_remove(DISPOSITION_ENV);
}

/// Execute one eligible cold invocation, replay its exact streams, and publish
/// only a complete successful observation.
pub(crate) fn run_and_store(
  mut command: Command,
  recorder: InvocationRecorder,
  cache_bytes_read: u64,
  trace: &mut NativeCacheWrapperTrace,
  context: &str,
) -> i32 {
  let output_paths = recorder.native_output_paths();
  let stdout_file = match tempfile::NamedTempFile::new() {
    Ok(file) => file,
    Err(_) => {
      return run_without_store(
        command,
        recorder,
        cache_bytes_read,
        trace,
        context,
        "native_cache_stream_capture_unavailable",
      );
    }
  };
  let stderr_file = match tempfile::NamedTempFile::new() {
    Ok(file) => file,
    Err(_) => {
      return run_without_store(
        command,
        recorder,
        cache_bytes_read,
        trace,
        context,
        "native_cache_stream_capture_unavailable",
      );
    }
  };
  let stdout_writer = match stdout_file.reopen() {
    Ok(file) => file,
    Err(_) => {
      return run_without_store(
        command,
        recorder,
        cache_bytes_read,
        trace,
        context,
        "native_cache_stream_capture_unavailable",
      );
    }
  };
  let stderr_writer = match stderr_file.reopen() {
    Ok(file) => file,
    Err(_) => {
      return run_without_store(
        command,
        recorder,
        cache_bytes_read,
        trace,
        context,
        "native_cache_stream_capture_unavailable",
      );
    }
  };
  let status = command.stdout(stdout_writer).stderr(stderr_writer).status();
  let status = match status {
    Ok(status) => status,
    Err(error) => {
      eprintln!("{context}: failed to execute compiler: {error}");
      return 1;
    }
  };
  let stdout_len = stdout_file.as_file().metadata().map(|metadata| metadata.len()).ok();
  let stderr_len = stderr_file.as_file().metadata().map(|metadata| metadata.len()).ok();
  if let Ok(mut stdout) = File::open(stdout_file.path()) {
    let _ = std::io::copy(&mut stdout, &mut std::io::stdout());
  }
  if let Ok(mut stderr) = File::open(stderr_file.path()) {
    let _ = std::io::copy(&mut stderr, &mut std::io::stderr());
  }

  let mut raw = match recorder.complete(status.success()) {
    Ok(raw) => raw,
    Err(_) => return status.code().unwrap_or(1),
  };
  if !status.success() {
    let _ = publish_and_record_cold_observation(
      &mut raw,
      "compiler_execution_failed",
      None,
      0,
      cache_bytes_read,
      0,
      trace,
    );
    return status.code().unwrap_or(1);
  }
  let Some(output_paths) = output_paths else {
    let _ = publish_and_record_cold_observation(
      &mut raw,
      "compiler_output_paths_unavailable",
      None,
      0,
      cache_bytes_read,
      0,
      trace,
    );
    return status.code().unwrap_or(1);
  };
  if stdout_len.is_none_or(|bytes| bytes > MAX_STREAM_BYTES as u64)
    || stderr_len.is_none_or(|bytes| bytes > MAX_STREAM_BYTES as u64)
  {
    let _ = publish_and_record_cold_observation(
      &mut raw,
      "compiler_stream_limit_exceeded",
      None,
      0,
      cache_bytes_read,
      0,
      trace,
    );
    return status.code().unwrap_or(1);
  }
  let Some(cache_context) = active_context() else {
    let _ = publish_and_record_cold_observation(
      &mut raw,
      "native_cache_context_unavailable",
      None,
      0,
      cache_bytes_read,
      0,
      trace,
    );
    return status.code().unwrap_or(1);
  };
  let source_root = &cache_context.source_root;
  let session = NativeCompilerSession::load(&cache_context.session, source_root);
  let session = match session {
    Ok(session) => session,
    Err(_) => {
      let _ = publish_and_record_cold_observation(
        &mut raw,
        "native_cache_session_unavailable",
        None,
        0,
        cache_bytes_read,
        0,
        trace,
      );
      return status.code().unwrap_or(1);
    }
  };
  let post_execution_bytes = match validate_post_execution_inputs(&raw, source_root, &session.class.host_target) {
    Ok(bytes_hashed) => bytes_hashed,
    Err((reason, bytes_hashed)) => {
      let bytes_hashed = cold_input_bytes(&raw, source_root, bytes_hashed);
      let _ = publish_and_record_cold_observation(&mut raw, reason, None, bytes_hashed, cache_bytes_read, 0, trace);
      return status.code().unwrap_or(1);
    }
  };
  let stdout = match read_bounded(stdout_file.path(), MAX_STREAM_BYTES) {
    Ok(bytes) => bytes,
    Err(_) => {
      let _ = publish_and_record_cold_observation(
        &mut raw,
        "compiler_stdout_unavailable",
        None,
        0,
        cache_bytes_read,
        0,
        trace,
      );
      return status.code().unwrap_or(1);
    }
  };
  let stderr = match read_bounded(stderr_file.path(), MAX_STREAM_BYTES) {
    Ok(bytes) => bytes,
    Err(_) => {
      let _ = publish_and_record_cold_observation(
        &mut raw,
        "compiler_stderr_unavailable",
        None,
        0,
        cache_bytes_read,
        0,
        trace,
      );
      return status.code().unwrap_or(1);
    }
  };
  let publication = publish_cold_result(&session, &raw, &output_paths, &stdout, &stderr, source_root);
  match publication {
    Ok((validation, written)) => {
      let initial = raw.cache_wrapper.clone().or_else(metadata_from_environment);
      let reason = initial
        .as_ref()
        .map(CompilerCacheWrapperMetadata::reason)
        .unwrap_or("candidate_not_found");
      let bytes_hashed = cold_input_bytes(&raw, source_root, post_execution_bytes);
      raw.cache_wrapper = Some(CompilerCacheWrapperMetadata::native(
        CompilerCacheWrapperStatus::Miss,
        format!("{reason};stored_verified_result"),
        Some(validation.candidate_key.clone()),
        Some(validation.action_key.clone()),
        bytes_hashed,
        0,
      ));
      write_cache_event(
        CompilerCacheWrapperStatus::Miss,
        "stored_verified_result",
        Some(&validation.candidate_key),
        Some(&validation.action_key),
        NativeCacheMetrics {
          bytes_hashed,
          cache_bytes_read,
          cache_bytes_written: written,
          bytes_restored: 0,
        },
        trace,
      );
    }
    Err(_) => {
      let initial = raw.cache_wrapper.clone().or_else(metadata_from_environment);
      let reason = initial.as_ref().map(CompilerCacheWrapperMetadata::reason).map_or_else(
        || "local_cache_store_failed".to_string(),
        |reason| format!("{reason};local_cache_store_failed"),
      );
      let bytes_hashed = cold_input_bytes(&raw, source_root, post_execution_bytes);
      raw.cache_wrapper = Some(CompilerCacheWrapperMetadata::native(
        CompilerCacheWrapperStatus::Bypassed,
        &reason,
        initial
          .as_ref()
          .and_then(CompilerCacheWrapperMetadata::candidate_key)
          .map(str::to_string),
        None,
        bytes_hashed,
        0,
      ));
      write_cache_event(
        CompilerCacheWrapperStatus::Bypassed,
        &reason,
        initial.as_ref().and_then(CompilerCacheWrapperMetadata::candidate_key),
        None,
        NativeCacheMetrics {
          bytes_hashed,
          cache_bytes_read,
          ..NativeCacheMetrics::default()
        },
        trace,
      );
    }
  }
  let _ = crate::compiler::observation::publish_raw(&cache_context.observation_directory, &raw);
  status.code().unwrap_or(1)
}

fn run_without_store(
  mut command: Command,
  recorder: InvocationRecorder,
  cache_bytes_read: u64,
  trace: &mut NativeCacheWrapperTrace,
  context: &str,
  reason: &'static str,
) -> i32 {
  match command.status() {
    Ok(status) => {
      if let Ok(mut raw) = recorder.complete(status.success()) {
        let _ = publish_and_record_cold_observation(&mut raw, reason, None, 0, cache_bytes_read, 0, trace);
      }
      status.code().unwrap_or(1)
    }
    Err(error) => {
      eprintln!("{context}: failed to execute compiler: {error}");
      1
    }
  }
}

fn publish_and_record_cold_observation(
  raw: &mut RawCompilerInvocation,
  reason: &'static str,
  action_key: Option<String>,
  bytes_hashed: u64,
  cache_bytes_read: u64,
  bytes_restored: u64,
  trace: &NativeCacheWrapperTrace,
) -> RailResult<()> {
  publish_cold_observation(raw, reason, action_key, bytes_hashed, bytes_restored)?;
  let metadata = raw.cache_wrapper.as_ref();
  write_cache_event(
    CompilerCacheWrapperStatus::Bypassed,
    reason,
    metadata.and_then(CompilerCacheWrapperMetadata::candidate_key),
    metadata.and_then(CompilerCacheWrapperMetadata::action_key),
    NativeCacheMetrics {
      bytes_hashed,
      cache_bytes_read,
      cache_bytes_written: 0,
      bytes_restored,
    },
    trace,
  );
  Ok(())
}

fn publish_cold_observation(
  raw: &mut RawCompilerInvocation,
  reason: &'static str,
  action_key: Option<String>,
  bytes_hashed: u64,
  bytes_restored: u64,
) -> RailResult<()> {
  let initial = raw.cache_wrapper.clone().or_else(metadata_from_environment);
  raw.cache_wrapper = Some(CompilerCacheWrapperMetadata::native(
    CompilerCacheWrapperStatus::Bypassed,
    reason,
    initial
      .as_ref()
      .and_then(CompilerCacheWrapperMetadata::candidate_key)
      .map(str::to_string),
    action_key,
    bytes_hashed,
    bytes_restored,
  ));
  let directory = active_context()
    .map(|context| &context.observation_directory)
    .ok_or_else(|| RailError::message("compiler observation directory is unavailable"))?;
  crate::compiler::observation::publish_raw(directory, raw)
}

fn validate_post_execution_inputs(
  observation: &RawCompilerInvocation,
  source_root: &Path,
  host_target: &str,
) -> Result<u64, (&'static str, u64)> {
  if let Some(reason) = invocation_bypass_reason(observation, true, host_target) {
    return Err((reason, 0));
  }
  match dep_info_contains_source_root(observation, source_root) {
    Ok(true) => return Err(("root_bound_dep_info_not_graduated", 0)),
    Ok(false) => {}
    Err(_) => return Err(("dep_info_portability_unavailable", 0)),
  }
  let mut bytes_hashed = 0u64;
  for file in observation
    .declared_inputs
    .iter()
    .chain(&observation.observed_reads)
    .chain(observation.dependency_artifacts.iter().map(|(_, file)| file))
  {
    if revalidate_file(file, source_root, &mut bytes_hashed).is_err() {
      return Err(("compiler_input_changed_during_execution", bytes_hashed));
    }
  }
  Ok(bytes_hashed)
}

fn dep_info_contains_source_root(observation: &RawCompilerInvocation, source_root: &Path) -> RailResult<bool> {
  let canonical_root = crate::utils::canonicalize_existing(source_root)?;
  let roots = remap_source_root_spellings(source_root, &canonical_root)
    .map_err(|reason| RailError::message(format!("compiler source root is not portable: {reason}")))?;
  let spellings = roots
    .iter()
    .flat_map(|root| {
      let mut escaped = Vec::with_capacity(root.len());
      for byte in root.bytes() {
        if byte.is_ascii_whitespace() || matches!(byte, b'\\' | b'#' | b':') {
          escaped.push(b'\\');
        }
        escaped.push(byte);
      }
      [root.as_bytes().to_vec(), escaped]
    })
    .collect::<BTreeSet<_>>();
  let dep_info = observation
    .emitted_outputs
    .iter()
    .find(|output| output.path.resolve(source_root).extension() == Some(OsStr::new("d")))
    .ok_or_else(|| RailError::message("compiler dep-info output is unavailable"))?;
  let bytes = fs::read(dep_info.path.resolve(source_root))?;
  Ok(
    spellings
      .iter()
      .any(|root| bytes.windows(root.len()).any(|window| window == root)),
  )
}

fn publish_cold_result(
  session: &NativeCompilerSession,
  observation: &RawCompilerInvocation,
  output_paths: &NativeOutputPaths,
  stdout: &[u8],
  stderr: &[u8],
  source_root: &Path,
) -> RailResult<(NativeCompilerValidation, u64)> {
  validated_output_parent(output_paths, source_root)?;
  let bindings = native_output_bindings(output_paths);
  // Rustc reports artifact paths with platform-specific spelling. Store one
  // canonical path token so a verified result can be late-bound to a different
  // Cargo output directory (and workspace root) on restore.
  let stdout = portable_stream_output_bindings(stdout, output_paths, source_root)?;
  let stderr = portable_stream_output_bindings(stderr, output_paths, source_root)?;
  let dep_info_observation = observed_output(observation, &output_paths.dep_info, source_root)?;
  let dep_info_bytes = read_bounded(
    &output_paths.dep_info,
    usize::try_from(fs::metadata(&output_paths.dep_info)?.len()).unwrap_or(usize::MAX),
  )?;
  if digest(&dep_info_bytes) != dep_info_observation.content_digest {
    return Err(RailError::message(
      "native compiler dep-info changed before canonical cache staging",
    ));
  }
  let portable_dep_info = portable_dep_info_output_bindings(&dep_info_bytes, output_paths, source_root)?;
  let mut cache_observation = observation.clone();
  let cached_dep_info = cache_observation
    .emitted_outputs
    .iter_mut()
    .find(|output| output.path == dep_info_observation.path)
    .ok_or_else(|| RailError::message("native compiler dep-info observation disappeared"))?;
  cached_dep_info.content_digest = digest(&portable_dep_info);
  let outputs = bindings
    .iter()
    .map(|(role, slot, path)| {
      let observed = observed_output(&cache_observation, path, source_root)?;
      let bytes = if *role == "dep_info" {
        portable_dep_info.len() as u64
      } else {
        fs::metadata(path)?.len()
      };
      Ok(NativeCompilerOutput {
        role: (*role).to_string(),
        slot: (*slot).to_string(),
        content_digest: observed.content_digest.clone(),
        bytes,
      })
    })
    .collect::<RailResult<Vec<_>>>()?;
  let validation =
    NativeCompilerValidation::new(session, cache_observation, outputs, digest(&stdout), digest(&stderr))?;
  if observation
    .cache_wrapper
    .as_ref()
    .and_then(CompilerCacheWrapperMetadata::candidate_key)
    .is_some_and(|candidate| candidate != validation.candidate_key)
  {
    return Err(RailError::message(
      "cold compiler observation does not match the candidate selected by the outer wrapper",
    ));
  }

  let staging = tempfile::tempdir()?;
  let stdout_slot = staging.path().join(STDOUT_SLOT);
  let stderr_slot = staging.path().join(STDERR_SLOT);
  let staged_outputs = bindings
    .iter()
    .map(|(_, slot, _)| staging.path().join(slot))
    .collect::<Vec<_>>();
  for directory in staged_outputs
    .iter()
    .filter_map(|path| path.parent())
    .chain([stdout_slot.parent(), stderr_slot.parent()].into_iter().flatten())
  {
    fs::create_dir_all(directory)?;
  }
  for (((role, _, source), staged), expected) in bindings.iter().zip(&staged_outputs).zip(&validation.outputs) {
    let observed = observed_output(&validation.observation, source, source_root)?;
    if *role == "dep_info" {
      write_new_file(staged, &portable_dep_info)?;
    } else {
      copy_regular_file(source, staged, expected.bytes)?;
    }
    validate_staged_output(staged, observed, expected.bytes)?;
  }
  write_new_file(&stdout_slot, &stdout)?;
  write_new_file(&stderr_slot, &stderr)?;
  let mut manifest_paths = staged_outputs;
  manifest_paths.extend([stdout_slot, stderr_slot]);
  let manifest = crate::hermetic::capture_native_compiler_outputs(staging.path(), &manifest_paths)?;
  let result = validation.result_digest(manifest.digest());
  let cas = LocalCas::open()?;
  let stats = cas.store_native(NativeStoreRequest {
    action_key: validation.action_key(),
    candidate_key: validation.candidate_key(),
    result_digest: &result,
    manifest: &manifest,
    validation: &validation,
    source_root: staging.path(),
  })?;
  Ok((validation, stats.bytes_written))
}

fn observed_output<'a>(
  observation: &'a RawCompilerInvocation,
  path: &Path,
  source_root: &Path,
) -> RailResult<&'a FileObservation> {
  let expected = ObservationPath::capture(path, source_root, source_root);
  observation
    .emitted_outputs
    .iter()
    .find(|output| output.path == expected)
    .ok_or_else(|| RailError::message(format!("compiler output '{}' was not observed", path.display())))
}

fn copy_regular_file(source: &Path, destination: &Path, expected_bytes: u64) -> RailResult<()> {
  let before = fs::symlink_metadata(source)?;
  if !before.is_file() || crate::utils::is_symlink_or_reparse(&before) || !single_link(&before) {
    return Err(RailError::message(format!(
      "compiler output '{}' is not a single-link regular file",
      source.display()
    )));
  }
  let input = File::open(source)?;
  let mut output = OpenOptions::new().write(true).create_new(true).open(destination)?;
  let copied = std::io::copy(&mut input.take(expected_bytes.saturating_add(1)), &mut output)?;
  output.sync_all()?;
  let after = fs::symlink_metadata(source)?;
  if copied != expected_bytes || before.len() != after.len() || before.modified()? != after.modified()? {
    return Err(RailError::message(format!(
      "compiler output '{}' changed during cache staging",
      source.display()
    )));
  }
  Ok(())
}

fn validate_staged_output(path: &Path, expected: &FileObservation, expected_bytes: u64) -> RailResult<()> {
  let metadata = fs::symlink_metadata(path)?;
  let staged = FileObservation::capture(path, path.parent().unwrap_or(Path::new("/")), Path::new("/"))?;
  if metadata.len() != expected_bytes
    || staged.content_digest != expected.content_digest
    || staged.executable != expected.executable
    || staged.symlink_target.is_some()
  {
    return Err(RailError::message(
      "staged compiler output does not match the post-compile digest",
    ));
  }
  Ok(())
}

fn write_new_file(path: &Path, bytes: &[u8]) -> RailResult<()> {
  let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
  file.write_all(bytes)?;
  file.sync_all()?;
  Ok(())
}

fn estimated_observed_read_bytes(observation: &RawCompilerInvocation, source_root: &Path) -> u64 {
  observation
    .observed_reads
    .iter()
    .filter_map(|file| fs::metadata(file.path.resolve(source_root)).ok())
    .fold(0u64, |total, metadata| total.saturating_add(metadata.len()))
}

fn cold_input_bytes(observation: &RawCompilerInvocation, source_root: &Path, post_execution_bytes: u64) -> u64 {
  observation
    .cache_wrapper
    .clone()
    .or_else(metadata_from_environment)
    .as_ref()
    .map(CompilerCacheWrapperMetadata::bytes_hashed)
    .unwrap_or_default()
    .saturating_add(estimated_observed_read_bytes(observation, source_root))
    .saturating_add(post_execution_bytes)
}

#[derive(Serialize)]
struct NativeCacheEvent<'a> {
  version: u32,
  status: CompilerCacheWrapperStatus,
  reason: &'a str,
  candidate_key: Option<&'a str>,
  action_key: Option<&'a str>,
  bytes_hashed: u64,
  cache_bytes_read: u64,
  cache_bytes_written: u64,
  bytes_restored: u64,
  #[serde(skip_serializing_if = "Option::is_none")]
  wrapper_trace: Option<NativeCacheWrapperTraceSnapshot>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnedNativeCacheEvent {
  version: u32,
  status: CompilerCacheWrapperStatus,
  reason: String,
  candidate_key: Option<String>,
  action_key: Option<String>,
  bytes_hashed: u64,
  cache_bytes_read: u64,
  cache_bytes_written: u64,
  bytes_restored: u64,
  wrapper_trace: Option<NativeCacheWrapperTraceSnapshot>,
}

fn write_cache_event(
  status: CompilerCacheWrapperStatus,
  reason: &str,
  candidate_key: Option<&str>,
  action_key: Option<&str>,
  metrics: NativeCacheMetrics,
  trace: &NativeCacheWrapperTrace,
) {
  let Some(directory) = active_context().map(|context| context.observation_directory.join("native-cache-events"))
  else {
    return;
  };
  if fs::create_dir_all(&directory).is_err() {
    return;
  }
  let event = NativeCacheEvent {
    version: NATIVE_CACHE_RUN_EVENT_VERSION,
    status,
    reason,
    candidate_key,
    action_key,
    bytes_hashed: metrics.bytes_hashed,
    cache_bytes_read: metrics.cache_bytes_read,
    cache_bytes_written: metrics.cache_bytes_written,
    bytes_restored: metrics.bytes_restored,
    wrapper_trace: trace.snapshot(),
  };
  let Ok(bytes) = serde_json::to_vec(&event) else {
    return;
  };
  let reason_slug = reason
    .bytes()
    .map(|byte| {
      if byte.is_ascii_alphanumeric() || byte == b'_' {
        byte as char
      } else {
        '_'
      }
    })
    .collect::<String>();
  let stem = format!("event-{}-{}-{reason_slug}", std::process::id(), status.as_str());
  write_unique_cache_event(&directory, &stem, &bytes);
}

fn write_unique_cache_event(directory: &Path, stem: &str, bytes: &[u8]) {
  for collision in 0..1_024 {
    let suffix = if collision == 0 {
      String::new()
    } else {
      format!("-{collision}")
    };
    let path = directory.join(format!("{stem}{suffix}.json"));
    match OpenOptions::new().write(true).create_new(true).open(path) {
      Ok(mut file) => {
        let _ = file.write_all(bytes);
        return;
      }
      Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
      Err(_) => return,
    }
  }
}

pub(crate) fn validate_candidate_key(value: &str) -> RailResult<()> {
  validate_identity(value, CANDIDATE_KEY_PREFIX).map(|_| ())
}

pub(crate) fn validate_action_key(value: &str) -> RailResult<()> {
  validate_identity(value, ACTION_KEY_PREFIX).map(|_| ())
}

fn validate_sha256(value: &str) -> RailResult<()> {
  validate_identity(value, "sha256:").map(|_| ())
}

fn validate_identity<'a>(value: &'a str, prefix: &str) -> RailResult<&'a str> {
  let hex = value
    .strip_prefix(prefix)
    .ok_or_else(|| RailError::message("native compiler identity has the wrong domain or version"))?;
  if hex.len() != 64
    || !hex
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
  {
    return Err(RailError::message("native compiler identity is not canonical SHA-256"));
  }
  Ok(hex)
}

fn append_frame(output: &mut Vec<u8>, tag: &[u8], value: &[u8]) {
  output.extend_from_slice(&(tag.len() as u64).to_le_bytes());
  output.extend_from_slice(tag);
  output.extend_from_slice(&(value.len() as u64).to_le_bytes());
  output.extend_from_slice(value);
}

#[cfg(test)]
pub(crate) mod tests {
  use super::*;
  use crate::compiler::observation::EnvironmentObservation;

  #[test]
  fn bundled_codegen_backends_are_supported_but_external_paths_are_not() {
    for option in [
      "codegen-backend=llvm",
      "codegen-backend=cranelift",
      "codegen-backend=my_backend-2",
    ] {
      assert!(supported_unstable_option(option), "rejected {option}");
    }
    for option in [
      "codegen-backend=",
      "codegen-backend=/opt/backend.so",
      "codegen-backend=C:\\backend.dll",
      "mir-opt-level=4",
    ] {
      assert!(!supported_unstable_option(option), "accepted {option}");
    }

    assert!(long_option_selected(
      &["--sysroot=/opt/toolchain".to_string()],
      "--sysroot"
    ));
    assert!(long_option_selected(
      &["--sysroot".to_string(), "/opt/toolchain".to_string()],
      "--sysroot"
    ));
    assert!(!long_option_selected(
      &["--sysroot-metadata=/opt/toolchain".to_string()],
      "--sysroot"
    ));
  }

  fn observed_file(path: &str, bytes: &[u8]) -> FileObservation {
    FileObservation {
      path: ObservationPath::Repository(path.to_string()),
      content_digest: digest(bytes),
      executable: false,
      symlink_target: None,
    }
  }

  fn graduated_observation() -> RawCompilerInvocation {
    let source = observed_file("src/lib.rs", b"pub fn value() -> u8 { 1 }\n");
    RawCompilerInvocation {
      version: 4,
      mode: CompilerMode::Rustc,
      crate_name: Some("fixture".to_string()),
      crate_types: BTreeSet::from(["lib".to_string()]),
      target_argument: None,
      cfg: BTreeSet::new(),
      emit_modes: BTreeSet::from(["dep-info".to_string(), "metadata".to_string()]),
      test_mode: false,
      compiler_arguments: [
        "--crate-name",
        "fixture",
        "--edition=2024",
        "src/lib.rs",
        "--crate-type",
        "lib",
        "--emit=dep-info,metadata",
        "-C",
        "metadata=0123456789abcdef",
        "-Cextra-filename=-0123456789abcdef",
        "--out-dir",
        "target/debug/deps",
        "--remap-path-prefix=repository:=/cargo-rail/workspace",
      ]
      .into_iter()
      .map(str::to_string)
      .collect(),
      declared_inputs: vec![source.clone()],
      observed_reads: vec![source],
      dependency_artifacts: Vec::new(),
      emitted_outputs: vec![
        observed_file("target/debug/deps/fixture-0123456789abcdef.d", b"dep-info"),
        observed_file("target/debug/deps/libfixture-0123456789abcdef.rmeta", b"metadata"),
      ],
      environment_reads: BTreeSet::new(),
      compiler: None,
      wrappers: Vec::new(),
      cache_wrapper: None,
      success: true,
      bypasses: BTreeSet::new(),
    }
  }

  fn graduated_session(source_root_identity: String) -> NativeCompilerSession {
    let class = NativeCompilerClass {
      name: "library_metadata_rlib".to_string(),
      platform: "unix-test-x86_64".to_string(),
      host_target: "x86_64-unknown-test".to_string(),
      rustc_release: "1.97.1".to_string(),
      cargo_release: "1.97.1".to_string(),
    };
    let capability_identity = digest(b"toolchain-capability");
    let toolchain_identity = digest(b"toolchain");
    let compiler_process_environment_identity = digest(b"compiler-process-environment");
    let execution_contract = DIAGNOSTIC_EXECUTION_CONTRACT.to_string();
    let identity = session_identity(
      &class,
      &capability_identity,
      &toolchain_identity,
      &compiler_process_environment_identity,
      &execution_contract,
    )
    .expect("session identity");
    NativeCompilerSession {
      version: 4,
      identity,
      source_root_identity,
      class,
      capability_identity,
      toolchain_identity,
      compiler_process_environment_identity,
      execution_contract,
    }
  }

  fn graduated_validation(observation: RawCompilerInvocation) -> NativeCompilerValidation {
    let session = graduated_session(digest(b"source-root"));
    let outputs = vec![
      NativeCompilerOutput {
        role: "dep_info".to_string(),
        slot: DEP_INFO_SLOT.to_string(),
        content_digest: observation.emitted_outputs[0].content_digest.clone(),
        bytes: 8,
      },
      NativeCompilerOutput {
        role: "metadata".to_string(),
        slot: METADATA_SLOT.to_string(),
        content_digest: observation.emitted_outputs[1].content_digest.clone(),
        bytes: 8,
      },
    ];
    NativeCompilerValidation::new(&session, observation, outputs, digest(b""), digest(b""))
      .expect("graduated validation")
  }

  pub(crate) fn cas_validation() -> NativeCompilerValidation {
    graduated_validation(graduated_observation())
  }

  #[test]
  fn candidate_never_contains_discovered_authority() {
    let session = graduated_session(digest(b"source-root"));
    let base = graduated_observation();
    let mut environment_changed = base.clone();
    environment_changed.environment_reads.insert(EnvironmentObservation {
      name: "P73_VALUE".to_string(),
      value_digest: Some(digest(b"one")),
      secret_capability: false,
    });
    let mut observed_changed = base.clone();
    observed_changed.observed_reads[0].content_digest = digest(b"different observed bytes");

    let candidate = candidate_key(&session.identity, &session.class, &base).expect("candidate");
    assert_eq!(
      candidate,
      candidate_key(&session.identity, &session.class, &environment_changed,).expect("environment candidate")
    );
    assert_eq!(
      candidate,
      candidate_key(&session.identity, &session.class, &observed_changed,).expect("observed candidate")
    );
    assert_ne!(
      action_key(&session.identity, &session.class, &base).expect("action"),
      action_key(&session.identity, &session.class, &environment_changed,).expect("environment action")
    );
    assert_ne!(
      action_key(&session.identity, &session.class, &base).expect("action"),
      action_key(&session.identity, &session.class, &observed_changed,).expect("observed action")
    );
  }

  #[test]
  fn current_candidate_capture_hashes_bytes_instead_of_trusting_size() {
    let root = tempfile::tempdir().expect("source root");
    fs::create_dir(root.path().join("src")).expect("source directory");
    let source = root.path().join("src/lib.rs");
    fs::write(&source, b"pub const VALUE: u8 = 1;\n").expect("source");
    let captured = FileObservation::capture(&source, root.path(), root.path()).expect("source observation");
    let mut observation = graduated_observation();
    observation.declared_inputs = vec![captured.clone()];
    observation.observed_reads = vec![captured];
    let session = graduated_session(path_identity(root.path()).expect("root identity"));
    let validation = NativeCompilerValidation::new(
      &session,
      observation.clone(),
      graduated_validation(observation.clone()).outputs,
      digest(b""),
      digest(b""),
    )
    .expect("validation");
    let revalidated =
      revalidate_candidate(&validation, &session, &observation, root.path()).expect("unchanged candidate");
    assert_eq!(revalidated.action_key, validation.action_key);
    assert_eq!(revalidated.bytes_hashed, 0, "declared inputs were already captured");

    fs::write(&source, b"pub const VALUE: u8 = 2;\n").expect("same-size mutation");
    let changed = FileObservation::capture(&source, root.path(), root.path()).expect("changed source observation");
    let mut current = observation;
    current.declared_inputs = vec![changed];
    let error = revalidate_candidate(&validation, &session, &current, root.path())
      .expect_err("same-size content mutation must miss");
    assert_eq!(error.0, "candidate_pre_execution_inputs_changed");
  }

  #[test]
  fn cold_publication_revalidates_dependency_bytes_after_compilation() {
    let root = tempfile::tempdir().expect("source root");
    fs::create_dir(root.path().join("src")).expect("source directory");
    fs::create_dir(root.path().join("target")).expect("target directory");
    let source = root.path().join("src/lib.rs");
    let dependency = root.path().join("target/libdependency.rmeta");
    fs::write(&source, b"pub fn value() {}\n").expect("source");
    fs::write(&dependency, b"dependency-one").expect("dependency");
    let expected_bytes = fs::metadata(&source).expect("source metadata").len() * 2
      + fs::metadata(&dependency).expect("dependency metadata").len();
    let source = FileObservation::capture(&source, root.path(), root.path()).expect("source observation");
    let dependency_observation =
      FileObservation::capture(&dependency, root.path(), root.path()).expect("dependency observation");
    let mut observation = graduated_observation();
    observation.declared_inputs = vec![source.clone()];
    observation.observed_reads = vec![source];
    observation.dependency_artifacts = vec![("dependency".to_string(), dependency_observation)];
    let dep_info = observation.emitted_outputs[0].path.resolve(root.path());
    fs::create_dir_all(dep_info.parent().expect("dep-info parent")).expect("dep-info directory");
    fs::write(
      dep_info,
      b"target/debug/deps/libfixture.rmeta: src/lib.rs target/libdependency.rmeta\n",
    )
    .expect("portable dep-info");

    let bytes =
      validate_post_execution_inputs(&observation, root.path(), "aarch64-apple-darwin").expect("stable inputs");
    assert_eq!(bytes, expected_bytes);

    fs::write(&dependency, b"dependency-two").expect("same-size dependency mutation");
    let error = validate_post_execution_inputs(&observation, root.path(), "aarch64-apple-darwin")
      .expect_err("changed dependency must prevent publication");
    assert_eq!(error.0, "compiler_input_changed_during_execution");
  }

  #[test]
  fn only_the_exact_compiler_class_is_graduated() {
    let session = graduated_session(digest(b"source-root"));
    let baseline = graduated_observation();
    assert_eq!(
      invocation_bypass_reason(&baseline, true, &session.class.host_target),
      None
    );

    let assert_bypass = |expected, mutate: fn(&mut RawCompilerInvocation)| {
      let mut observation = baseline.clone();
      mutate(&mut observation);
      assert_eq!(
        invocation_bypass_reason(&observation, true, &session.class.host_target),
        Some(expected),
        "{expected}"
      );
    };
    let mut explicit_host = baseline.clone();
    explicit_host.target_argument = Some(session.class.host_target.clone());
    explicit_host
      .compiler_arguments
      .push(format!("--target={}", session.class.host_target));
    assert_eq!(
      invocation_bypass_reason(&explicit_host, true, &session.class.host_target),
      None
    );
    let mut bundled_backend = baseline.clone();
    bundled_backend
      .compiler_arguments
      .push("-Zcodegen-backend=cranelift".to_string());
    assert_eq!(
      invocation_bypass_reason(&bundled_backend, true, &session.class.host_target),
      None
    );
    assert_bypass("rustdoc_not_graduated", |value| value.mode = CompilerMode::Rustdoc);
    assert_bypass("cross_target_not_graduated", |value| {
      value.target_argument = Some("x86_64-unknown-linux-gnu".to_string());
    });
    assert_bypass("test_compilation_not_graduated", |value| value.test_mode = true);
    assert_bypass("proc_macro_not_graduated", |value| {
      value.crate_types = BTreeSet::from(["proc-macro".to_string()]);
    });
    assert_bypass("linker_producing_crate_type_not_graduated", |value| {
      value.crate_types = BTreeSet::from(["cdylib".to_string()]);
    });
    assert_bypass("binary_not_graduated", |value| {
      value.crate_types = BTreeSet::from(["bin".to_string()]);
    });
    assert_bypass("build_script_not_graduated", |value| {
      value.crate_types = BTreeSet::from(["bin".to_string()]);
      value.crate_name = Some("build_script_build".to_string());
    });
    assert_bypass("compiler_emit_mode_not_graduated", |value| {
      value.emit_modes.insert("llvm-bc".to_string());
    });
    assert_bypass("compiler_stdin_not_graduated", |value| {
      value.compiler_arguments.push("-".to_string());
    });
    assert_bypass("native_linking_not_graduated", |value| {
      value
        .compiler_arguments
        .extend(["-L".to_string(), "native=/tmp".to_string()]);
    });
    assert_bypass("incremental_compilation_not_graduated", |value| {
      value
        .compiler_arguments
        .extend(["-C".to_string(), "incremental=target/incremental".to_string()]);
    });
    assert_bypass("compiler_flag_not_graduated", |value| {
      value.compiler_arguments.push("-Zunproven".to_string());
    });
    assert_bypass("compiler_flag_not_graduated", |value| {
      value
        .compiler_arguments
        .push("-Zcodegen-backend=/opt/backend.so".to_string());
    });
    assert_bypass("dependency_artifact_class_not_graduated", |value| {
      value.dependency_artifacts.push((
        "dep".to_string(),
        observed_file("target/debug/deps/libdep.dylib", b"dylib"),
      ));
    });
    assert_bypass("secret_compiler_environment", |value| {
      value.environment_reads.insert(EnvironmentObservation {
        name: "TOKEN".to_string(),
        value_digest: Some(digest(b"redacted")),
        secret_capability: true,
      });
    });
    assert_bypass("compiler_inputs_incomplete", |value| {
      value.bypasses.insert("unknown_input".to_string());
    });
    assert_bypass("declared_compiler_inputs_unavailable", |value| {
      value.declared_inputs.clear();
    });
    assert_bypass("complete_compiler_observation_unavailable", |value| {
      value.observed_reads.clear();
    });
  }

  #[cfg(target_os = "macos")]
  #[test]
  fn portable_proc_macro_execution_uses_a_unique_non_rooted_install_name() {
    let mut observation = graduated_observation();
    observation.crate_name = Some("fixture_macros".to_string());
    observation.crate_types = BTreeSet::from(["proc-macro".to_string()]);
    observation.emit_modes = BTreeSet::from(["dep-info".to_string(), "link".to_string()]);
    let arguments = observation
      .compiler_arguments
      .iter()
      .map(OsString::from)
      .collect::<Vec<_>>();

    let portable = portable_macos_bypass_arguments("proc_macro_not_graduated", &arguments, &observation)
      .expect("portable proc-macro arguments");
    assert_eq!(
      portable[portable.len() - 2..],
      [
        OsString::from("-C"),
        OsString::from("link-arg=-Wl,-install_name,@rpath/libfixture_macros-0123456789abcdef.dylib")
      ]
    );
    assert_eq!(
      invocation_bypass_reason(&observation, false, "aarch64-apple-darwin"),
      Some("proc_macro_not_graduated")
    );

    let mut explicit_linker = arguments.clone();
    explicit_linker.push(OsString::from("-Clinker=/usr/bin/cc"));
    assert!(
      portable_macos_proc_macro_arguments(&explicit_linker, &observation).is_none(),
      "explicit linker control must retain Cargo's exact compiler argv"
    );

    let mut consumer = graduated_observation();
    consumer.dependency_artifacts.push((
      "fixture_macros".to_string(),
      observed_file(
        "target/debug/deps/libfixture_macros-0123456789abcdef.dylib",
        b"proc-macro",
      ),
    ));
    assert_eq!(
      portable_macos_bypass_arguments("dependency_artifact_class_not_graduated", &arguments, &consumer,),
      Some(arguments.clone())
    );

    consumer.dependency_artifacts[0].1 =
      observed_file("target/debug/deps/libfixture_macros-0123456789abcdef.so", b"plugin");
    assert!(
      portable_macos_bypass_arguments("dependency_artifact_class_not_graduated", &arguments, &consumer,).is_none(),
      "unreviewed dynamic dependency classes must retain Cargo's exact compiler argv"
    );
  }

  #[test]
  fn validation_rejects_forged_output_bindings() {
    let validation = graduated_validation(graduated_observation());
    validation.validate_object().expect("baseline validation");

    let mut duplicate_slot = validation.clone();
    duplicate_slot.outputs[1].slot = DEP_INFO_SLOT.to_string();
    assert!(duplicate_slot.validate_object().is_err());

    let mut forged_digest = validation.clone();
    forged_digest.outputs[0].content_digest = digest(b"forged");
    assert!(forged_digest.validate_object().is_err());

    let mut forged_action = validation;
    forged_action.action_key = format!("{ACTION_KEY_PREFIX}{}", "0".repeat(64));
    assert!(forged_action.validate_object().is_err());
  }

  #[test]
  fn publication_root_must_remain_inside_the_source_root() {
    let source = tempfile::tempdir().expect("source root");
    let external = tempfile::tempdir().expect("external root");
    let internal = source.path().join("target/debug/deps");
    fs::create_dir_all(&internal).expect("internal target");
    let valid = NativeOutputPaths {
      dep_info: internal.join("fixture.d"),
      metadata: internal.join("libfixture.rmeta"),
      rlib: None,
    };
    assert!(validated_output_parent(&valid, source.path()).is_ok());

    let escaped = NativeOutputPaths {
      dep_info: external.path().join("fixture.d"),
      metadata: external.path().join("libfixture.rmeta"),
      rlib: None,
    };
    assert!(validated_output_parent(&escaped, source.path()).is_err());
  }

  #[test]
  fn publication_rehashes_staged_bytes_before_exposure() {
    let directory = tempfile::tempdir().expect("publication directory");
    let source = directory.path().join("restored.d");
    let destination = directory.path().join("published.d");
    fs::write(&source, b"forged!").expect("forged restored output");
    let expected = NativeCompilerOutput {
      role: "dep_info".to_string(),
      slot: DEP_INFO_SLOT.to_string(),
      content_digest: digest(b"correct"),
      bytes: 7,
    };

    publish_output(&source, &destination, &expected).expect_err("same-size forged bytes must fail closed");

    assert!(!destination.exists());
  }

  #[test]
  fn physical_source_roots_in_dep_info_are_not_portable() {
    let directory = tempfile::tempdir().expect("source root parent");
    let source_root = directory.path().join("workspace root");
    let output = source_root.join("target/debug/deps/fixture.d");
    fs::create_dir_all(output.parent().expect("dep-info parent")).expect("dep-info directory");
    let escaped_root = source_root.to_string_lossy().replace(' ', "\\ ");
    fs::write(
      &output,
      format!("target/debug/deps/fixture.rmeta: {escaped_root}/target/generated.rs\n"),
    )
    .expect("root-bound dep-info");
    let mut observation = graduated_observation();
    observation.emitted_outputs[0] =
      FileObservation::capture(&output, &source_root, &source_root).expect("dep-info observation");

    assert!(dep_info_contains_source_root(&observation, &source_root).expect("root-bound check"));

    fs::write(&output, "target/debug/deps/fixture.rmeta: target/generated.rs\n").expect("portable dep-info");
    assert!(!dep_info_contains_source_root(&observation, &source_root).expect("portable check"));
  }

  #[cfg(any(unix, windows))]
  #[test]
  fn publication_moves_the_verified_staging_file_without_copying_it() {
    let directory = tempfile::tempdir().expect("publication directory");
    let source = directory.path().join("restored.d");
    let destination = directory.path().join("published.d");
    fs::write(&source, b"correct").expect("verified restored output");
    let expected = NativeCompilerOutput {
      role: "dep_info".to_string(),
      slot: DEP_INFO_SLOT.to_string(),
      content_digest: digest(b"correct"),
      bytes: 7,
    };

    publish_output(&source, &destination, &expected).expect("verified publication");

    assert!(!source.exists(), "the private staging link should be released");
    assert_eq!(fs::read(&destination).expect("published bytes"), b"correct");
    assert!(single_link(
      &fs::symlink_metadata(&destination).expect("published metadata")
    ));
  }

  #[test]
  fn compiler_class_accepts_any_identified_host_and_release() {
    let session = graduated_session(digest(b"source-root"));
    assert!(session.class.is_valid());

    for (platform, host_target, release) in [
      ("unix-linux-aarch64", "aarch64-unknown-linux-gnu", "1.91.0"),
      ("unix-macos-x86_64", "x86_64-apple-darwin", "1.97.1"),
      ("windows-windows-x86_64", "x86_64-pc-windows-msvc", "1.98.0-nightly"),
      ("unix-freebsd-x86_64", "x86_64-unknown-freebsd", "custom"),
    ] {
      let mut class = session.class.clone();
      class.platform = platform.to_string();
      class.host_target = host_target.to_string();
      class.rustc_release = release.to_string();
      class.cargo_release = release.to_string();
      assert!(class.is_valid(), "{platform}/{host_target}/{release}");
    }

    let mut invalid = session.class;
    invalid.host_target = "unknown".to_string();
    assert!(!invalid.is_valid());
  }

  #[test]
  fn session_identity_changes_with_exact_toolchain_identity() {
    let session = graduated_session(digest(b"source-root"));
    let identity = |capability: &str, toolchain: &str, environment: &str, contract: &str| {
      session_identity(&session.class, capability, toolchain, environment, contract).expect("session identity")
    };
    assert_ne!(
      identity(
        &digest(b"changed-capability"),
        &session.toolchain_identity,
        &session.compiler_process_environment_identity,
        &session.execution_contract,
      ),
      session.identity
    );
    assert_ne!(
      identity(
        &session.capability_identity,
        &digest(b"changed-toolchain"),
        &session.compiler_process_environment_identity,
        &session.execution_contract,
      ),
      session.identity
    );
    assert_ne!(
      identity(
        &session.capability_identity,
        &session.toolchain_identity,
        &digest(b"changed-environment"),
        &session.execution_contract,
      ),
      session.identity
    );
    assert_ne!(
      identity(
        &session.capability_identity,
        &session.toolchain_identity,
        &session.compiler_process_environment_identity,
        DIRECT_EXECUTION_CONTRACT,
      ),
      session.identity
    );
  }

  #[test]
  fn session_validation_accepts_any_exact_capability_identity() {
    let mut session = graduated_session(digest(b"source-root"));
    let original_identity = session.identity.clone();
    session.capability_identity = digest(b"different-exact-toolchain");
    session.identity = session_identity(
      &session.class,
      &session.capability_identity,
      &session.toolchain_identity,
      &session.compiler_process_environment_identity,
      &session.execution_contract,
    )
    .expect("session identity");

    assert_ne!(session.identity, original_identity);
    session.validate_object().expect("exact toolchain identity");
  }

  #[test]
  fn direct_cache_never_reorders_or_replaces_an_existing_wrapper() {
    let source_root = tempfile::tempdir().expect("source root");
    for (plan, reason) in [
      (CacheWrapperPlan::PreserveSccache, "sccache_wrapper_preserved"),
      (
        CacheWrapperPlan::PreserveExisting,
        "existing_compiler_wrapper_preserved",
      ),
    ] {
      let setup = prepare_direct_cargo_cache(DirectNativeCacheIdentity {
        source_root: source_root.path(),
        rustc_version: "rustc 1.97.1 (test)\nhost: aarch64-apple-darwin\n",
        cargo_version: "cargo 1.97.1 (test)\n",
        toolchain_fingerprint: &digest(b"toolchain"),
        capability_identity: &digest(b"capability"),
        compiler_process_env_fingerprint: &digest(b"environment"),
        wrapper_plan: plan,
        setup_bytes_hashed: 0,
        retain_event_evidence: false,
      });
      assert_eq!(setup.bypass_reason(), Some(reason));
      assert!(setup.cargo_config_argument().is_none());
    }
  }

  #[test]
  fn direct_cache_report_retains_sorted_stable_unit_events() {
    let observations = tempfile::tempdir().expect("observations");
    let events = observations.path().join("native-cache-events");
    fs::create_dir(&events).expect("event directory");
    let mut observation = graduated_observation();
    observation.cache_wrapper = Some(CompilerCacheWrapperMetadata::native(
      CompilerCacheWrapperStatus::Miss,
      "stored_verified_result",
      Some("native-candidate-v1-sha256:aaaa".to_string()),
      Some("native-action-v1-sha256:1111".to_string()),
      10,
      0,
    ));
    fs::write(
      observations.path().join("rustc-1.json"),
      serde_json::to_vec(&observation).expect("observation JSON"),
    )
    .expect("observation");
    for (name, event) in [
      (
        "second.json",
        NativeCacheEvent {
          version: NATIVE_CACHE_RUN_EVENT_VERSION,
          status: CompilerCacheWrapperStatus::Hit,
          reason: "verified_local_result",
          candidate_key: Some("native-candidate-v1-sha256:bbbb"),
          action_key: Some("native-action-v1-sha256:2222"),
          bytes_hashed: 20,
          cache_bytes_read: 40,
          cache_bytes_written: 0,
          bytes_restored: 30,
          wrapper_trace: None,
        },
      ),
      (
        "first.json",
        NativeCacheEvent {
          version: NATIVE_CACHE_RUN_EVENT_VERSION,
          status: CompilerCacheWrapperStatus::Miss,
          reason: "stored_verified_result",
          candidate_key: Some("native-candidate-v1-sha256:aaaa"),
          action_key: Some("native-action-v1-sha256:1111"),
          bytes_hashed: 10,
          cache_bytes_read: 0,
          cache_bytes_written: 50,
          bytes_restored: 0,
          wrapper_trace: None,
        },
      ),
      (
        "old.json",
        NativeCacheEvent {
          version: NATIVE_CACHE_RUN_EVENT_VERSION - 1,
          status: CompilerCacheWrapperStatus::Hit,
          reason: "obsolete_run_event",
          candidate_key: None,
          action_key: None,
          bytes_hashed: u64::MAX,
          cache_bytes_read: u64::MAX,
          cache_bytes_written: u64::MAX,
          bytes_restored: u64::MAX,
          wrapper_trace: None,
        },
      ),
    ] {
      fs::write(events.join(name), serde_json::to_vec(&event).expect("event JSON")).expect("event");
    }
    let run = DirectNativeCacheRun {
      observations,
      cargo_config: OsString::new(),
      setup_bytes_hashed: 40,
    };

    let report = run.report();
    assert_eq!(report.hits, 1);
    assert_eq!(report.misses, 1);
    assert_eq!(report.setup_bytes_hashed, 40);
    assert_eq!(report.bytes_hashed, 30);
    assert_eq!(report.cache_bytes_read, 40);
    assert_eq!(report.cache_bytes_written, 50);
    assert_eq!(report.bytes_restored, 30);
    assert_eq!(report.events.len(), 2);
    assert_eq!(report.events[0].schema_version, NATIVE_CACHE_EVENT_EVIDENCE_VERSION);
    assert_eq!(
      report.events[0].unit_identity.as_deref(),
      Some("native-candidate-v1-sha256:aaaa")
    );
    assert_eq!(report.events[0].outcome, CompilerCacheWrapperStatus::Miss);
    let unit = report.events[0].unit.as_ref().expect("unit evidence");
    assert_eq!(unit.descriptor.crate_name.as_deref(), Some("fixture"));
    assert_eq!(
      unit.descriptor.crate_root,
      Some(ObservationPath::Repository("src/lib.rs".to_string()))
    );
    assert_eq!(
      unit.identity_inputs,
      native_cache_identity_inputs(&observation).expect("event identity inputs")
    );
    assert_eq!(
      unit.output_paths,
      observation
        .emitted_outputs
        .iter()
        .map(|output| output.path.clone())
        .collect::<Vec<_>>()
    );
    assert_eq!(unit.observed_outputs, observation.emitted_outputs);
    assert_eq!(unit.claimed_outputs.as_ref(), Some(&observation.emitted_outputs));
    assert_eq!(
      report.events[1].unit_identity.as_deref(),
      Some("native-candidate-v1-sha256:bbbb")
    );
    assert_eq!(report.events[1].outcome, CompilerCacheWrapperStatus::Hit);
  }

  #[test]
  fn native_cache_event_files_survive_reused_process_identifiers() {
    let directory = tempfile::tempdir().expect("event directory");
    write_unique_cache_event(directory.path(), "event-42-hit-verified_local_result", b"first");
    write_unique_cache_event(directory.path(), "event-42-hit-verified_local_result", b"second");

    let mut contents = fs::read_dir(directory.path())
      .expect("event files")
      .map(|entry| fs::read(entry.expect("event entry").path()).expect("event bytes"))
      .collect::<Vec<_>>();
    contents.sort();
    assert_eq!(contents, [b"first".to_vec(), b"second".to_vec()]);
  }

  #[test]
  fn session_identity_is_portable_but_each_session_file_is_root_bound() {
    let first = tempfile::tempdir().expect("first source root");
    let second = tempfile::tempdir().expect("second source root");
    let first_session = graduated_session(path_identity(first.path()).expect("first root identity"));
    let second_session = graduated_session(path_identity(second.path()).expect("second root identity"));
    assert_eq!(first_session.identity, second_session.identity);

    let session_file = first.path().join("session.json");
    fs::write(&session_file, serde_json::to_vec(&first_session).expect("session JSON")).expect("session file");
    NativeCompilerSession::load(&session_file, first.path()).expect("matching physical root");
    NativeCompilerSession::load(&session_file, second.path()).expect_err("replayed session file must fail closed");
  }

  #[test]
  fn metadata_and_rlib_outputs_share_one_verified_contract() {
    let session = graduated_session(digest(b"source-root"));
    let mut observation = graduated_observation();
    observation.emit_modes.insert("link".to_string());
    observation.compiler_arguments = observation
      .compiler_arguments
      .into_iter()
      .map(|argument| {
        if argument == "--emit=dep-info,metadata" {
          "--emit=dep-info,metadata,link".to_string()
        } else {
          argument
        }
      })
      .collect();
    observation.emitted_outputs.push(observed_file(
      "target/debug/deps/libfixture-0123456789abcdef.rlib",
      b"rlib",
    ));
    observation.emitted_outputs.sort();
    let outputs = [
      ("dep_info", DEP_INFO_SLOT, b"dep-info".as_slice()),
      ("metadata", METADATA_SLOT, b"metadata".as_slice()),
      ("rlib", RLIB_SLOT, b"rlib".as_slice()),
    ]
    .into_iter()
    .map(|(role, slot, bytes)| NativeCompilerOutput {
      role: role.to_string(),
      slot: slot.to_string(),
      content_digest: digest(bytes),
      bytes: bytes.len() as u64,
    })
    .collect();
    let validation = NativeCompilerValidation::new(&session, observation, outputs, digest(b""), digest(b""))
      .expect("metadata/rlib validation");
    validation.validate_object().expect("valid rlib binding");

    let mut forged = validation;
    forged.outputs[2].content_digest = digest(b"same-size-forgery");
    forged.validate_object().expect_err("rlib bytes remain action-bound");
  }

  #[test]
  fn every_pre_execution_mutation_changes_the_candidate_identity() {
    let session = graduated_session(digest(b"source-root"));
    let baseline = graduated_observation();
    let baseline_key = candidate_key(&session.identity, &session.class, &baseline).expect("baseline candidate");
    let assert_changed = |observation: RawCompilerInvocation, label: &str| {
      assert_ne!(
        candidate_key(&session.identity, &session.class, &observation).expect(label),
        baseline_key,
        "{label}"
      );
    };

    let mut source = baseline.clone();
    source.declared_inputs[0].content_digest = digest(b"changed source");
    assert_changed(source, "source");
    let mut dependency = baseline.clone();
    dependency.dependency_artifacts.push((
      "dep".to_string(),
      observed_file("target/debug/deps/libdep.rmeta", b"dependency"),
    ));
    assert_changed(dependency, "dependency");
    let mut feature = baseline.clone();
    feature.cfg.insert("feature=\"extended\"".to_string());
    feature
      .compiler_arguments
      .extend(["--cfg".to_string(), "feature=\"extended\"".to_string()]);
    assert_changed(feature, "feature");
    let mut cfg = baseline.clone();
    cfg.cfg.insert("cargo_rail_cfg".to_string());
    cfg
      .compiler_arguments
      .extend(["--cfg".to_string(), "cargo_rail_cfg".to_string()]);
    assert_changed(cfg, "cfg");
    let mut profile = baseline.clone();
    profile
      .compiler_arguments
      .extend(["-C".to_string(), "opt-level=3".to_string()]);
    assert_changed(profile, "profile/codegen");
    let mut flags = baseline;
    flags
      .compiler_arguments
      .extend(["--cap-lints".to_string(), "allow".to_string()]);
    assert_changed(flags, "flags");
  }

  #[test]
  fn output_and_dependency_directories_do_not_partition_candidate_identity() {
    let session = graduated_session(digest(b"source-root"));
    let mut first = graduated_observation();
    first.compiler_arguments.extend([
      "--extern".to_string(),
      "dep=target-one/debug/deps/libdep-0123456789abcdef.rmeta".to_string(),
      "-Ldependency=target-one/debug/deps".to_string(),
    ]);
    for argument in &mut first.compiler_arguments {
      if argument == "target/debug/deps" {
        *argument = "target-one/debug/deps".to_string();
      }
    }
    first.dependency_artifacts.push((
      "dep".to_string(),
      observed_file("target-one/debug/deps/libdep-0123456789abcdef.rmeta", b"dependency"),
    ));
    let mut second = first.clone();
    second.compiler_arguments = second
      .compiler_arguments
      .into_iter()
      .map(|argument| argument.replace("target-one", "target-two"))
      .collect();
    second.dependency_artifacts[0].1.path =
      ObservationPath::Repository("target-two/debug/deps/libdep-0123456789abcdef.rmeta".to_string());

    let first_key = candidate_key(&session.identity, &session.class, &first).expect("first candidate");
    assert_eq!(
      candidate_key(&session.identity, &session.class, &second).expect("second candidate"),
      first_key
    );

    let mut external_search = first.clone();
    external_search
      .compiler_arguments
      .push("-Ldependency=/outside/native-cache-search-one".to_string());
    let mut changed_external_search = external_search.clone();
    *changed_external_search
      .compiler_arguments
      .last_mut()
      .expect("external search") = "-Ldependency=/outside/native-cache-search-two".to_string();
    assert_ne!(
      candidate_key(&session.identity, &session.class, &external_search).expect("first external search candidate"),
      candidate_key(&session.identity, &session.class, &changed_external_search)
        .expect("changed external search candidate"),
      "only Cargo's exact output directory may be erased from dependency search identity"
    );

    second.dependency_artifacts[0].1.content_digest = digest(b"changed dependency");
    assert_ne!(
      candidate_key(&session.identity, &session.class, &second).expect("changed dependency candidate"),
      first_key
    );
  }

  #[test]
  fn dep_info_materialization_rebinds_only_verified_output_paths() {
    let source_root = tempfile::tempdir().expect("source root");
    let validation = graduated_validation(graduated_observation());
    let output_directory = source_root.path().join("build-two/debug/deps");
    let outputs = NativeOutputPaths {
      dep_info: output_directory.join("fixture-0123456789abcdef.d"),
      metadata: output_directory.join("libfixture-0123456789abcdef.rmeta"),
      rlib: None,
    };
    let portable = b"/cargo-rail/native-output/libfixture-0123456789abcdef.rmeta: src/lib.rs\n";
    let translated = translate_dep_info_output_bindings(portable, &validation, &outputs, source_root.path())
      .expect("portable dep-info");
    assert_eq!(
      translated,
      b"build-two/debug/deps/libfixture-0123456789abcdef.rmeta: src/lib.rs\n"
    );

    let windows_stream =
      br#"{"artifact":"/cargo-rail/native-output/libfixture-0123456789abcdef.rmeta","emit":"metadata"}"#;
    let translated_stream =
      translate_output_binding_bytes(windows_stream, &validation, &outputs, source_root.path(), false)
        .expect("Windows mixed-separator stream");
    assert_eq!(
      translated_stream,
      br#"{"artifact":"build-two/debug/deps/libfixture-0123456789abcdef.rmeta","emit":"metadata"}"#
    );

    let generated =
      b"/cargo-rail/native-output/libfixture-0123456789abcdef.rmeta: /cargo-rail/native-output/out/generated.rs\n";
    translate_dep_info_output_bindings(generated, &validation, &outputs, source_root.path())
      .expect_err("unmodeled output-directory inputs must not be rebound");
  }

  #[test]
  fn stream_output_paths_use_canonical_tokens_on_windows() {
    let absolute = r"C:\Users\builder\workspace\build-one\debug\deps\libfixture.rmeta";
    let portable = "/cargo-rail/native-output/libfixture.rmeta";
    let mut stream =
      br#"{"artifact":"C:\\Users\\builder\\workspace\\build-one\\debug\\deps\\libfixture.rmeta"}"#.to_vec();
    for (stored, portable) in canonical_output_path_replacements(absolute, portable) {
      stream = replace_bytes(&stream, &stored, &portable).0;
    }
    assert_eq!(stream, br#"{"artifact":"/cargo-rail/native-output/libfixture.rmeta"}"#);
  }

  #[test]
  fn dep_info_cas_bytes_do_not_depend_on_cargo_output_directory() {
    let source_root = tempfile::tempdir().expect("source root");
    let output_paths = |directory: &str| {
      let directory = source_root.path().join(directory);
      NativeOutputPaths {
        dep_info: directory.join("fixture-0123456789abcdef.d"),
        metadata: directory.join("libfixture-0123456789abcdef.rmeta"),
        rlib: None,
      }
    };
    let first = portable_dep_info_output_bindings(
      b"build-one/debug/deps/libfixture-0123456789abcdef.rmeta: src/lib.rs\n",
      &output_paths("build-one/debug/deps"),
      source_root.path(),
    )
    .expect("first portable dep-info");
    let second = portable_dep_info_output_bindings(
      b"build-two/debug/deps/libfixture-0123456789abcdef.rmeta: src/lib.rs\n",
      &output_paths("build-two/debug/deps"),
      source_root.path(),
    )
    .expect("second portable dep-info");
    assert_eq!(first, second);
  }

  #[test]
  fn portable_arguments_rebase_only_reviewed_repository_paths() {
    let source_root = tempfile::tempdir().expect("source root");
    let crate_root = source_root.path().join("crates/app");
    let output = source_root.path().join("target/debug/deps");
    fs::create_dir_all(crate_root.join("src")).expect("source directory");
    fs::create_dir_all(&output).expect("output directory");
    fs::write(crate_root.join("src/lib.rs"), "pub fn value() {}\n").expect("source");
    fs::write(output.join("libdep.rmeta"), b"dependency").expect("dependency");
    let arguments = vec![
      "--crate-name".into(),
      "fixture".into(),
      "--crate-type=lib".into(),
      "--emit=dep-info,metadata,link".into(),
      "-Cmetadata=0123456789abcdef".into(),
      "-Cextra-filename=-0123456789abcdef".into(),
      "--target=aarch64-apple-darwin".into(),
      "--out-dir".into(),
      output.as_os_str().into(),
      "--extern".into(),
      format!("dep={}", output.join("libdep.rmeta").display()).into(),
      "--extern".into(),
      "proc_macro".into(),
      "-L".into(),
      format!("dependency={}", output.display()).into(),
      "src/lib.rs".into(),
    ];
    let portable = portable_compiler_arguments(&arguments, &crate_root, source_root.path()).expect("portable argv");
    let portable = portable
      .iter()
      .map(|argument| argument.to_str().expect("UTF-8 argv"))
      .collect::<Vec<_>>();
    assert!(portable.contains(&"target/debug/deps"));
    assert!(portable.contains(&"dep=target/debug/deps/libdep.rmeta"));
    assert!(portable.contains(&"proc_macro"));
    assert!(portable.contains(&"dependency=target/debug/deps"));
    assert!(portable.contains(&"crates/app/src/lib.rs"));
    assert!(
      portable
        .iter()
        .any(|argument| argument.ends_with("=/cargo-rail/workspace"))
    );
    #[cfg(windows)]
    {
      let native = format!(
        "--remap-path-prefix={}=/cargo-rail/workspace",
        source_root.path().display()
      );
      let forward = native.replace('\\', "/");
      assert!(portable.contains(&native.as_str()));
      assert!(portable.contains(&forward.as_str()));
    }

    let semantic_root = vec![
      "--crate-name=fixture".into(),
      "--crate-type=lib".into(),
      "--emit=dep-info,metadata".into(),
      format!("-Cmetadata={}", source_root.path().display()).into(),
      format!("--out-dir={}", output.display()).into(),
      "src/lib.rs".into(),
    ];
    assert_eq!(
      portable_compiler_arguments(&semantic_root, &crate_root, source_root.path()),
      Err("compiler_argument_root_binding_not_graduated")
    );

    let external = tempfile::tempdir().expect("external output");
    let escaped = vec![
      "--crate-name=fixture".into(),
      "--crate-type=lib".into(),
      "--emit=dep-info,metadata".into(),
      format!("--out-dir={}", external.path().display()).into(),
      "src/lib.rs".into(),
    ];
    assert_eq!(
      portable_compiler_arguments(&escaped, &crate_root, source_root.path()),
      Err("compiler_output_root_not_graduated")
    );

    let equals_root = source_root.path().join("root=not-remappable");
    fs::create_dir(&equals_root).expect("equals root");
    assert_eq!(
      portable_compiler_arguments(&escaped, &crate_root, &equals_root),
      Err("source_root_not_remappable")
    );
  }
}
