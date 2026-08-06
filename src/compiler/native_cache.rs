//! Native rustc-result reuse for one explicitly graduated invocation class.
//!
//! Reuse resolves one exact pre-executable action directly to durable local
//! authority, then revalidates the complete captured action immediately before
//! publishing verified outputs.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::compiler::observation::{
  CompilerCacheWrapperMetadata, CompilerCacheWrapperStatus, CompilerMode, EnvironmentObservation, FileObservation,
  InvocationRecorder, NativeOutputPaths, ObservationPath, PreparedRawPublication, RawCompilerInvocation,
};
use crate::compiler::wrapper::CacheWrapperPlan;
use crate::error::{RailError, RailResult};
use crate::hermetic::cas::LocalCas;
use crate::hermetic::cas::NativeCacheLookup;
use crate::instrumentation::{
  NativeCacheWrapperDiagnostics, NativeCacheWrapperEventDiagnostics, NativeCacheWrapperPhase, NativeCacheWrapperTrace,
  NativeCacheWrapperTraceSnapshot, NativeCacheWrapperWork,
};
use crate::source::ContentDigest;

pub(crate) mod pack;
mod publication;

pub(crate) const ACTION_KEY_PREFIX: &str = "compiler-action-v9-sha256-";
pub(crate) const RESULT_KEY_PREFIX: &str = "compiler-result-v6-sha256-";
pub(crate) const BASE_ACTION_KEY_PREFIX: &str = "compiler-base-v4-sha256-";
pub(crate) const SESSION_ENV: &str = "CARGO_RAIL_NATIVE_COMPILER_CACHE_SESSION";
pub(crate) const DISPOSITION_ENV: &str = "CARGO_RAIL_NATIVE_COMPILER_CACHE_DISPOSITION";
const LEGACY_STORE_ENV: &str = "CARGO_RAIL_NATIVE_COMPILER_CACHE_STORE";
#[cfg(debug_assertions)]
const RESTORE_FAULT_ENV: &str = "CARGO_RAIL_TEST_NATIVE_RESTORE_FAULT";
#[cfg(debug_assertions)]
const RESTORE_ABORT_ENV: &str = "CARGO_RAIL_TEST_NATIVE_RESTORE_ABORT";
#[cfg(debug_assertions)]
const RESTORE_CANCEL_ENV: &str = "CARGO_RAIL_TEST_NATIVE_RESTORE_CANCEL";
#[cfg(debug_assertions)]
const RESTORE_CRATE_ENV: &str = "CARGO_RAIL_TEST_NATIVE_RESTORE_CRATE";
#[cfg(debug_assertions)]
const CAPTURE_PAUSE_PHASE_ENV: &str = "CARGO_RAIL_TEST_NATIVE_CAPTURE_PAUSE_PHASE";
#[cfg(debug_assertions)]
const CAPTURE_PAUSE_CRATE_ENV: &str = "CARGO_RAIL_TEST_NATIVE_CAPTURE_PAUSE_CRATE";
#[cfg(debug_assertions)]
const CAPTURE_PAUSE_DIRECTORY_ENV: &str = "CARGO_RAIL_TEST_NATIVE_CAPTURE_PAUSE_DIRECTORY";
pub(crate) const DIAGNOSTIC_EXECUTION_CONTRACT: &str = "diagnostic-workspace-wrapper-v9";
pub(crate) const DIRECT_EXECUTION_CONTRACT: &str = "direct-global-wrapper-v9";
const SESSION_FILE: &str = "native-compiler-cache-session-v6.json";
const DIRECT_CONTEXT_FILE: &str = "native-compiler-cache-context-v3.json";
const UNIT_EVIDENCE_DIRECTORY: &str = "native-cache-unit-evidence";
#[cfg(not(windows))]
const DIRECT_WRAPPER_NAME: &str = "cargo-rail-native-rustc-wrapper";
#[cfg(windows)]
const DIRECT_WRAPPER_NAME: &str = "cargo-rail-native-rustc-wrapper.exe";
const GRADUATED_NATIVE_CACHE_CLASS: &str = "library_metadata_rlib";
const NATIVE_CACHE_CAPABILITY_SCHEMA_VERSION: u32 = 6;
const NATIVE_CACHE_IDENTITY_CONTRACT_VERSION: u32 = 9;
const NATIVE_CACHE_EVENT_EVIDENCE_VERSION: u32 = 5;
const NATIVE_CACHE_RUN_EVENT_VERSION: u32 = 7;
const NATIVE_COMPILER_SESSION_VERSION: u32 = 9;
const MAX_SESSION_BYTES: u64 = 64 * 1024;
const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;
const STREAM_MEMORY_SPOOL_BYTES: usize = 64 * 1024;
const MAX_RESTORE_COMMIT_BYTES: usize = 64 * 1024;
const NATIVE_RESTORE_TRANSACTION_VERSION: u32 = 4;
const RESTORE_REGISTRATION_FILE: &str = "registration.json";
const RESTORE_PENDING_COMMIT_FILE: &str = "commit.json";
const RESTORE_MATERIALIZING_DIRECTORY: &str = "materializing";
const RESTORE_VERIFIED_DIRECTORY: &str = "verified";
const RESTORE_PREPARED_DIRECTORY: &str = "prepared";
const RESTORE_PREPARED_DEP_INFO_FILE: &str = "dep-info";
const MAX_SOURCE_ENTRIES: usize = 100_000;
const MAX_SOURCE_DEPTH: usize = 128;
const MAX_SOURCE_PATH_BYTES: usize = 8 * 1024 * 1024;
const MAX_SOURCE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_SOURCE_CAPTURE_TIME: Duration = Duration::from_secs(2);
#[cfg(debug_assertions)]
const TEST_CAPTURE_PAUSE_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(debug_assertions)]
const TEST_CAPTURE_LIMIT_ENV: &str = "CARGO_RAIL_TEST_NATIVE_CAPTURE_LIMIT";
#[cfg(debug_assertions)]
const MAX_TEST_CAPTURE_LIMIT_BYTES: usize = 96;
const MAX_COMPILER_ENVIRONMENT_NAMES: usize = 512;
const MAX_COMPILER_ENVIRONMENT_NAME_BYTES: usize = 256;
const MAX_COMPILER_ENVIRONMENT_BYTES: u64 = 16 * 1024 * 1024;
const DEP_INFO_SLOT: &str = "target/outputs/dep-info";
const METADATA_SLOT: &str = "target/outputs/metadata";
const RLIB_SLOT: &str = "target/outputs/rlib";
const STDOUT_SLOT: &str = "target/streams/stdout";
const STDERR_SLOT: &str = "target/streams/stderr";
const PORTABLE_SOURCE_ROOT: &str = "/cargo-rail/native-source/v2";
const PORTABLE_PACKAGE_ROOT: &str = "/cargo-rail/native-package/v2";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeCompilerSession {
  version: u32,
  identity: String,
  /// Physical binding for this session file only. It never enters a reusable key.
  source_root_identity: String,
  class: NativeCompilerClass,
  capability_identity: String,
  compiler_process_environment_identity: String,
  execution_contract: String,
  authority: NativeSessionAuthority,
}

/// Whether a wrapper session may resolve authoritative results directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NativeSessionAuthority {
  Exact,
  Discovery,
}

/// Exact result class and toolchain boundary for one native reuse session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeCompilerClass {
  name: String,
  platform: String,
  host_target: String,
  rustc_release: String,
}

impl NativeCompilerClass {
  fn capture(rustc_verbose_version: &str) -> Self {
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
    }
  }

  fn is_valid(&self) -> bool {
    self.name == GRADUATED_NATIVE_CACHE_CLASS
      && !self.platform.is_empty()
      && self.host_target != "unknown"
      && self.rustc_release != "unknown"
  }
}

/// Exact snapshot inputs needed to enable native reuse for an ordinary Cargo action.
pub(crate) struct DirectNativeCacheIdentity<'a> {
  pub(crate) source_root: &'a Path,
  pub(crate) source_root_spelling: &'a Path,
  pub(crate) session: NativeCompilerSession,
  pub(crate) deferred_session: Option<std::thread::JoinHandle<RailResult<(NativeCompilerSession, u64)>>>,
  pub(crate) wrapper_plan: CacheWrapperPlan,
  pub(crate) setup_bytes_hashed: u64,
  pub(crate) l2_alias: Option<&'a str>,
  pub(crate) retain_event_evidence: bool,
}

/// Activation result for one ordinary Cargo action.
pub(crate) enum DirectNativeCacheSetup {
  Active(Box<DirectNativeCacheRun>),
  Bypassed(DirectCacheBypass),
  OperationalFailure(String),
}

/// Stable action-level reason that prevents cargo-rail from installing its compiler cache wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirectCacheBypass {
  DisabledByRequest,
  DisabledByConfiguration,
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
      Self::DisabledByConfiguration => "native_cache_disabled_by_configuration",
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
  publication: Option<publication::Coordinator>,
  observations: tempfile::TempDir,
  cargo_config: OsString,
  setup_bytes_hashed: u64,
  remote: Option<crate::remote_cache::RemoteCoordinator>,
  remote_configuration_failed: bool,
  publication_configuration_failed: bool,
}

#[derive(Clone)]
pub(crate) struct NativeCacheContext {
  session: PathBuf,
  source_root: PathBuf,
  source_root_spelling: PathBuf,
  observation_directory: PathBuf,
  discovery_only: bool,
  retain_event_evidence: bool,
  capture_wrapper_diagnostics: bool,
  remote: Option<crate::remote_cache::RemoteWrapperContext>,
  publication: Option<publication::WrapperContext>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectNativeCacheContext {
  version: u32,
  source_root: String,
  source_root_spelling: String,
  discovery_only: bool,
  retain_event_evidence: bool,
  capture_wrapper_diagnostics: bool,
  remote: Option<crate::remote_cache::RemoteWrapperContext>,
  publication: Option<publication::WrapperContext>,
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
  pub(crate) remote: Option<crate::remote_cache::RemoteCoordinatorReport>,
  pub(crate) environment_selector_diverged: bool,
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
  result_key: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  unit: Option<NativeCacheUnitEvidence>,
}

/// Stable descriptor and exact action inputs for one compiler unit.
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

/// Complete pre-execution source namespace bound into one native action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeSourceState {
  version: u32,
  root: ObservationPath,
  entries: Vec<NativeSourceEntry>,
}

/// Root-independent source state used only for semantic action identity.
#[derive(Serialize)]
struct PortableNativeSourceState<'a> {
  version: u32,
  root: String,
  crate_root: &'a str,
  entries: &'a [NativeSourceEntry],
}

#[derive(Serialize)]
struct PortableNativeDeclaredInput<'a> {
  path: String,
  content_digest: &'a str,
  executable: bool,
  symlink_target: &'a Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeSourceEntry {
  path: String,
  kind: NativeSourceEntryKind,
}

/// Physical external package binding used for cold remapping and local revalidation only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativePackageBinding {
  root: PathBuf,
  spelling: PathBuf,
  source_relative: String,
}

impl NativePackageBinding {
  fn capture(source_root: &Path, source_root_spelling: &Path) -> RailResult<Self> {
    let spelling = std::env::var_os("CARGO_MANIFEST_DIR")
      .map(PathBuf::from)
      .ok_or_else(|| RailError::message("external native source has no Cargo package root"))?;
    let root = crate::utils::canonicalize_existing(&spelling)?;
    let source_relative = native_relative_path(
      source_root
        .strip_prefix(&root)
        .map_err(|_| RailError::message("external native source is outside its Cargo package root"))?,
    )?;
    let binding = Self {
      root,
      spelling,
      source_relative,
    };
    binding.validate_live(source_root, source_root_spelling)?;
    Ok(binding)
  }

  fn validate_object(&self) -> RailResult<()> {
    if !self.root.is_absolute()
      || !self.spelling.is_absolute()
      || self.root.as_os_str().as_encoded_bytes().contains(&0)
      || self.spelling.as_os_str().as_encoded_bytes().contains(&0)
      || native_relative_path(Path::new(&self.source_relative))? != self.source_relative
    {
      return Err(RailError::message("native package binding is invalid"));
    }
    Ok(())
  }

  fn validate_live(&self, source_root: &Path, source_root_spelling: &Path) -> RailResult<()> {
    self.validate_object()?;
    let metadata = fs::symlink_metadata(&self.spelling)?;
    if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
      return Err(RailError::message(
        "external native package root is not a real directory",
      ));
    }
    let canonical_root = crate::utils::canonicalize_existing(&self.spelling)?;
    let canonical_source = crate::utils::canonicalize_existing(&self.spelling.join(&self.source_relative))?;
    if canonical_root != self.root
      || crate::utils::canonicalize_existing(&self.root)? != self.root
      || canonical_source != source_root
      || crate::utils::canonicalize_existing(source_root_spelling)? != source_root
    {
      return Err(RailError::message(
        "native package binding does not own its external source namespace",
      ));
    }
    source_root_spellings(&self.spelling)?;
    Ok(())
  }

  fn portable_source_root(&self) -> String {
    if self.source_relative.is_empty() {
      PORTABLE_PACKAGE_ROOT.to_string()
    } else {
      format!("{PORTABLE_PACKAGE_ROOT}/{}", self.source_relative)
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum NativeSourceEntryKind {
  Directory {
    mode: u32,
  },
  RegularFile {
    bytes: u64,
    content_digest: String,
    mode: u32,
  },
}

/// Exact compiler environment visible after private wrapper capabilities are removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovedEnvState {
  version: u32,
  entries: Vec<ApprovedEnvEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovedEnvEntry {
  name: String,
  value_digest: Option<String>,
  root_mapped: bool,
}

/// Validate the one canonical environment-name set admitted by native cache selectors.
pub(crate) fn validate_environment_selector_names<'a>(names: impl IntoIterator<Item = &'a str>) -> RailResult<()> {
  let mut count = 0usize;
  let mut previous = None::<&str>;
  for name in names {
    count = count.saturating_add(1);
    if count > MAX_COMPILER_ENVIRONMENT_NAMES {
      return Err(RailError::message(format!(
        "native environment selector exceeds its {MAX_COMPILER_ENVIRONMENT_NAMES}-name bound"
      )));
    }
    if name.is_empty()
      || name.len() > MAX_COMPILER_ENVIRONMENT_NAME_BYTES
      || name.as_bytes().contains(&0)
      || name.contains('=')
      || name.chars().any(char::is_control)
      || private_compiler_environment(OsStr::new(name))
    {
      return Err(RailError::message(
        "native environment selector contains an invalid environment name",
      ));
    }
    if previous.is_some_and(|previous| previous >= name) {
      return Err(RailError::message(
        "native environment selector names are not strictly sorted and unique",
      ));
    }
    previous = Some(name);
  }
  Ok(())
}

impl ApprovedEnvState {
  fn empty() -> Self {
    Self {
      version: 3,
      entries: Vec::new(),
    }
  }

  fn validate_object(&self) -> RailResult<()> {
    if self.version != 3
      || validate_environment_selector_names(self.entries.iter().map(|entry| entry.name.as_str())).is_err()
      || self.entries.iter().any(|entry| {
        entry
          .value_digest
          .as_deref()
          .is_some_and(|digest| validate_sha256(digest).is_err())
      })
    {
      return Err(RailError::message("native compiler environment proof is invalid"));
    }
    Ok(())
  }
}

/// Canonical proof that rustc selected only capabilities already bound by the action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeCompilerWitness {
  version: u32,
  complete: bool,
  source_paths: Vec<String>,
  dependency_names: Vec<String>,
  environment_names: Vec<String>,
}

#[derive(Serialize)]
struct NativeResultIdentity<'a> {
  action_key: &'a str,
  witness: &'a NativeCompilerWitness,
  outputs: &'a [NativeCompilerOutput],
  stdout_digest: &'a str,
  stdout_bytes: u64,
  stderr_digest: &'a str,
  stderr_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct NativeCaptureGuard {
  entries: Vec<NativeGuardEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct NativeGuardEntry {
  path: String,
  metadata: NativeMetadataGuard,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct NativeMetadataGuard {
  entry_type: NativeGuardEntryType,
  len: u64,
  modified: SystemTime,
  readonly: bool,
  #[cfg(unix)]
  device: u64,
  #[cfg(unix)]
  inode: u64,
  #[cfg(unix)]
  mode: u32,
  #[cfg(unix)]
  changed_seconds: i64,
  #[cfg(unix)]
  changed_nanoseconds: i64,
  #[cfg(windows)]
  volume_serial_number: u64,
  #[cfg(windows)]
  file_id: u64,
  #[cfg(windows)]
  file_attributes: u32,
  #[cfg(windows)]
  creation_time: u64,
  #[cfg(windows)]
  last_write_time: u64,
  #[cfg(windows)]
  change_time: u64,
  #[cfg(windows)]
  number_of_links: u64,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeRestoreCommit {
  version: u32,
  transaction_id: String,
  action_key: String,
  transaction_directory: String,
  transaction_identity: NativeRestoreDirectoryIdentity,
  members: Vec<NativeRestoreMember>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeRestoreRegistration {
  version: u32,
  transaction_id: String,
  action_key: String,
  directory_identity: NativeRestoreDirectoryIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeRestoreDirectoryIdentity {
  #[cfg(unix)]
  device: u64,
  #[cfg(unix)]
  inode: u64,
  #[cfg(windows)]
  volume_serial_number: u64,
  #[cfg(windows)]
  file_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeRestoreFileIdentity {
  bytes: u64,
  #[cfg(unix)]
  device: u64,
  #[cfg(unix)]
  inode: u64,
  #[cfg(windows)]
  volume_serial_number: u64,
  #[cfg(windows)]
  file_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum NativeRestoreMember {
  Output {
    source: String,
    destination: String,
    source_identity: NativeRestoreFileIdentity,
    previous_identity: Option<NativeRestoreFileIdentity>,
    content_digest: String,
  },
  Observation {
    destination: String,
    content_digest: String,
    bytes: u64,
    preexisting: bool,
  },
}

impl NativeRestoreMember {
  fn destination(&self) -> &str {
    match self {
      Self::Output { destination, .. } | Self::Observation { destination, .. } => destination,
    }
  }
}

struct PreparedRestoreOutput {
  source: PathBuf,
  opened: File,
  source_identity: NativeRestoreFileIdentity,
  destination: PathBuf,
  observation: FileObservation,
  bytes: u64,
  mode: u32,
  bytes_hashed: u64,
}

#[derive(Debug)]
struct PublishedRestoreOutput {
  opened: File,
  destination: PathBuf,
  identity: NativeRestoreFileIdentity,
  bytes: u64,
  mode: u32,
}

impl PublishedRestoreOutput {
  fn sync(&self) -> RailResult<()> {
    self.opened.sync_all()?;
    Ok(())
  }

  fn revalidate(&self) -> RailResult<()> {
    let metadata = fs::symlink_metadata(&self.destination)?;
    if native_restore_file_identity(&self.opened)? != self.identity
      || !crate::utils::private_file_matches_path(&self.opened, &self.destination, self.bytes)?
      || native_output_mode(&metadata) != self.mode
    {
      return Err(RailError::message(format!(
        "published native compiler output '{}' changed before commit",
        self.destination.display()
      )));
    }
    Ok(())
  }
}

struct PreparedNativeRestore {
  outputs: Vec<PreparedRestoreOutput>,
  stdout: Vec<u8>,
  stderr: Vec<u8>,
  observation: PreparedRawPublication,
  bytes_restored: u64,
  publication_bytes_hashed: u64,
}

struct NativeRestorePaths {
  output_parent: PathBuf,
  marker: PathBuf,
  lock: PathBuf,
  transaction_directory: PathBuf,
  output_sources: BTreeMap<PathBuf, PathBuf>,
}

enum NativeRestoreTransactionState {
  Registered,
  Committed(NativeRestoreCommit),
  Complete,
}

struct NativeRestoreTransaction {
  paths: NativeRestorePaths,
  observation_directory: PathBuf,
  registration: NativeRestoreRegistration,
  state: NativeRestoreTransactionState,
  _lock: File,
}

enum RestorePublishFailure {
  BeforeEffect(RailError),
  AfterEffect(RailError),
  Operational(RailError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
enum NativeGuardEntryType {
  Directory,
  RegularFile,
}

#[derive(Debug, Clone)]
pub(crate) struct NativeActionCapture {
  source_root: PathBuf,
  source_root_spelling: PathBuf,
  crate_root: String,
  package_binding: Option<NativePackageBinding>,
  source_state: NativeSourceState,
  approved_environment: ApprovedEnvState,
  guard: NativeCaptureGuard,
  bytes_hashed: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativePublicationProof {
  version: u32,
  source_state: NativeSourceState,
  package_binding: Option<NativePackageBinding>,
  approved_environment: ApprovedEnvState,
  guard_identity: String,
  environment_bytes_hashed: u64,
}

/// One privately staged native result whose semantic identities were derived
/// from a live action capture rather than supplied by a storage caller.
pub(crate) struct PreparedNativeResult {
  staging: PreparedNativeStaging,
  staging_lock: Option<File>,
  manifest: crate::hermetic::OutputManifest,
  validation: NativeCompilerValidation,
  origin: PreparedNativeOrigin,
  move_preverified_blobs: bool,
}

pub(crate) enum PreparedNativeStaging {
  Temporary(tempfile::TempDir),
  CommandScoped(PathBuf),
}

impl PreparedNativeStaging {
  pub(crate) fn path(&self) -> &Path {
    match self {
      Self::Temporary(directory) => directory.path(),
      Self::CommandScoped(path) => path,
    }
  }
}

/// Admission origin is storage policy, never part of the semantic result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PreparedNativeOrigin {
  Local,
  Remote(RemoteAuthorityId),
}

/// Identity of one deployment-pinned authenticated remote authority tuple.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct RemoteAuthorityId(String);

impl RemoteAuthorityId {
  #[cfg(test)]
  pub(crate) fn for_test(label: &str) -> RailResult<Self> {
    Self::parse(format!(
      "remote-authority-v1-sha256-{}",
      ContentDigest::sha256(label.as_bytes())
    ))
  }

  pub(crate) fn as_str(&self) -> &str {
    &self.0
  }

  pub(crate) fn parse(value: String) -> RailResult<Self> {
    validate_identity(&value, "remote-authority-v1-sha256-")?;
    Ok(Self(value))
  }
}

impl PreparedNativeResult {
  #[cfg(test)]
  pub(crate) fn from_verified_staging(
    staging: tempfile::TempDir,
    manifest: crate::hermetic::OutputManifest,
    validation: NativeCompilerValidation,
  ) -> Self {
    Self {
      staging: PreparedNativeStaging::Temporary(staging),
      staging_lock: None,
      manifest,
      validation,
      origin: PreparedNativeOrigin::Local,
      move_preverified_blobs: false,
    }
  }

  fn from_verified_local_cas_staging(
    staging: pack::NativeResultStaging,
    manifest: crate::hermetic::OutputManifest,
    validation: NativeCompilerValidation,
  ) -> Self {
    let (staging, staging_lock, command_scoped) = staging.into_parts();
    let move_preverified_blobs = staging_lock.is_some() || command_scoped;
    Self {
      staging: PreparedNativeStaging::Temporary(staging),
      staging_lock,
      manifest,
      validation,
      origin: PreparedNativeOrigin::Local,
      move_preverified_blobs,
    }
  }

  fn from_authenticated_pack(
    staging: tempfile::TempDir,
    staging_lock: Option<File>,
    manifest: crate::hermetic::OutputManifest,
    validation: NativeCompilerValidation,
    authority: RemoteAuthorityId,
  ) -> Self {
    let move_preverified_blobs = staging_lock.is_some();
    Self {
      staging: PreparedNativeStaging::Temporary(staging),
      staging_lock,
      manifest,
      validation,
      origin: PreparedNativeOrigin::Remote(authority),
      move_preverified_blobs,
    }
  }

  pub(crate) fn into_parts(
    self,
  ) -> (
    PreparedNativeStaging,
    Option<File>,
    crate::hermetic::OutputManifest,
    NativeCompilerValidation,
    PreparedNativeOrigin,
    bool,
  ) {
    (
      self.staging,
      self.staging_lock,
      self.manifest,
      self.validation,
      self.origin,
      self.move_preverified_blobs,
    )
  }
}

/// Bind one authenticated, byte-verified pack to the current live action and
/// reconstruct the private local representation used by ordinary L1 hits.
fn prepare_authenticated_native_pack(
  decoded: pack::DecodedNativePack,
  authority: RemoteAuthorityId,
  session: &NativeCompilerSession,
  initial_capture: &NativeActionCapture,
  current_observation: &RawCompilerInvocation,
  output_paths: &NativeOutputPaths,
  source_root: &Path,
) -> RailResult<(PreparedNativeResult, u64)> {
  let pack::DecodedNativePack {
    staging,
    descriptor,
    bytes_read,
  } = decoded;
  let (staging, staging_lock, _command_scoped) = staging.into_parts();
  let live_action = action_key(&session.identity, &session.class, current_observation, initial_capture)?;
  if descriptor.action_key != live_action
    || !initial_capture.validates_witness(&descriptor.witness, current_observation)
  {
    return Err(RailError::message(
      "native result pack descriptor does not match the current action capability",
    ));
  }
  let current_bindings = native_output_bindings(output_paths);
  if current_bindings.len() != descriptor.outputs.len()
    || current_bindings
      .iter()
      .zip(&descriptor.outputs)
      .any(|((role, slot, _), expected)| *role != expected.role || *slot != expected.slot)
  {
    return Err(RailError::message(
      "native result pack output contract does not match the current invocation",
    ));
  }

  let mut observation = current_observation.clone();
  observation.observed_reads =
    current_observed_reads(initial_capture, &descriptor.witness, current_observation, source_root)?;
  observation.environment_reads = descriptor
    .witness
    .environment_names
    .iter()
    .map(|name| EnvironmentObservation {
      name: name.clone(),
      value_digest: initial_capture
        .approved_environment
        .entries
        .binary_search_by(|entry| entry.name.as_str().cmp(name))
        .ok()
        .and_then(|index| initial_capture.approved_environment.entries[index].value_digest.clone()),
      secret_capability: false,
    })
    .collect();
  observation.emitted_outputs = current_bindings
    .iter()
    .zip(&descriptor.outputs)
    .map(|((_, _, path), expected)| FileObservation {
      path: ObservationPath::capture(path, source_root, source_root),
      content_digest: expected.content_digest.clone(),
      executable: false,
      symlink_target: None,
    })
    .collect();
  observation.emitted_outputs.sort();
  observation.success = true;
  observation.cache_wrapper = None;

  let validation =
    NativeCompilerValidation::new(session, observation, &initial_capture.approved_environment, descriptor)?;
  let manifest_slots = validation
    .cas_output_bindings()
    .chain(validation.cas_stream_bindings())
    .collect::<Vec<_>>();
  let manifest = crate::hermetic::manifest_from_verified_native_slots(&manifest_slots)?;
  Ok((
    PreparedNativeResult::from_authenticated_pack(staging, staging_lock, manifest, validation, authority),
    bytes_read,
  ))
}

fn current_observed_reads(
  capture: &NativeActionCapture,
  witness: &NativeCompilerWitness,
  current_observation: &RawCompilerInvocation,
  source_root: &Path,
) -> RailResult<Vec<FileObservation>> {
  let mut observed = witness
    .source_paths
    .iter()
    .map(|relative| {
      let index = capture
        .source_state
        .entries
        .binary_search_by(|entry| entry.path.as_str().cmp(relative))
        .map_err(|_| RailError::message("native result witness is absent from current SourceState"))?;
      let NativeSourceEntryKind::RegularFile {
        content_digest, mode, ..
      } = &capture.source_state.entries[index].kind
      else {
        return Err(RailError::message(
          "native result witness selected a non-file source capability",
        ));
      };
      Ok(FileObservation {
        path: ObservationPath::capture(&capture.source_root_spelling.join(relative), source_root, source_root),
        content_digest: content_digest.clone(),
        executable: source_mode_executable(*mode),
        symlink_target: None,
      })
    })
    .collect::<RailResult<Vec<_>>>()?;
  observed.extend(
    current_observation
      .dependency_artifacts
      .iter()
      .map(|(_, artifact)| artifact.clone()),
  );
  observed.sort();
  observed.dedup();
  Ok(observed)
}

#[derive(Serialize)]
struct NativeCacheKeyInputs<'a> {
  cfg: &'a BTreeSet<String>,
  compiler_arguments: Vec<String>,
  declared_inputs: &'a [FileObservation],
  dependency_artifacts: Vec<NativeDependencyArtifactKey<'a>>,
}

#[derive(Serialize)]
struct PortableNativeCacheKeyInputs<'a> {
  cfg: &'a BTreeSet<String>,
  compiler_arguments: Vec<String>,
  declared_inputs: Vec<PortableNativeDeclaredInput<'a>>,
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

impl NativeActionCapture {
  fn from_publication_proof(
    observation: &RawCompilerInvocation,
    source_root: &Path,
    proof: &NativePublicationProof,
  ) -> RailResult<Self> {
    proof.validate_object()?;
    let [declared] = observation.declared_inputs.as_slice() else {
      return Err(RailError::message("native publication proof has no crate root"));
    };
    let crate_root_spelling = declared.path.resolve(source_root);
    let namespace_spelling = crate_root_spelling
      .parent()
      .ok_or_else(|| RailError::message("native publication crate root has no source namespace"))?
      .to_path_buf();
    let namespace = crate::utils::canonicalize_existing(&proof.source_state.root.resolve(source_root))?;
    let crate_root = crate::utils::canonicalize_existing(&crate_root_spelling)?;
    if crate_root.parent() != Some(namespace.as_path()) {
      return Err(RailError::message(
        "native publication SourceState does not own the declared crate root",
      ));
    }
    let crate_root = native_relative_path(
      crate_root
        .strip_prefix(&namespace)
        .map_err(|_| RailError::message("native publication crate root escaped its SourceState"))?,
    )?;
    if let Some(binding) = &proof.package_binding {
      binding.validate_live(&namespace, &namespace_spelling)?;
    }
    Ok(Self {
      source_root: namespace,
      source_root_spelling: namespace_spelling,
      crate_root,
      package_binding: proof.package_binding.clone(),
      source_state: proof.source_state.clone(),
      approved_environment: proof.approved_environment.clone(),
      guard: NativeCaptureGuard { entries: Vec::new() },
      bytes_hashed: 0,
    })
  }

  fn capture(observation: &RawCompilerInvocation, source_root: &Path) -> RailResult<Self> {
    Self::capture_with_environment(observation, source_root, None, None)
  }

  fn capture_with_approved_environment(
    observation: &RawCompilerInvocation,
    source_root: &Path,
    approved_environment: ApprovedEnvState,
  ) -> RailResult<Self> {
    Self::capture_with_environment(observation, source_root, Some(approved_environment), None)
  }

  fn capture_with_publication_proof(
    observation: &RawCompilerInvocation,
    source_root: &Path,
    proof: &NativePublicationProof,
  ) -> RailResult<Self> {
    proof.validate_object()?;
    Self::capture_with_environment(
      observation,
      source_root,
      Some(proof.approved_environment.clone()),
      proof.package_binding.clone(),
    )
  }

  fn capture_with_environment(
    observation: &RawCompilerInvocation,
    source_root: &Path,
    approved_environment: Option<ApprovedEnvState>,
    package_binding: Option<NativePackageBinding>,
  ) -> RailResult<Self> {
    let [declared] = observation.declared_inputs.as_slice() else {
      return Err(RailError::message(
        "native source capture requires one declared crate root",
      ));
    };
    if declared.symlink_target.is_some() {
      return Err(RailError::message("native crate root must not be a symlink"));
    }
    let crate_root = declared.path.resolve(source_root);
    let namespace_spelling = crate_root
      .parent()
      .ok_or_else(|| RailError::message("native crate root has no source namespace"))?
      .to_path_buf();
    let namespace = crate::utils::canonicalize_existing(&namespace_spelling)?;
    let crate_root = crate::utils::canonicalize_existing(&crate_root)?;
    if crate_root.parent() != Some(namespace.as_path()) {
      return Err(RailError::message(
        "native crate root crosses a source namespace capability",
      ));
    }

    let started = Instant::now();
    let crate_root_relative = crate_root
      .strip_prefix(&namespace)
      .map_err(|_| RailError::message("native crate root escaped its source namespace"))?;
    let crate_root_relative = native_relative_path(crate_root_relative)?;
    let mut budget = NativeCaptureBudget::new(native_capture_limits(observation)?);
    let (source_state, guard) =
      capture_native_source_namespace(&namespace, &crate_root, source_root, started, &mut budget)?;
    let package_binding = match &source_state.root {
      ObservationPath::Repository(_) => {
        if package_binding.is_some() {
          return Err(RailError::message(
            "workspace native source has an external package binding",
          ));
        }
        None
      }
      ObservationPath::Host(_) => {
        let binding = match package_binding {
          Some(binding) => binding,
          None => NativePackageBinding::capture(&namespace, &namespace_spelling)?,
        };
        binding.validate_live(&namespace, &namespace_spelling)?;
        Some(binding)
      }
    };
    let (approved_environment, environment_bytes) = match approved_environment {
      Some(environment) => (environment, 0),
      None => (ApprovedEnvState::empty(), 0),
    };
    let mut guard_entries = guard.entries;
    for (name, artifact) in &observation.dependency_artifacts {
      let path = artifact.path.resolve(source_root);
      let relative = format!("dependency:{name}:{}", artifact_name(&path)?);
      let (current, metadata, _) = capture_guarded_file(&path, started, &mut budget)?;
      if current != artifact.content_digest
        || executable_mode_from_guard(&metadata) != artifact.executable
        || artifact.symlink_target.is_some()
      {
        return Err(RailError::message(
          "native dependency artifact changed during action capture",
        ));
      }
      guard_entries.push(NativeGuardEntry {
        path: relative,
        metadata,
      });
    }
    guard_entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    for pair in guard_entries.windows(2) {
      if pair[0].path == pair[1].path {
        return Err(RailError::message(
          "native action capture contains duplicate guard paths",
        ));
      }
    }
    budget.check(0, started.elapsed())?;
    Ok(Self {
      source_root: namespace,
      source_root_spelling: namespace_spelling,
      crate_root: crate_root_relative,
      package_binding,
      source_state,
      approved_environment,
      guard: NativeCaptureGuard { entries: guard_entries },
      bytes_hashed: budget.bytes_hashed.saturating_add(environment_bytes),
    })
  }

  fn unchanged_from(&self, initial: &Self) -> bool {
    self.crate_root == initial.crate_root
      && self.package_binding == initial.package_binding
      && self.source_state == initial.source_state
      && self.approved_environment == initial.approved_environment
      && self.guard == initial.guard
  }

  fn guard_identity(&self) -> RailResult<String> {
    Ok(format!(
      "sha256:{}",
      ContentDigest::sha256(&serde_json::to_vec(&self.guard)?)
    ))
  }

  fn remotely_shareable(&self, remote: Option<&crate::remote_cache::RemoteWrapperContext>) -> bool {
    remote.is_some_and(|remote| {
      self
        .approved_environment
        .entries
        .iter()
        .all(|entry| remote.approves_environment_name(&entry.name))
    })
  }

  fn portable_source_root(&self) -> RailResult<String> {
    match &self.source_state.root {
      ObservationPath::Repository(relative) => {
        let relative = native_relative_path(Path::new(relative))?;
        Ok(if relative.is_empty() {
          PORTABLE_SOURCE_ROOT.to_string()
        } else {
          format!("{PORTABLE_SOURCE_ROOT}/{relative}")
        })
      }
      ObservationPath::Host(_) => self
        .package_binding
        .as_ref()
        .map(NativePackageBinding::portable_source_root)
        .ok_or_else(|| RailError::message("external native source has no package binding")),
    }
  }

  fn portable_crate_root(&self) -> RailResult<String> {
    let root = self.portable_source_root()?;
    Ok(if self.crate_root.is_empty() {
      root
    } else {
      format!("{root}/{}", self.crate_root)
    })
  }

  fn portable_source_state(&self) -> RailResult<PortableNativeSourceState<'_>> {
    Ok(PortableNativeSourceState {
      version: 2,
      root: self.portable_source_root()?,
      crate_root: &self.crate_root,
      entries: &self.source_state.entries,
    })
  }

  fn witness(&self, observation: &RawCompilerInvocation, workspace_root: &Path) -> RailResult<NativeCompilerWitness> {
    let mut source_paths = Vec::with_capacity(observation.observed_reads.len());
    for observed in &observation.observed_reads {
      if observation
        .dependency_artifacts
        .iter()
        .any(|(_, dependency)| dependency == observed)
      {
        continue;
      }
      let absolute = observed.path.resolve(workspace_root);
      let absolute = crate::utils::canonicalize_existing(&absolute)?;
      let relative = absolute.strip_prefix(&self.source_root).map_err(|_| {
        RailError::message(format!(
          "compiler selected source '{}' outside its complete namespace",
          absolute.display()
        ))
      })?;
      let relative = native_relative_path(relative)?;
      let index = self
        .source_state
        .entries
        .binary_search_by(|entry| entry.path.as_str().cmp(&relative))
        .map_err(|_| RailError::message("compiler selected source absent from captured SourceState"))?;
      let NativeSourceEntryKind::RegularFile {
        content_digest, mode, ..
      } = &self.source_state.entries[index].kind
      else {
        return Err(RailError::message("compiler selected a non-file source entry"));
      };
      if content_digest != &observed.content_digest
        || source_mode_executable(*mode) != observed.executable
        || observed.symlink_target.is_some()
      {
        return Err(RailError::message(
          "compiler-selected source does not match captured SourceState",
        ));
      }
      source_paths.push(relative);
    }
    source_paths.sort_unstable();
    source_paths.dedup();

    let mut dependency_names = observation
      .dependency_artifacts
      .iter()
      .map(|(name, _)| name.clone())
      .collect::<Vec<_>>();
    dependency_names.sort_unstable();
    if dependency_names.windows(2).any(|pair| pair[0] == pair[1]) {
      return Err(RailError::message(
        "compiler invocation contains duplicate dependency capabilities",
      ));
    }

    let mut environment_names = Vec::with_capacity(observation.environment_reads.len());
    for observed in &observation.environment_reads {
      if observed.secret_capability {
        return Err(RailError::message("compiler selected a secret environment capability"));
      }
      let captured = self
        .approved_environment
        .entries
        .binary_search_by(|entry| entry.name.as_str().cmp(&observed.name))
        .ok()
        .map(|index| &self.approved_environment.entries[index]);
      match (captured, observed.value_digest.as_deref()) {
        (Some(captured), observed_digest)
          if !captured.root_mapped && captured.value_digest.as_deref() == observed_digest => {}
        (None, None) => {}
        _ => {
          return Err(RailError::message(
            "compiler-selected environment does not match captured ApprovedEnvState",
          ));
        }
      }
      environment_names.push(observed.name.clone());
    }
    environment_names.sort_unstable();
    environment_names.dedup();
    Ok(NativeCompilerWitness {
      version: 1,
      complete: true,
      source_paths,
      dependency_names,
      environment_names,
    })
  }

  fn validates_witness(&self, witness: &NativeCompilerWitness, observation: &RawCompilerInvocation) -> bool {
    let mut dependencies = observation
      .dependency_artifacts
      .iter()
      .map(|(name, _)| name.as_str())
      .collect::<Vec<_>>();
    dependencies.sort_unstable();
    witness.version == 1
      && witness.complete
      && !witness.source_paths.is_empty()
      && strictly_sorted_unique_strings(&witness.source_paths)
      && strictly_sorted_unique_strings(&witness.dependency_names)
      && validate_environment_selector_names(witness.environment_names.iter().map(String::as_str)).is_ok()
      && witness.dependency_names.iter().map(String::as_str).eq(dependencies)
      && witness.source_paths.iter().all(|path| {
        self
          .source_state
          .entries
          .binary_search_by(|entry| entry.path.as_str().cmp(path))
          .ok()
          .is_some_and(|index| {
            matches!(
              self.source_state.entries[index].kind,
              NativeSourceEntryKind::RegularFile { .. }
            )
          })
      })
      && witness.environment_names.iter().all(|name| {
        !name.is_empty()
          && !name.as_bytes().contains(&0)
          && self
            .approved_environment
            .entries
            .binary_search_by(|entry| entry.name.as_str().cmp(name))
            .is_ok_and(|index| !self.approved_environment.entries[index].root_mapped)
      })
  }
}

#[derive(Debug, Clone, Copy)]
struct NativeCaptureLimits {
  entries: usize,
  depth: usize,
  path_bytes: usize,
  bytes_hashed: u64,
  elapsed: Duration,
}

const NATIVE_CAPTURE_LIMITS: NativeCaptureLimits = NativeCaptureLimits {
  entries: MAX_SOURCE_ENTRIES,
  depth: MAX_SOURCE_DEPTH,
  path_bytes: MAX_SOURCE_PATH_BYTES,
  bytes_hashed: MAX_SOURCE_BYTES,
  elapsed: MAX_SOURCE_CAPTURE_TIME,
};

#[cfg(debug_assertions)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestCaptureLimit {
  Entries,
  Depth,
  PathBytes,
  BytesHashed,
  Elapsed,
}

#[cfg(debug_assertions)]
fn parse_test_capture_limit(value: &str) -> RailResult<(&str, TestCaptureLimit)> {
  if value.is_empty() || value.len() > MAX_TEST_CAPTURE_LIMIT_BYTES {
    return Err(RailError::message("native test capture limit is not bounded"));
  }
  let (crate_name, limit) = value
    .split_once('/')
    .ok_or_else(|| RailError::message("native test capture limit is not canonical"))?;
  if crate_name.is_empty()
    || crate_name.len() > 64
    || !crate_name
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
  {
    return Err(RailError::message(
      "native test capture limit has an invalid crate name",
    ));
  }
  let limit = match limit {
    "entries" => TestCaptureLimit::Entries,
    "depth" => TestCaptureLimit::Depth,
    "path_bytes" => TestCaptureLimit::PathBytes,
    "bytes_hashed" => TestCaptureLimit::BytesHashed,
    "elapsed" => TestCaptureLimit::Elapsed,
    _ => return Err(RailError::message("native test capture limit is not canonical")),
  };
  Ok((crate_name, limit))
}

#[cfg(debug_assertions)]
fn native_capture_limits(observation: &RawCompilerInvocation) -> RailResult<NativeCaptureLimits> {
  let Some(value) = std::env::var_os(TEST_CAPTURE_LIMIT_ENV) else {
    return Ok(NATIVE_CAPTURE_LIMITS);
  };
  let value = value
    .to_str()
    .ok_or_else(|| RailError::message("native test capture limit is not valid UTF-8"))?;
  let (crate_name, selected) = parse_test_capture_limit(value)?;
  if observation.crate_name.as_deref() != Some(crate_name) {
    return Ok(NATIVE_CAPTURE_LIMITS);
  }
  let mut limits = NATIVE_CAPTURE_LIMITS;
  match selected {
    TestCaptureLimit::Entries => limits.entries = 1,
    TestCaptureLimit::Depth => limits.depth = 0,
    TestCaptureLimit::PathBytes => limits.path_bytes = 0,
    TestCaptureLimit::BytesHashed => limits.bytes_hashed = 0,
    TestCaptureLimit::Elapsed => limits.elapsed = Duration::ZERO,
  }
  Ok(limits)
}

#[cfg(not(debug_assertions))]
fn native_capture_limits(_observation: &RawCompilerInvocation) -> RailResult<NativeCaptureLimits> {
  Ok(NATIVE_CAPTURE_LIMITS)
}

struct NativeCaptureBudget {
  limits: NativeCaptureLimits,
  entries: usize,
  path_bytes: usize,
  bytes_hashed: u64,
}

impl NativeCaptureBudget {
  const fn new(limits: NativeCaptureLimits) -> Self {
    Self {
      limits,
      entries: 0,
      path_bytes: 0,
      bytes_hashed: 0,
    }
  }

  fn account_entry(&mut self, path: &str) -> RailResult<()> {
    let entries = self
      .entries
      .checked_add(1)
      .ok_or_else(|| RailError::message("native source entry bound exceeded"))?;
    let path_bytes = self
      .path_bytes
      .checked_add(path.len())
      .ok_or_else(|| RailError::message("native source path-byte bound exceeded"))?;
    if entries > self.limits.entries {
      return Err(RailError::message("native source entry bound exceeded"));
    }
    if path_bytes > self.limits.path_bytes {
      return Err(RailError::message("native source path-byte bound exceeded"));
    }
    self.entries = entries;
    self.path_bytes = path_bytes;
    Ok(())
  }

  fn account_hashed_bytes(&mut self, bytes: u64) -> RailResult<()> {
    let bytes_hashed = self
      .bytes_hashed
      .checked_add(bytes)
      .ok_or_else(|| RailError::message("native source byte bound exceeded"))?;
    if bytes_hashed > self.limits.bytes_hashed {
      return Err(RailError::message("native source byte bound exceeded"));
    }
    self.bytes_hashed = bytes_hashed;
    Ok(())
  }

  fn check(&self, depth: usize, elapsed: Duration) -> RailResult<()> {
    if depth > self.limits.depth {
      return Err(RailError::message("native source depth bound exceeded"));
    }
    if self.bytes_hashed > self.limits.bytes_hashed {
      return Err(RailError::message("native source byte bound exceeded"));
    }
    if elapsed > self.limits.elapsed {
      return Err(RailError::message("native source capture time bound exceeded"));
    }
    Ok(())
  }
}

fn capture_native_source_namespace(
  namespace: &Path,
  crate_root: &Path,
  source_root: &Path,
  started: Instant,
  budget: &mut NativeCaptureBudget,
) -> RailResult<(NativeSourceState, NativeCaptureGuard)> {
  let root_metadata = fs::symlink_metadata(namespace)?;
  if !root_metadata.is_dir() || crate::utils::is_symlink_or_reparse(&root_metadata) {
    return Err(RailError::message("native source namespace is not a real directory"));
  }
  let root = ObservationPath::capture(namespace, source_root, source_root);
  let mut entries = Vec::new();
  let mut guards = Vec::new();
  let mut pending = vec![(PathBuf::new(), 0usize)];
  let mut found_crate_root = false;
  budget.account_entry("")?;

  while let Some((relative_directory, depth)) = pending.pop() {
    budget.check(depth, started.elapsed())?;
    let absolute_directory = namespace.join(&relative_directory);
    let display = native_relative_path(&relative_directory)?;
    let before_metadata = fs::symlink_metadata(&absolute_directory)?;
    let directory_mode = semantic_mode(&before_metadata);
    let before = native_metadata_guard(&absolute_directory, &before_metadata)?;
    if before.entry_type != NativeGuardEntryType::Directory {
      return Err(RailError::message(
        "native source directory changed type during capture",
      ));
    }
    let child_depth = depth.saturating_add(1);
    let children = collect_native_directory_children(
      fs::read_dir(&absolute_directory)?.map(|entry| entry.map(|entry| entry.file_name())),
      &relative_directory,
      child_depth,
      started,
      budget,
    )?;
    let after = native_metadata_guard(&absolute_directory, &fs::symlink_metadata(&absolute_directory)?)?;
    if before != after {
      return Err(RailError::message("native source directory changed during capture"));
    }
    entries.push(NativeSourceEntry {
      path: display.clone(),
      kind: NativeSourceEntryKind::Directory { mode: directory_mode },
    });
    guards.push(NativeGuardEntry {
      path: display,
      metadata: before,
    });

    for (child, relative) in children.into_iter().rev() {
      let absolute = namespace.join(&child);
      let metadata = fs::symlink_metadata(&absolute)?;
      if crate::utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::message(format!(
          "native source namespace contains a symlink or reparse point at '{}'",
          absolute.display()
        )));
      }
      if metadata.is_dir() {
        pending.push((child, child_depth));
        continue;
      }
      if !metadata.is_file() {
        return Err(RailError::message(format!(
          "native source namespace contains an unsupported entry at '{}'",
          absolute.display()
        )));
      }
      let mode = semantic_mode(&metadata);
      let (content_digest, guard, bytes) = capture_guarded_file(&absolute, started, budget)?;
      if absolute == crate_root {
        found_crate_root = true;
      }
      entries.push(NativeSourceEntry {
        path: relative.clone(),
        kind: NativeSourceEntryKind::RegularFile {
          bytes,
          content_digest,
          mode,
        },
      });
      guards.push(NativeGuardEntry {
        path: relative,
        metadata: guard,
      });
    }
  }

  if !found_crate_root {
    return Err(RailError::message(
      "declared crate root disappeared from its source namespace",
    ));
  }
  entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
  guards.sort_unstable_by(|left, right| left.path.cmp(&right.path));
  Ok((
    NativeSourceState {
      version: 1,
      root,
      entries,
    },
    NativeCaptureGuard { entries: guards },
  ))
}

fn collect_native_directory_children<I>(
  children: I,
  relative_directory: &Path,
  child_depth: usize,
  started: Instant,
  budget: &mut NativeCaptureBudget,
) -> RailResult<Vec<(PathBuf, String)>>
where
  I: Iterator<Item = std::io::Result<OsString>>,
{
  let mut bounded = Vec::new();
  for child in children {
    budget.check(child_depth, started.elapsed())?;
    let child = relative_directory.join(child?);
    let relative = native_relative_path(&child)?;
    budget.account_entry(&relative)?;
    bounded.push((child, relative));
  }
  budget.check(0, started.elapsed())?;
  bounded.sort_unstable_by(|left, right| left.0.cmp(&right.0));
  budget.check(0, started.elapsed())?;
  Ok(bounded)
}

fn native_relative_path(path: &Path) -> RailResult<String> {
  if path.as_os_str().is_empty() {
    return Ok(String::new());
  }
  let normalized = path
    .to_str()
    .ok_or_else(|| RailError::message("native source path is not valid UTF-8"))?
    .replace('\\', "/");
  if normalized.is_empty()
    || normalized.as_bytes().contains(&0)
    || Path::new(&normalized).is_absolute()
    || Path::new(&normalized)
      .components()
      .any(|component| matches!(component, std::path::Component::ParentDir))
  {
    return Err(RailError::message("native source path is not canonical"));
  }
  Ok(normalized)
}

fn capture_guarded_file(
  path: &Path,
  started: Instant,
  budget: &mut NativeCaptureBudget,
) -> RailResult<(String, NativeMetadataGuard, u64)> {
  let before_metadata = fs::symlink_metadata(path)?;
  if !before_metadata.is_file() || crate::utils::is_symlink_or_reparse(&before_metadata) {
    return Err(RailError::message("native source entry is not a real regular file"));
  }
  let before = native_metadata_guard(path, &before_metadata)?;
  let mut file = File::open(path)?;
  if !crate::utils::opened_file_matches_path(&file, path, before.len)? {
    return Err(RailError::message("native source file changed before it was read"));
  }
  let mut hasher = Sha256::new();
  let mut bytes = 0u64;
  let mut buffer = [0u8; 64 * 1024];
  loop {
    budget.check(0, started.elapsed())?;
    let read = file.read(&mut buffer)?;
    if read == 0 {
      break;
    }
    bytes = bytes.saturating_add(read as u64);
    budget.account_hashed_bytes(read as u64)?;
    hasher.update(&buffer[..read]);
  }
  let after_metadata = fs::symlink_metadata(path)?;
  let after = native_metadata_guard(path, &after_metadata)?;
  if before != after || !crate::utils::opened_file_matches_path(&file, path, before.len)? || bytes != before.len {
    return Err(RailError::message("native source file changed while it was read"));
  }
  crate::instrumentation::record_hash_operation();
  crate::instrumentation::record_hash_input_bytes(bytes as usize);
  crate::instrumentation::record_hashed_file_bytes_read(bytes as usize);
  Ok((
    format!("sha256:{}", ContentDigest::from_sha256_bytes(hasher.finalize().into())),
    before,
    bytes,
  ))
}

fn artifact_name(path: &Path) -> RailResult<String> {
  path
    .file_name()
    .and_then(OsStr::to_str)
    .filter(|name| !name.is_empty())
    .map(str::to_string)
    .ok_or_else(|| RailError::message("native dependency artifact has no UTF-8 file name"))
}

fn native_metadata_guard(_path: &Path, metadata: &fs::Metadata) -> RailResult<NativeMetadataGuard> {
  let entry_type = if metadata.is_dir() {
    NativeGuardEntryType::Directory
  } else if metadata.is_file() {
    NativeGuardEntryType::RegularFile
  } else {
    return Err(RailError::message("native source entry has an unsupported type"));
  };
  let modified = metadata.modified()?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt as _;
    Ok(NativeMetadataGuard {
      entry_type,
      len: metadata.len(),
      modified,
      readonly: metadata.permissions().readonly(),
      device: metadata.dev(),
      inode: metadata.ino(),
      mode: metadata.mode(),
      changed_seconds: metadata.ctime(),
      changed_nanoseconds: metadata.ctime_nsec(),
    })
  }
  #[cfg(windows)]
  {
    use std::os::windows::fs::MetadataExt as _;

    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0010;
    let opened = crate::windows_fs::open_for_observation(_path)?;
    let observation = crate::windows_fs::observe_file(&opened)?;
    crate::windows_fs::prove_local_ntfs(&opened, observation.volume_serial_number)?;
    let current = crate::windows_fs::open_for_observation(_path)?;
    let current_observation = crate::windows_fs::observe_file(&current)?;
    crate::windows_fs::prove_local_ntfs(&current, current_observation.volume_serial_number)?;
    let observed_type = if observation.file_attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
      NativeGuardEntryType::Directory
    } else {
      NativeGuardEntryType::RegularFile
    };
    if observation != current_observation
      || observed_type != entry_type
      || observation.size != metadata.len()
      || observation.file_attributes != metadata.file_attributes()
      || observation.creation_time != metadata.creation_time()
      || observation.last_write_time != metadata.last_write_time()
    {
      return Err(RailError::message(
        "native source path changed while its NTFS identity was captured",
      ));
    }
    Ok(NativeMetadataGuard {
      entry_type,
      len: observation.size,
      modified,
      readonly: metadata.permissions().readonly(),
      volume_serial_number: observation.volume_serial_number,
      file_id: observation.file_id,
      file_attributes: observation.file_attributes,
      creation_time: observation.creation_time,
      last_write_time: observation.last_write_time,
      change_time: observation.change_time,
      number_of_links: observation.number_of_links,
    })
  }
  #[cfg(not(any(unix, windows)))]
  {
    Ok(NativeMetadataGuard {
      entry_type,
      len: metadata.len(),
      modified,
      readonly: metadata.permissions().readonly(),
    })
  }
}

#[cfg(unix)]
fn semantic_mode(metadata: &fs::Metadata) -> u32 {
  use std::os::unix::fs::PermissionsExt as _;
  metadata.permissions().mode()
}

#[cfg(windows)]
fn semantic_mode(metadata: &fs::Metadata) -> u32 {
  use std::os::windows::fs::MetadataExt as _;
  metadata.file_attributes()
}

#[cfg(not(any(unix, windows)))]
fn semantic_mode(metadata: &fs::Metadata) -> u32 {
  u32::from(metadata.permissions().readonly())
}

fn executable_mode_from_guard(metadata: &NativeMetadataGuard) -> bool {
  #[cfg(unix)]
  {
    metadata.mode & 0o111 != 0
  }
  #[cfg(not(unix))]
  {
    false
  }
}

#[cfg(unix)]
const fn source_mode_executable(mode: u32) -> bool {
  mode & 0o111 != 0
}

#[cfg(not(unix))]
const fn source_mode_executable(_mode: u32) -> bool {
  false
}

fn capture_approved_environment(
  source_root: &Path,
  source_root_spelling: &Path,
  capture: &NativeActionCapture,
  names: &[String],
  started: Instant,
) -> RailResult<(ApprovedEnvState, u64)> {
  validate_environment_selector_names(names.iter().map(String::as_str))?;
  if crate::utils::canonicalize_existing(source_root_spelling)? != crate::utils::canonicalize_existing(source_root)? {
    return Err(RailError::message(
      "compiler source-root spelling changed before environment capture",
    ));
  }
  let mut root_bindings = vec![(source_root_spellings(source_root_spelling)?, PORTABLE_SOURCE_ROOT)];
  if let Some(package) = &capture.package_binding {
    root_bindings.push((source_root_spellings(&package.spelling)?, PORTABLE_PACKAGE_ROOT));
  }
  let mut entries = Vec::with_capacity(names.len());
  let mut bytes_hashed = 0u64;
  for name in names {
    if started.elapsed() > MAX_SOURCE_CAPTURE_TIME {
      return Err(RailError::message(
        "compiler environment capture exceeded its time bound",
      ));
    }
    let value = std::env::var_os(name);
    let (value_digest, root_mapped) = if let Some(value) = value {
      let mut value = value.as_encoded_bytes().to_vec();
      bytes_hashed = bytes_hashed
        .checked_add(value.len() as u64)
        .ok_or_else(|| RailError::message("compiler environment exceeds its byte bound"))?;
      if bytes_hashed > MAX_COMPILER_ENVIRONMENT_BYTES {
        return Err(RailError::message("compiler environment exceeds its capture bound"));
      }
      let mut root_mapped = false;
      for (spellings, token) in &root_bindings {
        let (next, replaced) = replace_source_root_spellings(&value, spellings, token);
        value = next;
        root_mapped |= replaced;
      }
      (Some(format!("sha256:{}", ContentDigest::sha256(&value))), root_mapped)
    } else {
      (None, false)
    };
    entries.push(ApprovedEnvEntry {
      name: name.clone(),
      value_digest,
      root_mapped,
    });
  }
  Ok((ApprovedEnvState { version: 3, entries }, bytes_hashed))
}

fn source_root_spellings(source_root: &Path) -> RailResult<Vec<Vec<u8>>> {
  let canonical = crate::utils::canonicalize_existing(source_root)?;
  if canonical.parent().is_none() {
    return Err(RailError::message(
      "filesystem roots cannot be portable compiler source roots",
    ));
  }
  let mut spellings = [source_root, canonical.as_path()]
    .into_iter()
    .flat_map(source_root_path_spellings)
    .filter(|spelling| !spelling.is_empty())
    .collect::<Vec<_>>();
  if spellings.iter().any(|spelling| {
    [PORTABLE_SOURCE_ROOT, PORTABLE_PACKAGE_ROOT]
      .into_iter()
      .any(|token| spelling.windows(token.len()).any(|window| window == token.as_bytes()))
  }) {
    return Err(RailError::message(
      "compiler source root collides with the reserved portable root",
    ));
  }
  spellings.sort_unstable_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
  spellings.dedup();
  Ok(spellings)
}

#[cfg(unix)]
fn source_root_path_spellings(path: &Path) -> Vec<Vec<u8>> {
  vec![path.as_os_str().as_encoded_bytes().to_vec()]
}

#[cfg(windows)]
fn source_root_path_spellings(path: &Path) -> Vec<Vec<u8>> {
  let native = path.as_os_str().as_encoded_bytes().to_vec();
  let forward = crate::utils::path_to_git_format(path).into_bytes();
  let backward = forward
    .iter()
    .map(|byte| if *byte == b'/' { b'\\' } else { *byte })
    .collect();
  vec![native, forward, backward]
}

#[cfg(not(any(unix, windows)))]
fn source_root_path_spellings(path: &Path) -> Vec<Vec<u8>> {
  vec![path.as_os_str().as_encoded_bytes().to_vec()]
}

#[cfg(windows)]
fn source_root_display_bytes(source_root: &Path) -> Vec<u8> {
  crate::utils::path_to_git_format(source_root).into_bytes()
}

#[cfg(not(windows))]
fn source_root_display_bytes(source_root: &Path) -> Vec<u8> {
  source_root.as_os_str().as_encoded_bytes().to_vec()
}

fn replace_source_root_spellings(bytes: &[u8], spellings: &[Vec<u8>], token: &str) -> (Vec<u8>, bool) {
  spellings
    .iter()
    .fold((bytes.to_vec(), false), |(current, replaced), spelling| {
      let (next, count) = replace_bytes(&current, spelling, token.as_bytes());
      (next, replaced || count != 0)
    })
}

fn private_compiler_environment(name: &OsStr) -> bool {
  if private_test_compiler_environment(name) {
    return true;
  }
  matches!(
    name.to_str(),
    Some(
      SESSION_ENV
        | DISPOSITION_ENV
        | LEGACY_STORE_ENV
        | crate::remote_cache::TARGETS_ENV
        | crate::hermetic::cas::CACHE_BASE_ENV
        | crate::hermetic::cas::CACHE_MAX_BYTES_ENV
        | crate::hermetic::cas::CACHE_TRUST_DOMAIN_ENV
        | crate::compiler::wrapper::CACHE_WRAPPER_MARKER
        | crate::compiler::wrapper::WRAPPER_MARKER
        | crate::compiler::wrapper::INNER_WRAPPER_ENV
        | crate::compiler::wrapper::RUSTDOC_WRAPPER_MARKER
        | crate::compiler::wrapper::INNER_RUSTDOC_ENV
        | crate::compiler::wrapper::OBSERVATION_DIRECTORY_ENV
        | crate::compiler::wrapper::OBSERVATION_SOURCE_ROOT_ENV
        | crate::compiler::wrapper::OBSERVATION_ONLY_ENV
    )
  )
}

#[cfg(debug_assertions)]
fn private_test_compiler_environment(name: &OsStr) -> bool {
  matches!(
    name.to_str(),
    Some(
      RESTORE_FAULT_ENV
        | RESTORE_ABORT_ENV
        | RESTORE_CANCEL_ENV
        | RESTORE_CRATE_ENV
        | TEST_CAPTURE_LIMIT_ENV
        | CAPTURE_PAUSE_PHASE_ENV
        | CAPTURE_PAUSE_CRATE_ENV
        | CAPTURE_PAUSE_DIRECTORY_ENV
    )
  )
}

#[cfg(not(debug_assertions))]
fn private_test_compiler_environment(_name: &OsStr) -> bool {
  false
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
      Self::Bypassed(_) | Self::OperationalFailure(_) => None,
    }
  }

  pub(crate) fn bypass_reason(&self) -> Option<&'static str> {
    match self {
      Self::Active(_) => None,
      Self::Bypassed(reason) => Some(reason.as_str()),
      Self::OperationalFailure(_) => None,
    }
  }

  pub(crate) fn cargo_config_argument(&self) -> Option<&OsStr> {
    match self {
      Self::Active(run) => Some(&run.cargo_config),
      Self::Bypassed(_) | Self::OperationalFailure(_) => None,
    }
  }

  pub(crate) fn operational_failure(&self) -> Option<&str> {
    match self {
      Self::OperationalFailure(message) => Some(message),
      Self::Active(_) | Self::Bypassed(_) => None,
    }
  }

  pub(crate) fn remote_active(&self) -> bool {
    matches!(self, Self::Active(run) if run.remote.is_some())
  }
}

impl DirectNativeCacheRun {
  fn report(&self) -> DirectNativeCacheReport {
    let publication = self.publication.as_ref().map(publication::Coordinator::drain);
    let mut report = DirectNativeCacheReport {
      setup_bytes_hashed: self.setup_bytes_hashed.saturating_add(
        publication
          .as_ref()
          .map_or(0, |publication| publication.setup_bytes_hashed),
      ),
      environment_selector_diverged: publication.is_some_and(|publication| publication.selector_diverged),
      ..DirectNativeCacheReport::default()
    };
    let units = native_cache_unit_evidence(self.observations.path());
    let mut wrapper_events = Vec::new();
    let mut publication_handles = BTreeMap::new();
    let mut ambiguous_publication_handles = BTreeSet::new();
    let directory = self.observations.path().join("native-cache-events");
    let entries = fs::read_dir(directory).ok();
    for entry in entries.into_iter().flatten().filter_map(Result::ok) {
      let Ok(bytes) = read_bounded(&entry.path(), MAX_SESSION_BYTES as usize) else {
        continue;
      };
      let Ok(event) = serde_json::from_slice::<OwnedNativeCacheEvent>(&bytes) else {
        continue;
      };
      if event.version != NATIVE_CACHE_RUN_EVENT_VERSION {
        continue;
      }
      if let (Some(action_key), Some(result_key), Some(base_action_key)) =
        (&event.action_key, &event.result_key, &event.base_action_key)
        && validate_action_key(action_key).is_ok()
        && validate_result_key(result_key).is_ok()
        && validate_base_action_key(base_action_key).is_ok()
      {
        let handle = (action_key.clone(), result_key.clone());
        match publication_handles.get(&handle) {
          Some(existing) if existing != base_action_key => {
            ambiguous_publication_handles.insert(handle);
          }
          Some(_) => {}
          None => {
            publication_handles.insert(handle, base_action_key.clone());
          }
        }
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
      report.environment_selector_diverged |=
        native_cache_reason_contains(&event.reason, "environment_selector_diverged");
      *report.reasons.entry(event.reason.clone()).or_default() += 1;
      if let Some(trace) = event.wrapper_trace.clone() {
        wrapper_events.push(NativeCacheWrapperEventDiagnostics::new(
          event.action_key.clone(),
          event.status.as_str(),
          event.reason.clone(),
          trace,
        ));
      }
      let unit = event
        .action_key
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
        unit_identity: event.action_key,
        outcome: event.status,
        reason: event.reason,
        result_key: event.result_key,
        unit,
      });
    }
    report.events.sort_by(|left, right| {
      (&left.unit_identity, &left.result_key, &left.outcome, &left.reason).cmp(&(
        &right.unit_identity,
        &right.result_key,
        &right.outcome,
        &right.reason,
      ))
    });
    report.wrapper_diagnostics = NativeCacheWrapperDiagnostics::from_events(wrapper_events);
    if self.remote_configuration_failed {
      *report
        .reasons
        .entry("remote_configuration_unavailable".to_string())
        .or_default() += 1;
    }
    if self.publication_configuration_failed {
      *report
        .reasons
        .entry("local_publication_coordinator_unavailable".to_string())
        .or_default() += 1;
    }
    if let Some(publication) = publication
      && publication.rejected > 0
    {
      *report
        .reasons
        .entry("local_publication_request_rejected".to_string())
        .or_default() += publication.rejected;
    }
    if let Some(publication) = publication
      && publication.shutdown_abandoned > 0
    {
      *report
        .reasons
        .entry("local_publication_shutdown_abandoned".to_string())
        .or_default() += publication.shutdown_abandoned;
    }
    if publication.is_some_and(|publication| publication.session_failed) {
      *report
        .reasons
        .entry("exact_publication_session_unavailable".to_string())
        .or_default() += 1;
    }
    if publication.is_some_and(|publication| publication.selector_diverged) {
      *report
        .reasons
        .entry("environment_selector_diverged".to_string())
        .or_default() += 1;
    }
    if let Some(remote) = &self.remote {
      if remote.can_publish()
        && let Ok(cas) = LocalCas::open_initialized()
      {
        for ((action_key, result_key), base_action_key) in publication_handles {
          if ambiguous_publication_handles.contains(&(action_key.clone(), result_key.clone())) {
            *report
              .reasons
              .entry("remote_publication_handle_rejected".to_string())
              .or_default() += 1;
            continue;
          }
          match cas.native_result_needs_remote_publication(&action_key, &result_key, remote.authority()) {
            Ok(true) => {
              if remote.publish(&action_key, &result_key, &base_action_key).is_err() {
                *report
                  .reasons
                  .entry("remote_publication_failed".to_string())
                  .or_default() += 1;
              }
            }
            Ok(false) => {}
            Err(_) => {
              *report
                .reasons
                .entry("remote_publication_handle_rejected".to_string())
                .or_default() += 1;
            }
          }
        }
      }
      report.remote = Some(remote.report());
    }
    report
  }
}

fn native_cache_reason_contains(reason: &str, expected: &str) -> bool {
  reason.split(';').any(|segment| segment == expected)
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
      .and_then(CompilerCacheWrapperMetadata::action_key)
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
      && validate_action_key(&persisted.unit_identity).is_ok()
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
  let DirectNativeCacheIdentity {
    source_root,
    source_root_spelling,
    session,
    deferred_session,
    wrapper_plan,
    setup_bytes_hashed,
    l2_alias,
    retain_event_evidence,
  } = identity;
  if let Some(reason) = direct_cache_bypass_reason(wrapper_plan) {
    return DirectNativeCacheSetup::Bypassed(reason);
  }
  let discovery_only = session.authority == NativeSessionAuthority::Discovery;
  let observations = match private_command_directory() {
    Ok(directory) => directory,
    Err(_) => return DirectNativeCacheSetup::Bypassed(DirectCacheBypass::ObservationDirectoryUnavailable),
  };
  if session.persist(observations.path()).is_err() {
    return DirectNativeCacheSetup::Bypassed(DirectCacheBypass::SessionUnavailable);
  }
  let executable = match direct_wrapper_executable() {
    Ok(wrapper) => wrapper,
    Err(_) => return DirectNativeCacheSetup::Bypassed(DirectCacheBypass::WrapperExecutableUnavailable),
  };
  let wrapper = observations.path().join(DIRECT_WRAPPER_NAME);
  if create_direct_wrapper(&executable, &wrapper).is_err() {
    return DirectNativeCacheSetup::Bypassed(DirectCacheBypass::WrapperExecutableUnavailable);
  }
  let canonical_source_root = match crate::utils::canonicalize_existing(source_root) {
    Ok(root) => root,
    Err(_) => return DirectNativeCacheSetup::Bypassed(DirectCacheBypass::SourceRootUnavailable),
  };
  let source_root_spelling = if source_root_spelling.is_absolute() {
    source_root_spelling.to_path_buf()
  } else {
    match std::env::current_dir() {
      Ok(current) => current.join(source_root_spelling),
      Err(_) => return DirectNativeCacheSetup::Bypassed(DirectCacheBypass::SourceRootUnavailable),
    }
  };
  if crate::utils::canonicalize_existing(&source_root_spelling).ok().as_ref() != Some(&canonical_source_root) {
    return DirectNativeCacheSetup::Bypassed(DirectCacheBypass::SourceRootUnavailable);
  }
  let source_root = match canonical_source_root.to_str().map(str::to_string) {
    Some(root) => root,
    None => return DirectNativeCacheSetup::Bypassed(DirectCacheBypass::SourceRootUnavailable),
  };
  let source_root_spelling = match source_root_spelling.to_str().map(str::to_string) {
    Some(root) => root,
    None => return DirectNativeCacheSetup::Bypassed(DirectCacheBypass::SourceRootUnavailable),
  };
  let (publication, publication_configuration_failed) = match publication::Coordinator::prepare(
    Path::new(&source_root),
    observations.path(),
    &observations.path().join("native-cache-session").join(SESSION_FILE),
    deferred_session,
    discovery_only,
  ) {
    Ok(publication) => (Some(publication), false),
    Err(_) => (None, true),
  };
  if discovery_only && publication.is_none() {
    return DirectNativeCacheSetup::Bypassed(DirectCacheBypass::SessionUnavailable);
  }
  let (remote, remote_configuration_failed) =
    match crate::remote_cache::RemoteCoordinator::prepare(Path::new(&source_root), l2_alias) {
      Ok(remote) => {
        let configuration_failed = l2_alias.is_some() && remote.is_none();
        (remote, configuration_failed)
      }
      Err(error)
        if matches!(
          error.fault,
          crate::remote_cache::RemoteStoreFault::Unavailable
            | crate::remote_cache::RemoteStoreFault::Authentication
            | crate::remote_cache::RemoteStoreFault::Configuration
        ) =>
      {
        crate::remote_cache::warn_unavailable_once();
        (None, true)
      }
      Err(error) => {
        return DirectNativeCacheSetup::OperationalFailure(format!("remote cache setup failed: {error}"));
      }
    };
  let context = DirectNativeCacheContext {
    version: 8,
    source_root,
    source_root_spelling,
    discovery_only,
    retain_event_evidence,
    capture_wrapper_diagnostics: crate::instrumentation::enabled(),
    remote: remote.as_ref().map(crate::remote_cache::RemoteCoordinator::context),
    publication: publication.as_ref().map(publication::Coordinator::context),
  };
  if serde_json::to_vec(&context)
    .ok()
    .and_then(|bytes| write_private_command_file(&observations.path().join(DIRECT_CONTEXT_FILE), &bytes).ok())
    .is_none()
  {
    return DirectNativeCacheSetup::Bypassed(DirectCacheBypass::SessionUnavailable);
  }
  let wrapper = match wrapper.to_str().and_then(|path| serde_json::to_string(path).ok()) {
    Some(wrapper) => wrapper,
    None => return DirectNativeCacheSetup::Bypassed(DirectCacheBypass::WrapperExecutableUnavailable),
  };
  DirectNativeCacheSetup::Active(Box::new(DirectNativeCacheRun {
    publication,
    observations,
    cargo_config: format!("build.rustc-wrapper={wrapper}").into(),
    setup_bytes_hashed,
    remote,
    remote_configuration_failed,
    publication_configuration_failed,
  }))
}

pub(crate) fn direct_wrapper_executable() -> RailResult<PathBuf> {
  let cargo_rail_executable = std::env::current_exe()?;
  Ok(
    cargo_rail_executable
      .parent()
      .map(|directory| directory.join(DIRECT_WRAPPER_NAME))
      .filter(|candidate| {
        fs::metadata(candidate).is_ok_and(|metadata| metadata.is_file() && executable_metadata(&metadata))
      })
      .unwrap_or(cargo_rail_executable),
  )
}

#[cfg(unix)]
fn executable_metadata(metadata: &fs::Metadata) -> bool {
  use std::os::unix::fs::PermissionsExt as _;

  metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable_metadata(_metadata: &fs::Metadata) -> bool {
  true
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
  match fs::hard_link(executable, wrapper) {
    Ok(()) => Ok(()),
    Err(_) => fs::copy(executable, wrapper).map(|_| ()),
  }
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
    let source_root = std::env::var_os(crate::compiler::wrapper::OBSERVATION_SOURCE_ROOT_ENV).map(PathBuf::from)?;
    Some(Self {
      session: std::env::var_os(SESSION_ENV).map(PathBuf::from)?,
      source_root_spelling: source_root.clone(),
      source_root,
      observation_directory: std::env::var_os(crate::compiler::wrapper::OBSERVATION_DIRECTORY_ENV)
        .map(PathBuf::from)?,
      discovery_only: false,
      retain_event_evidence: false,
      capture_wrapper_diagnostics: false,
      remote: None,
      publication: None,
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
    let file = File::open(&context_path)?;
    if !crate::utils::private_file_matches_path(&file, &context_path, metadata.len())? {
      return Err(RailError::message(
        "native compiler cache context changed while it was opened",
      ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_SESSION_BYTES.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.len() {
      return Err(RailError::message(
        "native compiler cache context changed while it was read",
      ));
    }
    let context: DirectNativeCacheContext = serde_json::from_slice(&bytes)?;
    if context.version != 8 {
      return Err(RailError::message(
        "native compiler cache context has an incompatible schema",
      ));
    }
    let source_root = PathBuf::from(context.source_root);
    let source_root_spelling = PathBuf::from(context.source_root_spelling);
    if !source_root.is_absolute()
      || !source_root_spelling.is_absolute()
      || source_root.as_os_str().as_encoded_bytes().contains(&0)
      || source_root_spelling.as_os_str().as_encoded_bytes().contains(&0)
      || crate::utils::canonicalize_existing(&source_root_spelling)? != source_root
    {
      return Err(RailError::message(
        "native compiler cache context has an invalid source root",
      ));
    }
    Ok(Self {
      session: directory.join("native-cache-session").join(SESSION_FILE),
      source_root,
      source_root_spelling,
      observation_directory: directory,
      discovery_only: context.discovery_only,
      retain_event_evidence: context.retain_event_evidence,
      capture_wrapper_diagnostics: context.capture_wrapper_diagnostics,
      remote: context.remote,
      publication: context.publication,
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
  /// Create a provisional session for an empty-authority command.
  ///
  /// Discovery sessions never resolve or publish authority. Their compiler
  /// class is deliberately provisional so exact toolchain probing can overlap
  /// Cargo startup; publication reclassifies every completed invocation under
  /// the exact session before it can become authoritative.
  pub(crate) fn capture_discovery(
    source_root: &Path,
    capability_identity: &str,
    compiler_process_environment_identity: &str,
    execution_contract: &str,
  ) -> RailResult<Self> {
    Self::capture(
      source_root,
      "rustc deferred\nhost: cargo-rail-discovery\n",
      capability_identity,
      compiler_process_environment_identity,
      execution_contract,
      NativeSessionAuthority::Discovery,
    )
  }

  pub(crate) fn write(
    directory: &Path,
    source_root: &Path,
    rustc_verbose_version: &str,
    capability_identity: &str,
    compiler_process_environment_identity: &str,
    execution_contract: &str,
  ) -> RailResult<PathBuf> {
    Self::capture(
      source_root,
      rustc_verbose_version,
      capability_identity,
      compiler_process_environment_identity,
      execution_contract,
      NativeSessionAuthority::Exact,
    )?
    .persist(directory)
  }

  pub(crate) fn capture(
    source_root: &Path,
    rustc_verbose_version: &str,
    capability_identity: &str,
    compiler_process_environment_identity: &str,
    execution_contract: &str,
    authority: NativeSessionAuthority,
  ) -> RailResult<Self> {
    let source_root = crate::utils::canonicalize_existing(source_root)?;
    let source_root_identity = path_identity(&source_root)?;
    let class = NativeCompilerClass::capture(rustc_verbose_version);
    let identity = session_identity(
      &class,
      capability_identity,
      compiler_process_environment_identity,
      execution_contract,
      authority,
    )?;
    let session = Self {
      version: NATIVE_COMPILER_SESSION_VERSION,
      identity,
      source_root_identity,
      class,
      capability_identity: capability_identity.to_string(),
      compiler_process_environment_identity: compiler_process_environment_identity.to_string(),
      execution_contract: execution_contract.to_string(),
      authority,
    };
    session.validate_object()?;
    Ok(session)
  }

  fn persist(self, directory: &Path) -> RailResult<PathBuf> {
    if self.class.is_valid() {
      LocalCas::open()?;
    }
    let session_directory = directory.join("native-cache-session");
    fs::create_dir(&session_directory)?;
    let path = session_directory.join(SESSION_FILE);
    write_private_command_file(&path, &serde_json::to_vec(&self)?)?;
    Ok(path)
  }

  fn load(path: &Path, source_root: &Path) -> RailResult<Self> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) || metadata.len() > MAX_SESSION_BYTES {
      return Err(RailError::message(
        "native compiler cache session is not a bounded regular file",
      ));
    }
    let file = File::open(path)?;
    if !crate::utils::private_file_matches_path(&file, path, metadata.len())? {
      return Err(RailError::message(
        "native compiler cache session changed while it was opened",
      ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_SESSION_BYTES.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.len() {
      return Err(RailError::message(
        "native compiler cache session changed while it was read",
      ));
    }
    let session: Self = serde_json::from_slice(&bytes)?;
    session.validate_object()?;
    if session.source_root_identity != path_identity(source_root)? {
      return Err(RailError::message("native compiler cache session source root changed"));
    }
    Ok(session)
  }

  fn validate_object(&self) -> RailResult<()> {
    if self.version != NATIVE_COMPILER_SESSION_VERSION {
      return Err(RailError::message(
        "native compiler cache session has an incompatible schema",
      ));
    }
    for digest in [
      &self.identity,
      &self.source_root_identity,
      &self.capability_identity,
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
      &self.compiler_process_environment_identity,
      &self.execution_contract,
      self.authority,
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
  mode: u32,
}

/// Canonical proof and output descriptor for one exact pre-executable action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeCompilerValidation {
  version: u32,
  action_key: String,
  result_key: String,
  session_identity: String,
  session_authority: NativeSessionAuthority,
  class: NativeCompilerClass,
  witness: NativeCompilerWitness,
  observation: RawCompilerInvocation,
  compiler_environment_names: Vec<String>,
  outputs: Vec<NativeCompilerOutput>,
  stdout_digest: String,
  stdout_bytes: u64,
  stderr_digest: String,
  stderr_bytes: u64,
}

impl NativeCompilerValidation {
  fn new(
    session: &NativeCompilerSession,
    observation: RawCompilerInvocation,
    approved_environment: &ApprovedEnvState,
    descriptor: pack::NativeResultDescriptor,
  ) -> RailResult<Self> {
    let result_key = descriptor.result_key()?;
    let pack::NativeResultDescriptor {
      action_key,
      witness,
      outputs,
      stdout_digest,
      stdout_bytes,
      stderr_digest,
      stderr_bytes,
    } = descriptor;
    let validation = Self {
      version: 9,
      action_key,
      result_key,
      session_identity: session.identity.clone(),
      session_authority: session.authority,
      class: session.class.clone(),
      witness,
      observation,
      compiler_environment_names: approved_environment
        .entries
        .iter()
        .map(|entry| entry.name.clone())
        .collect(),
      outputs,
      stdout_digest,
      stdout_bytes,
      stderr_digest,
      stderr_bytes,
    };
    validation.validate_object()?;
    Ok(validation)
  }

  fn rebind_discovery_session(
    &self,
    discovery: &NativeCompilerSession,
    exact: &NativeCompilerSession,
    proof: &NativePublicationProof,
    source_root: &Path,
  ) -> RailResult<Self> {
    self.validate_object()?;
    discovery.validate_object()?;
    exact.validate_object()?;
    proof.validate_object()?;
    if discovery.authority != NativeSessionAuthority::Discovery
      || exact.authority != NativeSessionAuthority::Exact
      || self.session_authority != NativeSessionAuthority::Discovery
      || self.session_identity != discovery.identity
      || self.class != discovery.class
      || discovery.source_root_identity != exact.source_root_identity
      || exact.source_root_identity != path_identity(source_root)?
      || discovery.compiler_process_environment_identity != exact.compiler_process_environment_identity
      || discovery.execution_contract != exact.execution_contract
      || !self
        .compiler_environment_names
        .iter()
        .eq(proof.approved_environment.entries.iter().map(|entry| &entry.name))
    {
      return Err(RailError::message(
        "native discovery result does not match its exact compiler session",
      ));
    }
    if invocation_bypass_reason(&self.observation, true, &exact.class.host_target).is_some() {
      return Err(RailError::message(
        "native discovery result is not eligible under its exact compiler session",
      ));
    }
    let capture = NativeActionCapture::from_publication_proof(&self.observation, source_root, proof)?;
    if action_key(&discovery.identity, &discovery.class, &self.observation, &capture)? != self.action_key
      || !capture.validates_witness(&self.witness, &self.observation)
    {
      return Err(RailError::message(
        "native discovery key material does not match its provisional action",
      ));
    }
    let action_key = action_key(&exact.identity, &exact.class, &self.observation, &capture)?;
    let mut observation = self.observation.clone();
    let prior = observation
      .cache_wrapper
      .as_ref()
      .ok_or_else(|| RailError::message("native discovery result has no cache disposition"))?;
    observation.cache_wrapper = Some(CompilerCacheWrapperMetadata::native(
      CompilerCacheWrapperStatus::Miss,
      prior.reason(),
      Some(action_key.clone()),
      None,
      prior.bytes_hashed(),
      0,
    ));
    Self::new(
      exact,
      observation,
      &proof.approved_environment,
      pack::NativeResultDescriptor {
        action_key,
        witness: self.witness.clone(),
        outputs: self.outputs.clone(),
        stdout_digest: self.stdout_digest.clone(),
        stdout_bytes: self.stdout_bytes,
        stderr_digest: self.stderr_digest.clone(),
        stderr_bytes: self.stderr_bytes,
      },
    )
  }

  pub(crate) fn is_authoritative(&self) -> bool {
    self.session_authority == NativeSessionAuthority::Exact
  }

  pub(crate) fn action_key(&self) -> &str {
    &self.action_key
  }

  pub(crate) fn result_key(&self) -> &str {
    &self.result_key
  }

  fn publication_base_action_key(
    &self,
    session: &NativeCompilerSession,
    source_root: &Path,
    proof: &NativePublicationProof,
  ) -> RailResult<String> {
    let capture = NativeActionCapture::from_publication_proof(&self.observation, source_root, proof)?;
    base_action_key(&session.identity, &session.class, &self.observation, &capture)
  }

  fn revalidate_publication(
    &self,
    session: &NativeCompilerSession,
    source_root: &Path,
    proof: &NativePublicationProof,
  ) -> RailResult<u64> {
    self.validate_object()?;
    session.validate_object()?;
    proof.validate_object()?;
    if session.source_root_identity != path_identity(source_root)?
      || session.authority != NativeSessionAuthority::Exact
      || self.session_identity != session.identity
      || self.session_authority != session.authority
      || self.class != session.class
      || !self
        .compiler_environment_names
        .iter()
        .eq(proof.approved_environment.entries.iter().map(|entry| &entry.name))
    {
      return Err(RailError::message(
        "native publication proof does not match its compiler session",
      ));
    }
    capture_test_pause("before_admission_revalidation", &self.observation)?;
    let capture = NativeActionCapture::capture_with_publication_proof(&self.observation, source_root, proof)?;
    if action_key(&session.identity, &session.class, &self.observation, &capture)? != self.action_key
      || !capture.validates_witness(&self.witness, &self.observation)
      || capture.guard_identity()? != proof.guard_identity
    {
      return Err(RailError::message(
        "native compiler inputs changed before publication authority",
      ));
    }
    Ok(capture.bytes_hashed.saturating_add(proof.environment_bytes_hashed))
  }

  pub(crate) fn remote_environment_is_approved(&self, approved_names: &[String]) -> bool {
    self
      .compiler_environment_names
      .iter()
      .all(|name| approved_names.binary_search(name).is_ok())
  }

  /// Verify that a remote-publication capability names the base action bound by
  /// this exact action, then return its canonical compiler-selected names.
  pub(crate) fn remote_publication_environment_names(&self, base_action_key: &str) -> RailResult<&[String]> {
    self.validate_object()?;
    let approved_environment = ApprovedEnvState {
      version: 3,
      entries: self
        .observation
        .environment_reads
        .iter()
        .map(|environment| ApprovedEnvEntry {
          name: environment.name.clone(),
          value_digest: environment.value_digest.clone(),
          // Authoritative observations reject root-mapped environment values.
          // Reconstructing `false` is therefore exact for publishable actions
          // and makes every other action fail the binding check below.
          root_mapped: false,
        })
        .collect(),
    };
    if action_key_from_base(base_action_key, &approved_environment)? != self.action_key {
      return Err(RailError::message(
        "native remote-publication base action does not bind its exact action",
      ));
    }
    Ok(&self.compiler_environment_names)
  }

  pub(crate) fn result_digest(&self, _output_manifest: &str) -> String {
    self.result_key.clone()
  }

  pub(crate) fn cas_output_bindings(&self) -> impl Iterator<Item = (&str, &str, u64, u32)> {
    self.outputs.iter().map(|output| {
      (
        output.slot.as_str(),
        output.content_digest.as_str(),
        output.bytes,
        output.mode,
      )
    })
  }

  pub(crate) fn cas_stream_bindings(&self) -> [(&str, &str, u64, u32); 2] {
    [
      (STDOUT_SLOT, self.stdout_digest.as_str(), self.stdout_bytes, 0o644),
      (STDERR_SLOT, self.stderr_digest.as_str(), self.stderr_bytes, 0o644),
    ]
  }

  pub(crate) fn validate_object(&self) -> RailResult<()> {
    if self.version != 9 {
      return Err(RailError::message(
        "native compiler observation has an incompatible schema",
      ));
    }
    validate_identity(&self.action_key, ACTION_KEY_PREFIX)?;
    validate_identity(&self.result_key, RESULT_KEY_PREFIX)?;
    for digest in [&self.session_identity, &self.stdout_digest, &self.stderr_digest] {
      validate_sha256(digest)?;
    }
    let observed_environment_names = self
      .observation
      .environment_reads
      .iter()
      .map(|environment| environment.name.as_str());
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
        .any(|output| validate_sha256(&output.content_digest).is_err() || !valid_native_output_mode(output.mode))
      || !complete_library_observation(&self.observation)
      || !outputs_match_observation(&self.outputs, &self.observation.emitted_outputs)
      || validate_environment_selector_names(self.compiler_environment_names.iter().map(String::as_str)).is_err()
      || validate_environment_selector_names(observed_environment_names.clone()).is_err()
      || !self
        .compiler_environment_names
        .iter()
        .map(String::as_str)
        .eq(observed_environment_names.clone())
    {
      return Err(RailError::message(
        "native compiler observation is outside the graduated class",
      ));
    }
    if self.witness.version != 1
      || !self.witness.complete
      || !strictly_sorted_unique_strings(&self.witness.source_paths)
      || !strictly_sorted_unique_strings(&self.witness.dependency_names)
      || validate_environment_selector_names(self.witness.environment_names.iter().map(String::as_str)).is_err()
      || !self
        .witness
        .environment_names
        .iter()
        .map(String::as_str)
        .eq(observed_environment_names)
      || self.witness.source_paths.len() > MAX_SOURCE_ENTRIES
      || self.witness.dependency_names.len() > MAX_SOURCE_ENTRIES
      || self
        .witness
        .source_paths
        .iter()
        .any(|path| !native_relative_path(Path::new(path)).is_ok_and(|canonical| canonical == *path))
      || self
        .witness
        .dependency_names
        .iter()
        .any(|name| name.is_empty() || name.as_bytes().contains(&0))
    {
      return Err(RailError::message("native compiler witness is invalid"));
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
      if environment.secret_capability
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
    if result_key(
      &self.action_key,
      &self.witness,
      &self.outputs,
      &self.stdout_digest,
      self.stdout_bytes,
      &self.stderr_digest,
      self.stderr_bytes,
    )? != self.result_key
    {
      return Err(RailError::message(
        "native compiler result identity does not match its descriptor",
      ));
    }
    Ok(())
  }
}

impl NativePublicationProof {
  fn validate_object(&self) -> RailResult<()> {
    if self.version != 4 {
      return Err(RailError::message(
        "native publication proof has an incompatible schema",
      ));
    }
    validate_native_source_state(&self.source_state)?;
    match (&self.source_state.root, &self.package_binding) {
      (ObservationPath::Repository(_), None) => {}
      (ObservationPath::Host(_), Some(binding)) => binding.validate_object()?,
      _ => {
        return Err(RailError::message(
          "native publication proof has an invalid package binding",
        ));
      }
    }
    self.approved_environment.validate_object()?;
    validate_sha256(&self.guard_identity)
  }
}

fn validate_native_source_state(state: &NativeSourceState) -> RailResult<()> {
  if state.version != 1 || state.entries.is_empty() || state.entries.len() > MAX_SOURCE_ENTRIES {
    return Err(RailError::message("native publication SourceState is invalid"));
  }
  match &state.root {
    ObservationPath::Repository(path) => {
      crate::source::RepositoryPath::new(Path::new(path))?;
    }
    ObservationPath::Host(path) => {
      if !Path::new(path).is_absolute() || path.as_bytes().contains(&0) {
        return Err(RailError::message("native publication SourceState has an invalid root"));
      }
    }
  }
  let mut path_bytes = 0usize;
  let mut source_bytes = 0u64;
  for (index, entry) in state.entries.iter().enumerate() {
    if index > 0 && state.entries[index - 1].path >= entry.path {
      return Err(RailError::message(
        "native publication SourceState is not strictly ordered",
      ));
    }
    if native_relative_path(Path::new(&entry.path))? != entry.path
      || Path::new(&entry.path).components().count() > MAX_SOURCE_DEPTH
    {
      return Err(RailError::message(
        "native publication SourceState contains an invalid path",
      ));
    }
    path_bytes = path_bytes.saturating_add(entry.path.len());
    if let NativeSourceEntryKind::RegularFile {
      bytes, content_digest, ..
    } = &entry.kind
    {
      validate_sha256(content_digest)?;
      source_bytes = source_bytes.saturating_add(*bytes);
    }
  }
  if path_bytes > MAX_SOURCE_PATH_BYTES || source_bytes > MAX_SOURCE_BYTES {
    return Err(RailError::message("native publication SourceState exceeds its bound"));
  }
  Ok(())
}

fn strictly_sorted_unique_strings(values: &[String]) -> bool {
  values.windows(2).all(|pair| pair[0] < pair[1])
}

fn result_key(
  action_key: &str,
  witness: &NativeCompilerWitness,
  outputs: &[NativeCompilerOutput],
  stdout_digest: &str,
  stdout_bytes: u64,
  stderr_digest: &str,
  stderr_bytes: u64,
) -> RailResult<String> {
  pack::NativeResultDescriptor::from_identity(NativeResultIdentity {
    action_key,
    witness,
    outputs,
    stdout_digest,
    stdout_bytes,
    stderr_digest,
    stderr_bytes,
  })?
  .result_key()
}

fn session_identity(
  class: &NativeCompilerClass,
  capability_identity: &str,
  compiler_process_environment_identity: &str,
  execution_contract: &str,
  authority: NativeSessionAuthority,
) -> RailResult<String> {
  let class = serde_json::to_vec(class)?;
  Ok(sha256_identity(
    "sha256:",
    b"cargo-rail-native-compiler-session\0",
    &[
      (b"version", &9_u32.to_le_bytes()),
      (
        b"toolchain-capability-contract",
        &NATIVE_CACHE_IDENTITY_CONTRACT_VERSION.to_le_bytes(),
      ),
      (b"class", &class),
      (b"capability", capability_identity.as_bytes()),
      (
        b"compiler-process-environment",
        compiler_process_environment_identity.as_bytes(),
      ),
      (b"execution-contract", execution_contract.as_bytes()),
      (
        b"authority",
        match authority {
          NativeSessionAuthority::Exact => b"exact",
          NativeSessionAuthority::Discovery => b"discovery",
        },
      ),
    ],
  ))
}

fn action_key(
  session_identity: &str,
  class: &NativeCompilerClass,
  observation: &RawCompilerInvocation,
  capture: &NativeActionCapture,
) -> RailResult<String> {
  let base_action = base_action_key(session_identity, class, observation, capture)?;
  action_key_from_base(&base_action, &capture.approved_environment)
}

fn action_key_from_base(base_action: &str, approved_environment: &ApprovedEnvState) -> RailResult<String> {
  validate_identity(base_action, BASE_ACTION_KEY_PREFIX)?;
  approved_environment.validate_object()?;
  let approved_environment = serde_json::to_vec(approved_environment)?;
  Ok(sha256_identity(
    ACTION_KEY_PREFIX,
    b"cargo-rail-native-compiler-action\0",
    &[
      (b"version", &9_u32.to_le_bytes()),
      (b"base-action", base_action.as_bytes()),
      (b"approved-environment", &approved_environment),
    ],
  ))
}

fn base_action_key(
  session_identity: &str,
  class: &NativeCompilerClass,
  observation: &RawCompilerInvocation,
  capture: &NativeActionCapture,
) -> RailResult<String> {
  let class = serde_json::to_vec(class)?;
  let identity_inputs = portable_native_cache_key_inputs(observation, capture)?;
  let pre_execution = serde_json::to_vec(&(
    &observation.mode,
    &observation.crate_name,
    &observation.crate_types,
    &observation.target_argument,
    &observation.emit_modes,
    observation.test_mode,
    &identity_inputs,
  ))?;
  let source_state = serde_json::to_vec(&capture.portable_source_state()?)?;
  Ok(sha256_identity(
    BASE_ACTION_KEY_PREFIX,
    b"cargo-rail-native-compiler-base-action\0",
    &[
      (b"version", &4_u32.to_le_bytes()),
      (b"session", session_identity.as_bytes()),
      (b"class", &class),
      (b"pre-execution", &pre_execution),
      (b"source-state", &source_state),
    ],
  ))
}

fn portable_native_cache_key_inputs<'a>(
  observation: &'a RawCompilerInvocation,
  capture: &NativeActionCapture,
) -> RailResult<PortableNativeCacheKeyInputs<'a>> {
  let NativeCacheKeyInputs {
    cfg,
    mut compiler_arguments,
    declared_inputs,
    dependency_artifacts,
  } = native_cache_key_inputs(observation)?;
  let [declared] = declared_inputs else {
    return Err(RailError::message(
      "portable native action requires one declared crate root",
    ));
  };
  let portable_crate_root = capture.portable_crate_root()?;
  let source_arguments = compiler_arguments
    .iter()
    .enumerate()
    .filter(|(_, argument)| !argument.starts_with('-') && argument.ends_with(".rs"))
    .map(|(index, _)| index)
    .collect::<Vec<_>>();
  let [source_argument] = source_arguments.as_slice() else {
    return Err(RailError::message(
      "portable native action requires one positional Rust source argument",
    ));
  };
  let declared_name = observation_path_basename(&declared.path)
    .ok_or_else(|| RailError::message("portable native crate root has no file name"))?;
  if portable_path_basename(&compiler_arguments[*source_argument]) != Some(declared_name) {
    return Err(RailError::message(
      "portable native source argument disagrees with its declared crate root",
    ));
  }
  compiler_arguments[*source_argument] = portable_crate_root.clone();
  Ok(PortableNativeCacheKeyInputs {
    cfg,
    compiler_arguments,
    declared_inputs: vec![PortableNativeDeclaredInput {
      path: portable_crate_root,
      content_digest: &declared.content_digest,
      executable: declared.executable,
      symlink_target: &declared.symlink_target,
    }],
    dependency_artifacts,
  })
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

#[cfg(unix)]
const fn valid_native_output_mode(mode: u32) -> bool {
  mode & !0o666 == 0 && mode & 0o400 != 0
}

#[cfg(not(unix))]
const fn valid_native_output_mode(mode: u32) -> bool {
  matches!(mode, 0o444 | 0o644)
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
  if compiler_long_option_value(&observation.compiler_arguments, "--error-format") != Some("json") {
    return Some("compiler_diagnostic_format_not_graduated");
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

fn compiler_long_option_value<'a>(arguments: &'a [String], option: &str) -> Option<&'a str> {
  let inline = format!("{option}=");
  let mut selected = None;
  let mut index = 0usize;
  while index < arguments.len() {
    if arguments[index] == option {
      selected = arguments.get(index + 1).map(String::as_str);
      index = index.saturating_add(2);
    } else {
      if let Some(value) = arguments[index].strip_prefix(&inline) {
        selected = Some(value);
      }
      index = index.saturating_add(1);
    }
  }
  selected
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
    capture: NativeActionCapture,
    base_action_key: String,
    cache_bytes_read: u64,
  },
  /// A restore crossed its irreversible effect boundary and must not run rustc.
  OperationalFailure(RailError),
  /// Execute a cache-bypassed dependency producer with portable artifacts and
  /// current-root diagnostics.
  ExecutePortable(PortableCompilerExecution),
  /// Execute the original invocation unchanged.
  Execute,
}

#[derive(Clone)]
pub(crate) struct PortableCompilerExecution {
  stream_bindings: Vec<(Vec<u8>, Vec<u8>)>,
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
  prepare_original_child(command, diagnostic_wrapper);
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
  let source_root_spelling = &context.source_root_spelling;
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
  if context.discovery_only != (session.authority == NativeSessionAuthority::Discovery)
    || (context.discovery_only && context.remote.is_some())
  {
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      "native_cache_session_authority_mismatch",
      None,
      0,
      diagnostic_wrapper,
      trace,
    );
    return OuterCacheAction::Execute;
  }
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
  let recorder = match crate::compiler::observation::begin_invocation_in(
    observation_directory,
    source_root,
    &original_current_dir,
    rustc,
    compiler_arguments,
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
  if let Some(outputs) = recorder.native_output_paths()
    && let Err(error) = recover_restore_commit(&outputs, source_root, observation_directory)
  {
    return OuterCacheAction::OperationalFailure(error);
  }
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
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      reason,
      None,
      initial_input_bytes,
      diagnostic_wrapper,
      trace,
    );
    return portable_bypass_action(
      command,
      compiler_arguments,
      observation,
      source_root,
      source_root_spelling,
      &session.class.host_target,
      &original_current_dir,
    );
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
    return portable_bypass_action(
      command,
      compiler_arguments,
      observation,
      source_root,
      source_root_spelling,
      &session.class.host_target,
      &original_current_dir,
    );
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
    return portable_bypass_action(
      command,
      compiler_arguments,
      observation,
      source_root,
      source_root_spelling,
      &session.class.host_target,
      &original_current_dir,
    );
  }
  let action_capture_phase = trace.start(NativeCacheWrapperPhase::ActionCapture);
  let capture = NativeActionCapture::capture(observation, source_root);
  let capture_bytes = capture.as_ref().map_or(0, |capture| capture.bytes_hashed);
  let base_action = capture
    .as_ref()
    .ok()
    .and_then(|capture| base_action_key(&session.identity, &session.class, observation, capture).ok());
  let provisional_action = capture
    .as_ref()
    .ok()
    .and_then(|capture| action_key(&session.identity, &session.class, observation, capture).ok());
  trace.finish(
    action_capture_phase,
    NativeCacheWrapperWork {
      bytes_hashed: capture_bytes,
      ..NativeCacheWrapperWork::default()
    },
  );
  let (capture, base_action, provisional_action) = match (capture, base_action, provisional_action) {
    (Ok(capture), Some(base_action), Some(provisional_action)) => (capture, base_action, provisional_action),
    _ => {
      configure_cold(
        command,
        CompilerCacheWrapperStatus::Bypassed,
        "complete_action_capture_unavailable",
        None,
        initial_input_bytes.saturating_add(capture_bytes),
        diagnostic_wrapper,
        trace,
      );
      return portable_bypass_action(
        command,
        compiler_arguments,
        recorder.observation(),
        source_root,
        source_root_spelling,
        &session.class.host_target,
        &original_current_dir,
      );
    }
  };
  if capture_test_pause("after_initial_capture", observation).is_err() {
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      "capture_test_pause_failed",
      None,
      initial_input_bytes.saturating_add(capture.bytes_hashed),
      diagnostic_wrapper,
      trace,
    );
    return portable_bypass_action(
      command,
      compiler_arguments,
      recorder.observation(),
      source_root,
      source_root_spelling,
      &session.class.host_target,
      &original_current_dir,
    );
  }
  if context.retain_event_evidence {
    retain_pre_execution_unit_evidence(
      observation_directory,
      &provisional_action,
      observation,
      Some(&output_paths),
      source_root,
    );
  }
  let mut metrics = NativeCacheMetrics {
    bytes_hashed: initial_input_bytes.saturating_add(capture.bytes_hashed),
    ..NativeCacheMetrics::default()
  };
  if context.discovery_only {
    let metadata = configure_cold(
      command,
      CompilerCacheWrapperStatus::Miss,
      "empty_local_authority",
      Some(provisional_action.clone()),
      metrics.bytes_hashed,
      diagnostic_wrapper,
      trace,
    );
    let mut recorder = recorder;
    recorder.set_cache_wrapper(metadata);
    if prepare_observed_cold_child(
      command,
      rustc,
      compiler_arguments,
      source_root,
      source_root_spelling,
      &capture,
      diagnostic_wrapper,
    )
    .is_err()
    {
      configure_cold(
        command,
        CompilerCacheWrapperStatus::Bypassed,
        "portable_compiler_source_unavailable",
        Some(provisional_action),
        metrics.bytes_hashed,
        diagnostic_wrapper,
        trace,
      );
      return portable_bypass_action(
        command,
        compiler_arguments,
        recorder.observation(),
        source_root,
        source_root_spelling,
        &session.class.host_target,
        &original_current_dir,
      );
    }
    return OuterCacheAction::Store {
      recorder,
      capture,
      base_action_key: base_action,
      cache_bytes_read: 0,
    };
  }
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
        Some(provisional_action),
        metrics.bytes_hashed,
        diagnostic_wrapper,
        trace,
      );
      return portable_bypass_action(
        command,
        compiler_arguments,
        observation,
        source_root,
        source_root_spelling,
        &session.class.host_target,
        &original_current_dir,
      );
    }
  };
  let mut missing_selector_reason = "environment_selector_not_found";
  let environment_names = match cas.native_environment_selector(&base_action) {
    Ok(Some(names)) => Some(names),
    Ok(None) => match context.remote.as_ref() {
      Some(remote) => match crate::remote_cache::resolve_remote_selector(remote, &base_action) {
        Ok(crate::remote_cache::RemoteSelectorResolution::Miss) => {
          missing_selector_reason = "remote_environment_selector_not_found";
          None
        }
        Ok(crate::remote_cache::RemoteSelectorResolution::Unique(names)) => {
          if !names.iter().all(|name| remote.approves_environment_name(name)) {
            missing_selector_reason = "remote_environment_not_shareable";
            None
          } else {
            match cas.publish_native_environment_selector(&base_action, &names) {
              Ok(
                crate::hermetic::cas::NativeEnvironmentSelectorPublication::Created
                | crate::hermetic::cas::NativeEnvironmentSelectorPublication::Converged,
              ) => Some(names),
              Ok(crate::hermetic::cas::NativeEnvironmentSelectorPublication::Diverged) => {
                return OuterCacheAction::OperationalFailure(RailError::message(
                  "remote compiler environment selector conflicts with local authority",
                ));
              }
              Err(error) => {
                return OuterCacheAction::OperationalFailure(RailError::message(format!(
                  "remote compiler environment selector could not become local authority: {error}"
                )));
              }
            }
          }
        }
        Ok(crate::remote_cache::RemoteSelectorResolution::Conflict(_, _)) => {
          return OuterCacheAction::OperationalFailure(RailError::message(
            "remote cache integrity failure: one compiler base action has two environment selectors",
          ));
        }
        Err(error) if remote_fault_allows_cold_fallback(error.fault) => {
          missing_selector_reason = "remote_cache_unavailable";
          None
        }
        Err(error) => {
          return OuterCacheAction::OperationalFailure(RailError::message(format!(
            "remote cache selector resolution failed: {error}"
          )));
        }
      },
      None => None,
    },
    Err(error) => {
      return OuterCacheAction::OperationalFailure(RailError::message(format!(
        "native compiler environment selector is invalid: {error}"
      )));
    }
  };
  let environment_names = match environment_names {
    Some(names) => names,
    None => {
      let metadata = configure_cold(
        command,
        CompilerCacheWrapperStatus::Miss,
        missing_selector_reason,
        Some(provisional_action.clone()),
        metrics.bytes_hashed,
        diagnostic_wrapper,
        trace,
      );
      let mut recorder = recorder;
      recorder.set_cache_wrapper(metadata);
      if prepare_observed_cold_child(
        command,
        rustc,
        compiler_arguments,
        source_root,
        source_root_spelling,
        &capture,
        diagnostic_wrapper,
      )
      .is_err()
      {
        configure_cold(
          command,
          CompilerCacheWrapperStatus::Bypassed,
          "portable_compiler_source_unavailable",
          Some(provisional_action),
          metrics.bytes_hashed,
          diagnostic_wrapper,
          trace,
        );
        return portable_bypass_action(
          command,
          compiler_arguments,
          recorder.observation(),
          source_root,
          source_root_spelling,
          &session.class.host_target,
          &original_current_dir,
        );
      }
      return OuterCacheAction::Store {
        recorder,
        capture,
        base_action_key: base_action,
        cache_bytes_read: 0,
      };
    }
  };
  let mut capture = capture;
  match capture_approved_environment(
    source_root,
    source_root_spelling,
    &capture,
    &environment_names,
    Instant::now(),
  ) {
    Ok((environment, bytes_hashed)) => {
      capture.approved_environment = environment;
      metrics.bytes_hashed = metrics.bytes_hashed.saturating_add(bytes_hashed);
    }
    Err(_) => {
      configure_cold(
        command,
        CompilerCacheWrapperStatus::Bypassed,
        "compiler_environment_selector_unavailable",
        Some(provisional_action),
        metrics.bytes_hashed,
        diagnostic_wrapper,
        trace,
      );
      return portable_bypass_action(
        command,
        compiler_arguments,
        observation,
        source_root,
        source_root_spelling,
        &session.class.host_target,
        &original_current_dir,
      );
    }
  }
  let action = match action_key(&session.identity, &session.class, observation, &capture) {
    Ok(action) => action,
    Err(_) => {
      configure_cold(
        command,
        CompilerCacheWrapperStatus::Bypassed,
        "selected_action_identity_unavailable",
        Some(provisional_action),
        metrics.bytes_hashed,
        diagnostic_wrapper,
        trace,
      );
      return portable_bypass_action(
        command,
        compiler_arguments,
        observation,
        source_root,
        source_root_spelling,
        &session.class.host_target,
        &original_current_dir,
      );
    }
  };
  let remote_shareable = capture.remotely_shareable(context.remote.as_ref());
  let action_lookup_phase = trace.start(NativeCacheWrapperPhase::ActionLookup);
  let cached = cas.native_action_for_authority(
    &action,
    context
      .remote
      .as_ref()
      .filter(|_| remote_shareable)
      .map(crate::remote_cache::RemoteWrapperContext::authority),
  );
  let lookup_bytes = match &cached {
    Ok(crate::hermetic::cas::NativeActionLookup::Hit(cached)) => cached.bytes_read,
    Ok(crate::hermetic::cas::NativeActionLookup::Miss(miss)) => miss.bytes_read,
    Err(_) => 0,
  };
  trace.finish(
    action_lookup_phase,
    NativeCacheWrapperWork {
      cache_bytes_read: lookup_bytes,
      ..NativeCacheWrapperWork::default()
    },
  );
  let cached = match cached {
    Ok(cached) => cached,
    Err(_) => {
      configure_cold(
        command,
        CompilerCacheWrapperStatus::Bypassed,
        "local_cache_action_state_unavailable",
        Some(action),
        metrics.bytes_hashed,
        diagnostic_wrapper,
        trace,
      );
      return portable_bypass_action(
        command,
        compiler_arguments,
        observation,
        source_root,
        source_root_spelling,
        &session.class.host_target,
        &original_current_dir,
      );
    }
  };
  let mut miss_reason = match cached {
    crate::hermetic::cas::NativeActionLookup::Hit(cached)
      if cached.validation.session_identity == session.identity
        && cached.validation.class == session.class
        && cached.validation.action_key == action
        && capture.validates_witness(&cached.validation.witness, observation) =>
    {
      metrics.cache_bytes_read = metrics.cache_bytes_read.saturating_add(cached.bytes_read);
      match restore_and_publish(&cached, &capture, observation, &output_paths, &mut metrics, trace) {
        Ok(()) => return OuterCacheAction::Hit(0),
        Err(RestorePublishFailure::BeforeEffect(error)) => {
          drop(error);
          "verified_result_materialization_failed".to_string()
        }
        Err(RestorePublishFailure::AfterEffect(error) | RestorePublishFailure::Operational(error)) => {
          return OuterCacheAction::OperationalFailure(error);
        }
      }
    }
    crate::hermetic::cas::NativeActionLookup::Hit(cached) => {
      metrics.cache_bytes_read = metrics.cache_bytes_read.saturating_add(cached.bytes_read);
      "action_descriptor_incompatible".to_string()
    }
    crate::hermetic::cas::NativeActionLookup::Miss(miss) => {
      metrics.cache_bytes_read = metrics.cache_bytes_read.saturating_add(miss.bytes_read);
      miss.reason
    }
  };
  if let Some(remote) = &context.remote
    && remote_shareable
  {
    match attempt_remote_reuse(
      &cas,
      remote,
      &session,
      &capture,
      observation,
      &output_paths,
      source_root,
      &action,
      &mut metrics,
      trace,
    ) {
      RemoteReuseOutcome::Hit => return OuterCacheAction::Hit(0),
      RemoteReuseOutcome::Cold(reason) => miss_reason = reason.to_string(),
      RemoteReuseOutcome::OperationalFailure(error) => return OuterCacheAction::OperationalFailure(error),
    }
  } else if context.remote.is_some() && !remote_shareable {
    miss_reason = "remote_environment_not_shareable".to_string();
  }
  let metadata = configure_cold(
    command,
    CompilerCacheWrapperStatus::Miss,
    &miss_reason,
    Some(action.clone()),
    metrics.bytes_hashed,
    diagnostic_wrapper,
    trace,
  );
  let mut recorder = recorder;
  recorder.set_cache_wrapper(metadata);
  if prepare_observed_cold_child(
    command,
    rustc,
    compiler_arguments,
    source_root,
    source_root_spelling,
    &capture,
    diagnostic_wrapper,
  )
  .is_err()
  {
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      "portable_compiler_source_unavailable",
      Some(action),
      metrics.bytes_hashed,
      diagnostic_wrapper,
      trace,
    );
    return portable_bypass_action(
      command,
      compiler_arguments,
      recorder.observation(),
      source_root,
      source_root_spelling,
      &session.class.host_target,
      &original_current_dir,
    );
  }
  OuterCacheAction::Store {
    recorder,
    capture,
    base_action_key: base_action,
    cache_bytes_read: metrics.cache_bytes_read,
  }
}

enum RemoteReuseOutcome {
  Hit,
  Cold(&'static str),
  OperationalFailure(RailError),
}

enum RemoteAdmissionRecovery<'a> {
  Authoritative(Box<crate::hermetic::cas::NativeActionHit<'a>>),
  Cold,
  OperationalFailure(RailError),
}

fn recover_failed_remote_admission<'a>(
  cas: &'a LocalCas,
  authority: &RemoteAuthorityId,
  action_key: &str,
  admission_error: RailError,
) -> RemoteAdmissionRecovery<'a> {
  match cas.native_action_for_authority(action_key, Some(authority)) {
    Ok(crate::hermetic::cas::NativeActionLookup::Hit(cached)) => RemoteAdmissionRecovery::Authoritative(cached),
    Ok(crate::hermetic::cas::NativeActionLookup::Miss(miss)) if miss.reason == "action_not_found" => {
      RemoteAdmissionRecovery::Cold
    }
    Ok(crate::hermetic::cas::NativeActionLookup::Miss(miss)) => {
      RemoteAdmissionRecovery::OperationalFailure(RailError::message(format!(
        "remote pack admission failed and the local action state is terminal or incompatible ({}): {admission_error}",
        miss.reason
      )))
    }
    Err(resolution_error) => RemoteAdmissionRecovery::OperationalFailure(RailError::message(format!(
      "remote pack admission failed and its local action state could not be re-resolved: admission={admission_error}; resolution={resolution_error}"
    ))),
  }
}

fn remote_store_failure(
  error: crate::remote_cache::RemoteStoreError,
  operation: &'static str,
  cold_reason: &'static str,
) -> RemoteReuseOutcome {
  if remote_fault_allows_cold_fallback(error.fault) {
    RemoteReuseOutcome::Cold(cold_reason)
  } else {
    RemoteReuseOutcome::OperationalFailure(RailError::message(format!("remote cache {operation} failed: {error}")))
  }
}

const fn remote_fault_allows_cold_fallback(fault: crate::remote_cache::RemoteStoreFault) -> bool {
  matches!(
    fault,
    crate::remote_cache::RemoteStoreFault::Unavailable
      | crate::remote_cache::RemoteStoreFault::Authentication
      | crate::remote_cache::RemoteStoreFault::Configuration
  )
}

fn remote_integrity_failure(operation: &'static str, error: impl std::fmt::Display) -> RemoteReuseOutcome {
  RemoteReuseOutcome::OperationalFailure(RailError::message(format!(
    "remote cache {operation} failed integrity verification: {error}"
  )))
}

fn remote_conflict_failure(
  cas: &LocalCas,
  action_key: &str,
  first_result: &str,
  second_result: &str,
) -> RemoteReuseOutcome {
  if let Err(error) = cas.record_remote_conflict(action_key, first_result, second_result) {
    return remote_integrity_failure("action-conflict recording", error);
  }
  RemoteReuseOutcome::OperationalFailure(RailError::message(
    "remote cache integrity failure: one compiler action has two distinct results",
  ))
}

#[allow(clippy::too_many_arguments)]
fn attempt_remote_reuse(
  cas: &LocalCas,
  remote: &crate::remote_cache::RemoteWrapperContext,
  session: &NativeCompilerSession,
  capture: &NativeActionCapture,
  observation: &RawCompilerInvocation,
  output_paths: &NativeOutputPaths,
  source_root: &Path,
  action_key: &str,
  metrics: &mut NativeCacheMetrics,
  trace: &mut NativeCacheWrapperTrace,
) -> RemoteReuseOutcome {
  let (result_key, mut stream, declared_length) = match crate::remote_cache::fetch_remote(remote, action_key) {
    Ok(crate::remote_cache::RemoteFetch::Miss) => {
      return RemoteReuseOutcome::Cold("remote_action_not_found");
    }
    Ok(crate::remote_cache::RemoteFetch::Expired) => {
      return RemoteReuseOutcome::Cold("remote_result_expired");
    }
    Ok(crate::remote_cache::RemoteFetch::Conflict(first, second)) => {
      return remote_conflict_failure(cas, action_key, first.result_key(), second.result_key());
    }
    Ok(crate::remote_cache::RemoteFetch::Unique {
      result_key,
      stream,
      length,
    }) => (result_key, stream, length),
    Err(error) => return remote_store_failure(error, "action fetch", "remote_cache_unavailable"),
  };

  let attached_local = match cas.attach_remote_origin(action_key, &result_key, remote.authority()) {
    Ok(attached) => attached,
    Err(error) => return remote_integrity_failure("local-origin attachment", error),
  };
  if attached_local {
    match cas.native_action_for_authority(action_key, Some(remote.authority())) {
      Ok(crate::hermetic::cas::NativeActionLookup::Hit(cached))
        if cached.validation.session_identity == session.identity
          && cached.validation.class == session.class
          && cached.validation.action_key == action_key
          && cached.validation.result_key == result_key
          && capture.validates_witness(&cached.validation.witness, observation) =>
      {
        metrics.cache_bytes_read = metrics.cache_bytes_read.saturating_add(cached.bytes_read);
        drop(stream);
        return match restore_and_publish(&cached, capture, observation, output_paths, metrics, trace) {
          Ok(()) => RemoteReuseOutcome::Hit,
          Err(RestorePublishFailure::BeforeEffect(_)) => {
            RemoteReuseOutcome::Cold("remote_local_result_materialization_failed")
          }
          Err(RestorePublishFailure::AfterEffect(error) | RestorePublishFailure::Operational(error)) => {
            RemoteReuseOutcome::OperationalFailure(error)
          }
        };
      }
      Ok(crate::hermetic::cas::NativeActionLookup::Hit(cached)) => {
        metrics.cache_bytes_read = metrics.cache_bytes_read.saturating_add(cached.bytes_read);
        drop(stream);
        return RemoteReuseOutcome::OperationalFailure(RailError::message(
          "remote cache integrity failure: the accepted local result is incompatible with the live action",
        ));
      }
      Ok(crate::hermetic::cas::NativeActionLookup::Miss(miss)) => {
        metrics.cache_bytes_read = metrics.cache_bytes_read.saturating_add(miss.bytes_read);
      }
      Err(error) => return remote_integrity_failure("accepted local-result verification", error),
    }
  }

  let staging = match cas.native_result_staging() {
    Ok(staging) => staging,
    Err(_) => return RemoteReuseOutcome::Cold("remote_pack_staging_unavailable"),
  };
  let (decoded, association) =
    match pack::decode_for_action(&mut stream, action_key, Some(declared_length), Some(staging)) {
      Ok(decoded) => decoded,
      Err(_) if stream.transport_failed() => return RemoteReuseOutcome::Cold("remote_pack_unavailable"),
      Err(error) => return remote_integrity_failure("pack decoding", error),
    };
  if association.result_key() != result_key {
    return remote_integrity_failure("pack action binding", "remote action and pack name different results");
  }
  let (prepared, pack_bytes) = match prepare_authenticated_native_pack(
    decoded,
    remote.authority().clone(),
    session,
    capture,
    observation,
    output_paths,
    source_root,
  ) {
    Ok(prepared) => prepared,
    Err(error) => return remote_integrity_failure("pack action binding", error),
  };
  metrics.cache_bytes_read = metrics.cache_bytes_read.saturating_add(pack_bytes);
  let (_, stored) = match cas.store_native(prepared) {
    Ok(stored) => stored,
    Err(error) => match recover_failed_remote_admission(cas, remote.authority(), action_key, error) {
      RemoteAdmissionRecovery::Authoritative(cached) => {
        metrics.cache_bytes_read = metrics.cache_bytes_read.saturating_add(cached.bytes_read);
        if cached.validation.session_identity != session.identity
          || cached.validation.class != session.class
          || cached.validation.action_key != action_key
          || cached.validation.result_key != result_key
          || !capture.validates_witness(&cached.validation.witness, observation)
        {
          return RemoteReuseOutcome::OperationalFailure(RailError::message(
            "remote cache integrity failure: post-admission local authority is incompatible with the fetched result",
          ));
        }
        return match restore_and_publish(&cached, capture, observation, output_paths, metrics, trace) {
          Ok(()) => RemoteReuseOutcome::Hit,
          Err(RestorePublishFailure::BeforeEffect(_)) => {
            RemoteReuseOutcome::Cold("remote_result_materialization_failed")
          }
          Err(RestorePublishFailure::AfterEffect(error) | RestorePublishFailure::Operational(error)) => {
            RemoteReuseOutcome::OperationalFailure(error)
          }
        };
      }
      RemoteAdmissionRecovery::Cold => return RemoteReuseOutcome::Cold("remote_pack_admission_failed"),
      RemoteAdmissionRecovery::OperationalFailure(error) => return RemoteReuseOutcome::OperationalFailure(error),
    },
  };
  metrics.cache_bytes_written = metrics.cache_bytes_written.saturating_add(stored.bytes_written);
  let cached = match cas.native_action_for_authority(action_key, Some(remote.authority())) {
    Ok(crate::hermetic::cas::NativeActionLookup::Hit(cached)) => cached,
    Ok(crate::hermetic::cas::NativeActionLookup::Miss(miss)) => {
      metrics.cache_bytes_read = metrics.cache_bytes_read.saturating_add(miss.bytes_read);
      return RemoteReuseOutcome::OperationalFailure(RailError::message(
        "remote cache integrity failure: imported pack did not become locally authoritative",
      ));
    }
    Err(error) => return remote_integrity_failure("imported-result verification", error),
  };
  metrics.cache_bytes_read = metrics.cache_bytes_read.saturating_add(cached.bytes_read);
  if cached.validation.session_identity != session.identity
    || cached.validation.class != session.class
    || cached.validation.action_key != action_key
    || cached.validation.result_key != result_key
    || !capture.validates_witness(&cached.validation.witness, observation)
  {
    return RemoteReuseOutcome::OperationalFailure(RailError::message(
      "remote cache integrity failure: imported result is incompatible with the live action",
    ));
  }
  match restore_and_publish(&cached, capture, observation, output_paths, metrics, trace) {
    Ok(()) => RemoteReuseOutcome::Hit,
    Err(RestorePublishFailure::BeforeEffect(_)) => RemoteReuseOutcome::Cold("remote_result_materialization_failed"),
    Err(RestorePublishFailure::AfterEffect(error) | RestorePublishFailure::Operational(error)) => {
      RemoteReuseOutcome::OperationalFailure(error)
    }
  }
}

fn prepare_original_child(command: &mut Command, diagnostic_wrapper: bool) {
  if !diagnostic_wrapper {
    suppress_nested_observation(command);
  }
}

fn suppress_nested_observation(command: &mut Command) {
  remove_private_environment(command);
}

fn append_compiler_remap(command: &mut Command, from: &OsStr, to: &str) {
  let mut remap = from.to_os_string();
  remap.push("=");
  remap.push(to);
  command.arg("--remap-path-prefix").arg(remap);
}

fn portable_dependency_producer(observation: &RawCompilerInvocation, host_target: &str) -> bool {
  observation.mode == CompilerMode::Rustc
    && !observation.test_mode
    && observation
      .target_argument
      .as_deref()
      .is_none_or(|target| target == host_target)
    && compiler_long_option_value(&observation.compiler_arguments, "--error-format") == Some("json")
    && (observation.crate_types.len() == 1
      && observation
        .crate_types
        .iter()
        .next()
        .is_some_and(|crate_type| matches!(crate_type.as_str(), "lib" | "rlib" | "proc-macro")))
}

fn prepare_portable_bypass_child(
  command: &mut Command,
  compiler_arguments: &[OsString],
  observation: &RawCompilerInvocation,
  source_root: &Path,
  source_root_spelling: &Path,
  host_target: &str,
  current_dir: &Path,
) -> RailResult<PortableCompilerExecution> {
  if !portable_dependency_producer(observation, host_target)
    || compiler_arguments.iter().any(|argument| {
      argument.to_str().is_some_and(|argument| {
        argument == "--remap-path-prefix"
          || argument.starts_with("--remap-path-prefix=")
          || argument == "--remap-path-scope"
          || argument.starts_with("--remap-path-scope=")
      })
    })
  {
    return Err(RailError::message(
      "compiler invocation does not admit portable dependency output",
    ));
  }
  let source_arguments = compiler_arguments
    .iter()
    .enumerate()
    .filter(|(_, argument)| {
      argument
        .to_str()
        .is_some_and(|argument| !argument.starts_with('-') && argument.ends_with(".rs"))
    })
    .collect::<Vec<_>>();
  let [(source_argument, source_spelling)] = source_arguments.as_slice() else {
    return Err(RailError::message(
      "portable compiler execution requires one positional Rust source argument",
    ));
  };
  let [declared_source] = observation.declared_inputs.as_slice() else {
    return Err(RailError::message(
      "portable compiler execution requires one declared Rust source",
    ));
  };
  if declared_source.symlink_target.is_some() {
    return Err(RailError::message("portable compiler source must not be a symlink"));
  }
  let source_path = Path::new(source_spelling);
  let source_path = if source_path.is_absolute() {
    source_path.to_path_buf()
  } else {
    current_dir.join(source_path)
  };
  let source_path = crate::utils::canonicalize_existing(&source_path)?;
  let declared_path = crate::utils::canonicalize_existing(&declared_source.path.resolve(source_root))?;
  if source_path != declared_path {
    return Err(RailError::message(
      "compiler source argument does not match its declared source",
    ));
  }

  source_root_spellings(source_root_spelling)?;
  let mut package_roots = Vec::new();
  let (portable_source, package_spelling) = match &declared_source.path {
    ObservationPath::Repository(relative) => {
      let relative = native_relative_path(Path::new(relative))?;
      let portable = if relative.is_empty() {
        PORTABLE_SOURCE_ROOT.to_string()
      } else {
        format!("{PORTABLE_SOURCE_ROOT}/{relative}")
      };
      (portable, None)
    }
    ObservationPath::Host(_) => {
      let spelling = std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| RailError::message("external compiler source has no Cargo package root"))?;
      if !spelling.is_absolute() {
        return Err(RailError::message("external Cargo package root is not absolute"));
      }
      let metadata = fs::symlink_metadata(&spelling)?;
      if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::message(
          "external Cargo package root is not a real directory",
        ));
      }
      let root = crate::utils::canonicalize_existing(&spelling)?;
      let relative = native_relative_path(
        source_path
          .strip_prefix(&root)
          .map_err(|_| RailError::message("external compiler source is outside its Cargo package root"))?,
      )?;
      source_root_spellings(&spelling)?;
      package_roots.extend([spelling.clone(), root]);
      let portable = if relative.is_empty() {
        PORTABLE_PACKAGE_ROOT.to_string()
      } else {
        format!("{PORTABLE_PACKAGE_ROOT}/{relative}")
      };
      (portable, Some(spelling))
    }
  };

  append_compiler_remap(
    command,
    compiler_arguments[*source_argument].as_os_str(),
    &portable_source,
  );
  let mut workspace_roots = vec![source_root_spelling.to_path_buf(), source_root.to_path_buf()];
  workspace_roots.sort_unstable();
  workspace_roots.dedup();
  for workspace_root in workspace_roots {
    append_compiler_remap(command, workspace_root.as_os_str(), PORTABLE_SOURCE_ROOT);
  }
  package_roots.sort_unstable();
  package_roots.dedup();
  for package_root in package_roots {
    append_compiler_remap(command, package_root.as_os_str(), PORTABLE_PACKAGE_ROOT);
  }
  command.arg("--remap-path-scope=all");

  let mut stream_bindings = vec![(
    PORTABLE_SOURCE_ROOT.as_bytes().to_vec(),
    json_string_contents(&source_root_display_bytes(source_root)),
  )];
  if let Some(package_spelling) = package_spelling {
    stream_bindings.push((
      PORTABLE_PACKAGE_ROOT.as_bytes().to_vec(),
      json_string_contents(&source_root_display_bytes(&package_spelling)),
    ));
  }
  Ok(PortableCompilerExecution { stream_bindings })
}

fn portable_bypass_action(
  command: &mut Command,
  compiler_arguments: &[OsString],
  observation: &RawCompilerInvocation,
  source_root: &Path,
  source_root_spelling: &Path,
  host_target: &str,
  current_dir: &Path,
) -> OuterCacheAction {
  match prepare_portable_bypass_child(
    command,
    compiler_arguments,
    observation,
    source_root,
    source_root_spelling,
    host_target,
    current_dir,
  ) {
    Ok(execution) => OuterCacheAction::ExecutePortable(execution),
    Err(_) => OuterCacheAction::Execute,
  }
}

fn prepare_observed_cold_child(
  command: &mut Command,
  rustc: &OsStr,
  compiler_arguments: &[OsString],
  source_root: &Path,
  source_root_spelling: &Path,
  capture: &NativeActionCapture,
  diagnostic_wrapper: bool,
) -> RailResult<()> {
  source_root_spellings(source_root_spelling)?;
  let source_arguments = compiler_arguments
    .iter()
    .enumerate()
    .filter(|(_, argument)| {
      argument
        .to_str()
        .is_some_and(|argument| !argument.starts_with('-') && argument.ends_with(".rs"))
    })
    .map(|(index, _)| index)
    .collect::<Vec<_>>();
  let [source_argument] = source_arguments.as_slice() else {
    return Err(RailError::message(
      "portable native action requires one positional Rust source argument",
    ));
  };
  let portable_crate_root = capture.portable_crate_root()?;
  let mut workspace_roots = vec![source_root_spelling, source_root];
  workspace_roots.sort_unstable();
  workspace_roots.dedup();
  let package_roots = capture.package_binding.as_ref().map(|package| {
    let mut roots = vec![package.spelling.as_path(), package.root.as_path()];
    roots.sort_unstable();
    roots.dedup();
    roots
  });
  *command = Command::new(rustc);
  command.args(compiler_arguments);
  if diagnostic_wrapper {
    command.arg("--warn=unused-crate-dependencies");
  }
  append_compiler_remap(
    command,
    compiler_arguments[*source_argument].as_os_str(),
    &portable_crate_root,
  );
  for workspace_root in workspace_roots {
    append_compiler_remap(command, workspace_root.as_os_str(), PORTABLE_SOURCE_ROOT);
  }
  if let Some(package_roots) = package_roots {
    for package_root in package_roots {
      append_compiler_remap(command, package_root.as_os_str(), PORTABLE_PACKAGE_ROOT);
    }
  }
  command.arg("--remap-path-scope=all");
  suppress_nested_observation(command);
  Ok(())
}

fn configure_cold(
  command: &mut Command,
  status: CompilerCacheWrapperStatus,
  reason: &str,
  action_key: Option<String>,
  bytes_hashed: u64,
  propagate_metadata: bool,
  trace: &NativeCacheWrapperTrace,
) -> CompilerCacheWrapperMetadata {
  let metadata = CompilerCacheWrapperMetadata::native(status, reason, action_key.clone(), None, bytes_hashed, 0);
  if propagate_metadata && let Ok(encoded) = serde_json::to_string(&metadata) {
    command.env(DISPOSITION_ENV, encoded);
  }
  if status != CompilerCacheWrapperStatus::Miss {
    write_cache_event(
      status,
      reason,
      action_key.as_deref(),
      None,
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

fn validate_restore_environment_authority(
  cached: &crate::hermetic::cas::NativeActionHit<'_>,
  capture: &NativeActionCapture,
  observation: &RawCompilerInvocation,
) -> Result<String, RestorePublishFailure> {
  let validation = &cached.validation;
  let base_action = base_action_key(&validation.session_identity, &validation.class, observation, capture)
    .map_err(RestorePublishFailure::Operational)?;
  cached
    .validate_environment_selector(
      &base_action,
      capture
        .approved_environment
        .entries
        .iter()
        .map(|entry| entry.name.as_str()),
    )
    .map_err(RestorePublishFailure::Operational)?;
  Ok(base_action)
}

fn restore_and_publish(
  cached: &crate::hermetic::cas::NativeActionHit<'_>,
  initial_capture: &NativeActionCapture,
  current_observation: &RawCompilerInvocation,
  output_paths: &NativeOutputPaths,
  metrics: &mut NativeCacheMetrics,
  trace: &mut NativeCacheWrapperTrace,
) -> Result<(), RestorePublishFailure> {
  let base_action_key = validate_restore_environment_authority(cached, initial_capture, current_observation)?;
  let before = RestorePublishFailure::BeforeEffect;
  let validation = &cached.validation;
  let context =
    active_context().ok_or_else(|| before(RailError::message("native compiler cache context disappeared")))?;
  let source_root = &context.source_root;
  let observation_directory = &context.observation_directory;
  validate_current_output_binding(validation, output_paths, source_root).map_err(before)?;
  let mut transaction = begin_restore_transaction(
    output_paths,
    source_root,
    observation_directory,
    validation.action_key(),
  )
  .map_err(before)?;
  if let Err(error) = restore_commit_test_fault("after_registration", 0, current_observation) {
    return Err(fail_restore_transaction(&mut transaction, error, 0));
  }
  let prepared = match prepare_registered_restore(
    cached,
    &transaction,
    initial_capture,
    current_observation,
    output_paths,
    metrics,
    trace,
    source_root,
    observation_directory,
  ) {
    Ok(prepared) => prepared,
    Err(error) => return Err(fail_restore_transaction(&mut transaction, error, 0)),
  };
  if let Err(error) = restore_commit_test_fault("before_marker_publish", 0, current_observation) {
    drop(prepared);
    return Err(fail_restore_transaction(&mut transaction, error, 0));
  }
  if let Err(error) = transaction.authorize(&prepared, observation_directory, current_observation) {
    drop(prepared);
    return Err(fail_restore_transaction(&mut transaction, error, 0));
  }

  let PreparedNativeRestore {
    outputs,
    stdout,
    stderr,
    observation,
    bytes_restored,
    publication_bytes_hashed,
  } = prepared;
  let publication_phase = trace.start(NativeCacheWrapperPhase::CargoOutputPublication);
  let mut visible_effects = 0usize;
  let commit_result = (|| -> RailResult<()> {
    restore_commit_test_fault("after_marker", 0, current_observation)?;
    let mut published_outputs = Vec::with_capacity(outputs.len());
    for output in outputs {
      let member = transaction.output_member(&output.destination)?;
      published_outputs.push(publish_prepared_restore_output(output, member)?);
      visible_effects = visible_effects.saturating_add(1);
      restore_commit_test_fault("after_output", visible_effects, current_observation)?;
    }
    visible_effects = visible_effects.saturating_add(1);
    crate::compiler::observation::publish_prepared_raw_durable(observation)?;
    restore_commit_test_fault("after_observation", visible_effects, current_observation)?;
    visible_effects = visible_effects.saturating_add(1);
    std::io::stdout().write_all(&stdout)?;
    restore_commit_test_fault("after_stdout", visible_effects, current_observation)?;
    visible_effects = visible_effects.saturating_add(1);
    std::io::stderr().write_all(&stderr)?;
    restore_commit_test_fault("after_stderr", visible_effects, current_observation)?;
    for output in &published_outputs {
      output.sync()?;
    }
    for output in &published_outputs {
      output.revalidate()?;
    }
    sync_native_directory(&transaction.paths.output_parent)?;
    restore_commit_test_fault("before_marker_removal", visible_effects, current_observation)?;
    transaction.complete()?;
    restore_commit_test_fault("after_marker_removal", visible_effects, current_observation)?;
    transaction.cleanup_private()?;
    restore_commit_test_fault("after_transaction_cleanup", visible_effects, current_observation)?;
    Ok(())
  })();
  if let Err(error) = commit_result {
    return Err(fail_restore_transaction(&mut transaction, error, visible_effects));
  }
  trace.finish(
    publication_phase,
    NativeCacheWrapperWork {
      bytes_hashed: publication_bytes_hashed,
      ..NativeCacheWrapperWork::default()
    },
  );
  write_cache_event(
    CompilerCacheWrapperStatus::Hit,
    "verified_local_result",
    Some(&validation.action_key),
    Some(&validation.result_key),
    initial_capture
      .remotely_shareable(context.remote.as_ref())
      .then_some(base_action_key.as_str()),
    NativeCacheMetrics {
      bytes_restored,
      ..*metrics
    },
    trace,
  );
  Ok(())
}

#[allow(clippy::too_many_arguments)]
fn prepare_registered_restore(
  cached: &crate::hermetic::cas::NativeActionHit<'_>,
  transaction: &NativeRestoreTransaction,
  initial_capture: &NativeActionCapture,
  current_observation: &RawCompilerInvocation,
  output_paths: &NativeOutputPaths,
  metrics: &mut NativeCacheMetrics,
  trace: &mut NativeCacheWrapperTrace,
  source_root: &Path,
  observation_directory: &Path,
) -> RailResult<PreparedNativeRestore> {
  let validation = &cached.validation;
  let restore_phase = trace.start(NativeCacheWrapperPhase::ResultRestoreMaterialization);
  let cache_bytes_before = metrics.cache_bytes_read;
  let restored = transaction.paths.transaction_directory.join(RESTORE_VERIFIED_DIRECTORY);
  let staging = transaction
    .paths
    .transaction_directory
    .join(RESTORE_MATERIALIZING_DIRECTORY);
  let hit = match cached.restore_registered(&restored, &staging) {
    NativeCacheLookup::Hit(hit) => {
      metrics.cache_bytes_read = metrics.cache_bytes_read.saturating_add(hit.bytes_read);
      hit
    }
    NativeCacheLookup::Miss(miss) => {
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
  let stream_bindings = source_root_stream_bindings(source_root, initial_capture);
  let stdout = rebind_portable_source_roots(&stdout, &stream_bindings)?;
  let stderr = rebind_portable_source_roots(&stderr, &stream_bindings)?;
  trace.finish(
    restore_phase,
    NativeCacheWrapperWork {
      bytes_hashed: hit.bytes_restored,
      cache_bytes_read: metrics.cache_bytes_read.saturating_sub(cache_bytes_before),
      bytes_restored: hit.bytes_restored,
      ..NativeCacheWrapperWork::default()
    },
  );
  let bindings = native_output_bindings(output_paths);
  let mut prepared_outputs = Vec::with_capacity(bindings.len());
  let mut publication_bytes_hashed = 0u64;
  for ((role, slot, destination), expected) in bindings.iter().zip(&validation.outputs) {
    let prepared = if *role == "dep_info" {
      let source = restored.join(slot);
      let bytes = read_bounded(&source, usize::try_from(expected.bytes).unwrap_or(usize::MAX))?;
      if bytes.len() as u64 != expected.bytes || digest(&bytes) != expected.content_digest {
        return Err(RailError::message(
          "native compiler dep-info binding changed after restore",
        ));
      }
      let materialized =
        translate_dep_info_output_bindings(&bytes, validation, output_paths, source_root, initial_capture)?;
      prepare_restore_bytes(
        &materialized,
        &source,
        expected,
        destination,
        source_root,
        &transaction.paths.transaction_directory,
      )?
    } else {
      prepare_restore_output(&restored.join(slot), destination, expected, source_root)?
    };
    publication_bytes_hashed = publication_bytes_hashed.saturating_add(prepared.bytes_hashed);
    prepared_outputs.push(prepared);
  }
  capture_test_pause("before_restore_revalidation", current_observation)?;
  let final_capture_phase = trace.start(NativeCacheWrapperPhase::FinalActionRevalidation);
  let final_capture = NativeActionCapture::capture_with_approved_environment(
    current_observation,
    source_root,
    initial_capture.approved_environment.clone(),
  );
  let final_capture_bytes = final_capture.as_ref().map_or(0, |capture| capture.bytes_hashed);
  trace.finish(
    final_capture_phase,
    NativeCacheWrapperWork {
      bytes_hashed: final_capture_bytes,
      ..NativeCacheWrapperWork::default()
    },
  );
  let final_capture = final_capture?;
  metrics.bytes_hashed = metrics.bytes_hashed.saturating_add(final_capture.bytes_hashed);
  if !final_capture.unchanged_from(initial_capture)
    || !final_capture.validates_witness(&validation.witness, current_observation)
  {
    return Err(RailError::message(
      "native compiler inputs changed before the restore commit",
    ));
  }
  let emitted_outputs = prepared_outputs
    .iter()
    .map(|output| output.observation.clone())
    .collect::<Vec<_>>();
  let mut raw = validation.observation.clone();
  raw.compiler_arguments = current_observation.compiler_arguments.clone();
  raw.declared_inputs = current_observation.declared_inputs.clone();
  raw.dependency_artifacts = current_observation.dependency_artifacts.clone();
  raw.observed_reads = current_observed_reads(initial_capture, &validation.witness, current_observation, source_root)?;
  raw.emitted_outputs = emitted_outputs;
  raw.emitted_outputs.sort();
  raw.success = true;
  raw.cache_wrapper = Some(CompilerCacheWrapperMetadata::native(
    CompilerCacheWrapperStatus::Hit,
    "verified_local_result",
    Some(validation.action_key.clone()),
    Some(validation.result_key.clone()),
    metrics.bytes_hashed,
    hit.bytes_restored,
  ));
  let observation = crate::compiler::observation::prepare_raw_publication(observation_directory, &raw)?;
  Ok(PreparedNativeRestore {
    outputs: prepared_outputs,
    stdout,
    stderr,
    observation,
    bytes_restored: hit.bytes_restored,
    publication_bytes_hashed,
  })
}

fn fail_restore_transaction(
  transaction: &mut NativeRestoreTransaction,
  error: RailError,
  visible_effects: usize,
) -> RestorePublishFailure {
  let cleanup = transaction.rollback();
  let cleanup_failed = cleanup.is_err();
  let error = match cleanup {
    Ok(()) => error,
    Err(cleanup) => RailError::message(format!("{error}; restore cleanup failed: {cleanup}")),
  };
  if visible_effects > 0 {
    RestorePublishFailure::AfterEffect(error)
  } else if cleanup_failed {
    RestorePublishFailure::Operational(error)
  } else {
    RestorePublishFailure::BeforeEffect(error)
  }
}

fn prepare_restore_output(
  source: &Path,
  destination: &Path,
  expected: &NativeCompilerOutput,
  source_root: &Path,
) -> RailResult<PreparedRestoreOutput> {
  let metadata = fs::symlink_metadata(source)?;
  if !metadata.is_file()
    || crate::utils::is_symlink_or_reparse(&metadata)
    || !single_link(&metadata)
    || metadata.len() != expected.bytes
    || native_output_mode(&metadata) != expected.mode
  {
    return Err(RailError::message(
      "verified native compiler output changed before commit preparation",
    ));
  }
  let opened = File::open(source)?;
  if !crate::utils::private_file_matches_path(&opened, source, expected.bytes)? {
    return Err(RailError::message(
      "verified native compiler output path changed before commit preparation",
    ));
  }
  let observation = FileObservation {
    path: ObservationPath::capture(destination, source_root, source_root),
    content_digest: expected.content_digest.clone(),
    executable: source_mode_executable(expected.mode),
    symlink_target: None,
  };
  let source_identity = native_restore_file_identity(&opened)?;
  Ok(PreparedRestoreOutput {
    source: source.to_path_buf(),
    opened,
    source_identity,
    destination: destination.to_path_buf(),
    observation,
    bytes: expected.bytes,
    mode: expected.mode,
    bytes_hashed: 0,
  })
}

fn prepare_restore_bytes(
  bytes: &[u8],
  mode_source: &Path,
  expected: &NativeCompilerOutput,
  destination: &Path,
  source_root: &Path,
  transaction_directory: &Path,
) -> RailResult<PreparedRestoreOutput> {
  let metadata = fs::symlink_metadata(mode_source)?;
  if !metadata.is_file()
    || crate::utils::is_symlink_or_reparse(&metadata)
    || !single_link(&metadata)
    || native_output_mode(&metadata) != expected.mode
  {
    return Err(RailError::message(
      "native compiler output mode source is not a private regular file",
    ));
  }
  let opened = File::open(mode_source)?;
  if !crate::utils::private_file_matches_path(&opened, mode_source, metadata.len())? {
    return Err(RailError::message(
      "native compiler output mode source changed before commit preparation",
    ));
  }
  let prepared_directory = transaction_directory.join(RESTORE_PREPARED_DIRECTORY);
  fs::create_dir(&prepared_directory)?;
  let source = prepared_directory.join(RESTORE_PREPARED_DEP_INFO_FILE);
  let mut opened = OpenOptions::new()
    .read(true)
    .write(true)
    .create_new(true)
    .open(&source)?;
  opened.write_all(bytes)?;
  set_native_output_mode(&source, expected.mode)?;
  if !crate::utils::private_file_matches_path(&opened, &source, bytes.len() as u64)? {
    return Err(RailError::message(
      "prepared native compiler dep-info changed before registration",
    ));
  }
  let content_digest = digest(bytes);
  let source_identity = native_restore_file_identity(&opened)?;
  Ok(PreparedRestoreOutput {
    source,
    opened,
    source_identity,
    destination: destination.to_path_buf(),
    observation: FileObservation {
      path: ObservationPath::capture(destination, source_root, source_root),
      content_digest,
      executable: source_mode_executable(expected.mode),
      symlink_target: None,
    },
    bytes: bytes.len() as u64,
    mode: expected.mode,
    bytes_hashed: bytes.len() as u64,
  })
}

fn publish_prepared_restore_output(
  output: PreparedRestoreOutput,
  member: &NativeRestoreMember,
) -> RailResult<PublishedRestoreOutput> {
  let NativeRestoreMember::Output {
    source,
    destination,
    source_identity,
    previous_identity,
    content_digest,
  } = member
  else {
    return Err(RailError::message(
      "native restore output resolved to a non-output transaction member",
    ));
  };
  if output.source.to_str() != Some(source)
    || output.destination.to_str() != Some(destination)
    || &output.source_identity != source_identity
    || output.observation.content_digest != *content_digest
    || !crate::utils::private_file_matches_path(&output.opened, &output.source, output.bytes)?
    || native_restore_file_identity(&output.opened)? != *source_identity
  {
    return Err(RailError::message(
      "prepared native compiler output changed before atomic publication",
    ));
  }
  validate_restore_destination_state(&output.destination, previous_identity.as_ref())?;
  if !crate::utils::private_file_matches_path(&output.opened, &output.source, output.bytes)?
    || native_restore_file_identity(&output.opened)? != *source_identity
  {
    return Err(RailError::message(
      "prepared native compiler output changed at the publication boundary",
    ));
  }
  let destination = output.destination;
  let identity = output.source_identity;
  let bytes = output.bytes;
  let mode = output.mode;
  #[cfg(windows)]
  let opened = {
    rename_restore_write_through(
      &output.source,
      &output.opened,
      source_identity,
      &destination,
      previous_identity.as_ref(),
    )?;
    output.opened
  };
  #[cfg(not(windows))]
  let opened = {
    let temporary_path = tempfile::TempPath::try_from_path(output.source)?;
    let temporary = tempfile::NamedTempFile::from_parts(output.opened, temporary_path);
    crate::utils::persist_regenerable_file_atomic(temporary, &destination)?
  };
  Ok(PublishedRestoreOutput {
    opened,
    destination,
    identity,
    bytes,
    mode,
  })
}

#[cfg(windows)]
fn rename_restore_write_through(
  source: &Path,
  opened: &File,
  source_identity: &NativeRestoreFileIdentity,
  destination: &Path,
  destination_identity: Option<&NativeRestoreFileIdentity>,
) -> RailResult<()> {
  const MAX_ATTEMPTS: usize = 50;
  for attempt in 0..MAX_ATTEMPTS {
    if native_restore_file_identity(opened)? != *source_identity
      || !crate::utils::private_file_matches_path(opened, source, source_identity.bytes)?
    {
      return Err(RailError::message(format!(
        "native restore source '{}' changed at the rename boundary",
        source.display()
      )));
    }
    validate_restore_destination_state(destination, destination_identity)?;
    match crate::windows_fs::rename_write_through(source, destination, destination_identity.is_some()) {
      Ok(()) => return Ok(()),
      Err(error) if matches!(error.raw_os_error(), Some(5 | 32 | 33)) && attempt.saturating_add(1) < MAX_ATTEMPTS => {
        std::thread::sleep(Duration::from_millis(1));
      }
      Err(error) => {
        return Err(RailError::message(format!(
          "failed to publish native restore path '{}' to '{}': {error}",
          source.display(),
          destination.display()
        )));
      }
    }
  }
  Err(RailError::message("native restore rename retry bound was exhausted"))
}

fn restore_commit_paths(outputs: &NativeOutputPaths, source_root: &Path) -> RailResult<NativeRestorePaths> {
  let output_parent = validated_output_parent(outputs, source_root)?;
  let bindings = native_output_bindings(outputs);
  let mut expected_outputs = bindings
    .iter()
    .map(|(_, _, destination)| (*destination).to_path_buf())
    .collect::<Vec<_>>();
  expected_outputs.sort_unstable();
  let encoded = expected_outputs
    .iter()
    .map(|path| {
      path
        .to_str()
        .map(str::as_bytes)
        .ok_or_else(|| RailError::message("native restore destination is not valid UTF-8"))
    })
    .collect::<RailResult<Vec<_>>>()?;
  let mut hasher = Sha256::new();
  hasher.update(b"cargo-rail-native-restore-destinations-v1\0");
  for path in encoded {
    hasher.update((path.len() as u64).to_le_bytes());
    hasher.update(path);
  }
  let identity = ContentDigest::from_sha256_bytes(hasher.finalize().into());
  let marker = output_parent.join(format!(".cargo-rail-restore-{identity}.json"));
  let lock = output_parent.join(format!(".cargo-rail-restore-{identity}.lock"));
  let transaction_directory = output_parent.join(format!(".cargo-rail-restore-{identity}"));
  let output_sources = bindings
    .into_iter()
    .map(|(role, slot, destination)| {
      let source = if role == "dep_info" {
        transaction_directory
          .join(RESTORE_PREPARED_DIRECTORY)
          .join(RESTORE_PREPARED_DEP_INFO_FILE)
      } else {
        transaction_directory.join(RESTORE_VERIFIED_DIRECTORY).join(slot)
      };
      (destination.to_path_buf(), source)
    })
    .collect();
  Ok(NativeRestorePaths {
    output_parent,
    marker,
    lock,
    transaction_directory,
    output_sources,
  })
}

fn begin_restore_transaction(
  outputs: &NativeOutputPaths,
  source_root: &Path,
  observation_directory: &Path,
  action_key: &str,
) -> RailResult<NativeRestoreTransaction> {
  validate_action_key(action_key)?;
  let paths = restore_commit_paths(outputs, source_root)?;
  let lock = crate::utils::open_cache_lock_file(&paths.lock, true)?;
  if !crate::utils::private_file_matches_path(&lock, &paths.lock, 0)? {
    return Err(RailError::message(
      "native restore-commit lock is not a private empty file",
    ));
  }
  lock.lock()?;
  recover_restore_commit_locked(&paths, observation_directory)?;
  fs::create_dir(&paths.transaction_directory)?;
  let transaction_identity = native_restore_directory_identity(&paths.transaction_directory)?;
  let transaction_id = digest(format!("{action_key}\0{}\0{}", std::process::id(), native_unix_nanos()).as_bytes());
  let registration = NativeRestoreRegistration {
    version: NATIVE_RESTORE_TRANSACTION_VERSION,
    transaction_id: transaction_id.clone(),
    action_key: action_key.to_string(),
    directory_identity: transaction_identity.clone(),
  };
  let registration_path = paths.transaction_directory.join(RESTORE_REGISTRATION_FILE);
  if let Err(error) = write_restore_record(&registration_path, &registration)
    .and_then(|_| sync_native_directory(&paths.transaction_directory))
    .and_then(|_| sync_native_directory(&paths.output_parent))
  {
    let cleanup = cleanup_restore_transaction_directory(&paths, &transaction_identity);
    return Err(match cleanup {
      Ok(()) => error,
      Err(cleanup) => RailError::message(format!(
        "{error}; native restore registration cleanup failed: {cleanup}"
      )),
    });
  }
  Ok(NativeRestoreTransaction {
    paths,
    observation_directory: observation_directory.to_path_buf(),
    registration,
    state: NativeRestoreTransactionState::Registered,
    _lock: lock,
  })
}

impl NativeRestoreTransaction {
  fn commit(&self, members: Vec<NativeRestoreMember>) -> RailResult<NativeRestoreCommit> {
    Ok(NativeRestoreCommit {
      version: NATIVE_RESTORE_TRANSACTION_VERSION,
      transaction_id: self.registration.transaction_id.clone(),
      action_key: self.registration.action_key.clone(),
      transaction_directory: restore_path_string(&self.paths.transaction_directory)?.to_string(),
      transaction_identity: self.registration.directory_identity.clone(),
      members,
    })
  }

  fn authorize(
    &mut self,
    prepared: &PreparedNativeRestore,
    observation_directory: &Path,
    current_observation: &RawCompilerInvocation,
  ) -> RailResult<()> {
    if !matches!(self.state, NativeRestoreTransactionState::Registered)
      || observation_directory != self.observation_directory
    {
      return Err(RailError::message(
        "native restore transaction is not awaiting authorization",
      ));
    }
    validate_registered_restore_transaction(self)?;
    if prepared.outputs.len() != self.paths.output_sources.len() {
      return Err(RailError::message(
        "native restore prepared-output inventory is incomplete",
      ));
    }
    let mut members = Vec::with_capacity(prepared.outputs.len().saturating_add(1));
    let mut seen = BTreeSet::new();
    for output in &prepared.outputs {
      let Some(expected_source) = self.paths.output_sources.get(&output.destination) else {
        return Err(RailError::message(
          "native restore prepared an output outside its exact transaction",
        ));
      };
      if &output.source != expected_source
        || !seen.insert(output.destination.clone())
        || native_restore_file_identity(&output.opened)? != output.source_identity
        || !crate::utils::private_file_matches_path(&output.opened, &output.source, output.bytes)?
      {
        return Err(RailError::message(
          "native restore prepared-output authority changed before registration",
        ));
      }
      validate_sha256(&output.observation.content_digest)?;
      members.push(NativeRestoreMember::Output {
        source: restore_path_string(&output.source)?.to_string(),
        destination: restore_path_string(&output.destination)?.to_string(),
        source_identity: output.source_identity.clone(),
        previous_identity: native_restore_destination_identity(&output.destination)?,
        content_digest: output.observation.content_digest.clone(),
      });
    }
    if seen.len() != self.paths.output_sources.len() {
      return Err(RailError::message(
        "native restore prepared-output inventory does not own every destination",
      ));
    }
    validate_restore_observation_destination(prepared.observation.destination(), observation_directory)?;
    validate_sha256(prepared.observation.content_digest())?;
    let preexisting = validate_restore_observation_state(
      prepared.observation.destination(),
      prepared.observation.encoded().len() as u64,
      prepared.observation.content_digest(),
    )?;
    members.push(NativeRestoreMember::Observation {
      destination: restore_path_string(prepared.observation.destination())?.to_string(),
      content_digest: prepared.observation.content_digest().to_string(),
      bytes: prepared.observation.encoded().len() as u64,
      preexisting,
    });
    members.sort_unstable_by(|left, right| left.destination().cmp(right.destination()));
    let commit = self.commit(members)?;
    validate_restore_commit_contract(&commit, &self.paths, observation_directory)?;
    let pending_path = self.paths.transaction_directory.join(RESTORE_PENDING_COMMIT_FILE);
    let pending = write_restore_record(&pending_path, &commit)?;
    #[cfg(windows)]
    let pending_identity = native_restore_file_identity(&pending)?;
    sync_native_directory(&self.paths.transaction_directory)?;
    restore_commit_test_fault("after_pending_commit", 0, current_observation)?;
    #[cfg(windows)]
    let marker = {
      if native_restore_file_identity(&pending)? != pending_identity
        || !crate::utils::private_file_matches_path(&pending, &pending_path, pending_identity.bytes)?
      {
        return Err(RailError::message(
          "native restore authority record changed at the publication boundary",
        ));
      }
      rename_restore_write_through(&pending_path, &pending, &pending_identity, &self.paths.marker, None)?;
      pending
    };
    #[cfg(not(windows))]
    let marker = {
      let temporary_path = tempfile::TempPath::try_from_path(pending_path)?;
      let temporary = tempfile::NamedTempFile::from_parts(pending, temporary_path);
      temporary.persist_noclobber(&self.paths.marker).map_err(|error| {
        RailError::message(format!(
          "failed to publish native restore authority marker '{}': {}",
          self.paths.marker.display(),
          error.error
        ))
      })?
    };
    self.state = NativeRestoreTransactionState::Committed(commit);
    drop(marker);
    restore_commit_test_fault("after_marker_publish", 0, current_observation)?;
    sync_native_directory(&self.paths.transaction_directory)?;
    sync_native_directory(&self.paths.output_parent)
  }

  fn output_member(&self, destination: &Path) -> RailResult<&NativeRestoreMember> {
    let NativeRestoreTransactionState::Committed(commit) = &self.state else {
      return Err(RailError::message("native restore transaction is not authorized"));
    };
    commit
      .members
      .iter()
      .find(|member| matches!(member, NativeRestoreMember::Output { destination: registered, .. } if Path::new(registered) == destination))
      .ok_or_else(|| RailError::message("native restore transaction does not own the requested output"))
  }

  fn complete(&mut self) -> RailResult<()> {
    let NativeRestoreTransactionState::Committed(commit) = &self.state else {
      return Err(RailError::message("native restore transaction cannot be committed"));
    };
    remove_restore_authority_marker(&self.paths, commit)?;
    self.state = NativeRestoreTransactionState::Complete;
    sync_native_directory(&self.paths.output_parent)
  }

  fn cleanup_private(&self) -> RailResult<()> {
    if !matches!(self.state, NativeRestoreTransactionState::Complete) {
      return Err(RailError::message("native restore transaction is still authoritative"));
    }
    cleanup_restore_transaction_directory(&self.paths, &self.registration.directory_identity)
  }

  fn rollback(&mut self) -> RailResult<()> {
    if matches!(self.state, NativeRestoreTransactionState::Registered) {
      if restore_path_exists(&self.paths.marker)? {
        return Err(RailError::message(
          "native restore authority appeared before transaction authorization",
        ));
      }
      validate_registered_restore_transaction(self)?;
      return cleanup_restore_transaction_directory(&self.paths, &self.registration.directory_identity);
    }
    if let NativeRestoreTransactionState::Committed(commit) = &self.state {
      validate_authorized_restore_transaction(&self.paths, &self.observation_directory, &self.registration, commit)?;
      cleanup_restore_member_destinations(&commit.members)?;
      remove_restore_authority_marker(&self.paths, commit)?;
      self.state = NativeRestoreTransactionState::Complete;
      sync_native_directory(&self.paths.output_parent)?;
    }
    cleanup_restore_transaction_directory(&self.paths, &self.registration.directory_identity)
  }
}

fn recover_restore_commit(
  outputs: &NativeOutputPaths,
  source_root: &Path,
  observation_directory: &Path,
) -> RailResult<()> {
  let paths = restore_commit_paths(outputs, source_root)?;
  let lock = crate::utils::open_cache_lock_file(&paths.lock, true)?;
  if !crate::utils::private_file_matches_path(&lock, &paths.lock, 0)? {
    return Err(RailError::message(
      "native restore-commit lock is not a private empty file",
    ));
  }
  lock.lock()?;
  recover_restore_commit_locked(&paths, observation_directory)
}

fn recover_restore_commit_locked(paths: &NativeRestorePaths, observation_directory: &Path) -> RailResult<()> {
  let marker_exists = restore_path_exists(&paths.marker)?;
  let transaction_exists = restore_path_exists(&paths.transaction_directory)?;
  if !marker_exists {
    if !transaction_exists {
      return Ok(());
    }
    return recover_private_restore_transaction(paths);
  }
  if !transaction_exists {
    return Err(RailError::message(
      "native restore authority marker has no registered transaction directory",
    ));
  }
  let commit: NativeRestoreCommit = read_restore_record(&paths.marker, "authority marker")?;
  let registration: NativeRestoreRegistration = read_restore_record(
    &paths.transaction_directory.join(RESTORE_REGISTRATION_FILE),
    "registration",
  )?;
  validate_authorized_restore_transaction(paths, observation_directory, &registration, &commit)?;
  cleanup_restore_member_destinations(&commit.members)?;
  remove_restore_authority_marker(paths, &commit)?;
  sync_native_directory(&paths.output_parent)?;
  cleanup_restore_transaction_directory(paths, &commit.transaction_identity)
}

fn validate_restore_observation_destination(destination: &Path, directory: &Path) -> RailResult<()> {
  if destination.parent() != Some(directory) {
    return Err(RailError::message(
      "native restore observation is outside its private command directory",
    ));
  }
  validate_restore_observation_name(destination)
}

fn validate_restore_observation_history_destination(destination: &Path, current_directory: &Path) -> RailResult<()> {
  validate_restore_observation_name(destination)?;
  if destination.parent() == Some(current_directory) {
    return Ok(());
  }
  let historical_directory = destination
    .parent()
    .and_then(Path::file_name)
    .and_then(OsStr::to_str)
    .is_some_and(|name| {
      name.starts_with("cargo-rail-native-cargo-") || name.starts_with("cargo-rail-compiler-observations-")
    });
  if destination.is_absolute() && historical_directory {
    Ok(())
  } else {
    Err(RailError::message(
      "native restore observation does not name a private command receipt",
    ))
  }
}

fn validate_restore_observation_name(destination: &Path) -> RailResult<()> {
  let Some(name) = destination.file_name().and_then(OsStr::to_str) else {
    return Err(RailError::message(
      "native restore observation has an invalid destination name",
    ));
  };
  let Some(identity) = name
    .strip_prefix("rustc-sha256-")
    .and_then(|name| name.strip_suffix(".json"))
  else {
    return Err(RailError::message(
      "native restore observation has the wrong destination contract",
    ));
  };
  validate_identity(identity, "").map(|_| ())
}

fn recover_private_restore_transaction(paths: &NativeRestorePaths) -> RailResult<()> {
  let registration_path = paths.transaction_directory.join(RESTORE_REGISTRATION_FILE);
  if !restore_path_exists(&registration_path)? {
    let metadata = fs::symlink_metadata(&paths.transaction_directory)?;
    if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
      return Err(RailError::message(
        "native restore transaction path is not a real directory",
      ));
    }
    if fs::read_dir(&paths.transaction_directory)?.next().is_some() {
      return Err(RailError::message(
        "native restore transaction has state without a durable registration",
      ));
    }
    fs::remove_dir(&paths.transaction_directory)?;
    return sync_native_directory(&paths.output_parent);
  }
  let registration: NativeRestoreRegistration = match read_restore_record(&registration_path, "registration") {
    Ok(registration) => registration,
    Err(_error) if restore_transaction_contains_only_registration(paths)? => {
      let identity = native_restore_directory_identity(&paths.transaction_directory)?;
      return cleanup_restore_transaction_directory(paths, &identity);
    }
    Err(error) => return Err(error),
  };
  validate_restore_registration(
    &registration,
    &native_restore_directory_identity(&paths.transaction_directory)?,
  )?;
  validate_restore_transaction_inventory(paths, &registration.directory_identity)?;
  cleanup_restore_transaction_directory(paths, &registration.directory_identity)
}

fn validate_registered_restore_transaction(transaction: &NativeRestoreTransaction) -> RailResult<()> {
  let current_identity = native_restore_directory_identity(&transaction.paths.transaction_directory)?;
  let registration: NativeRestoreRegistration = read_restore_record(
    &transaction.paths.transaction_directory.join(RESTORE_REGISTRATION_FILE),
    "registration",
  )?;
  if current_identity != transaction.registration.directory_identity || registration != transaction.registration {
    return Err(RailError::message(
      "native restore transaction changed after registration",
    ));
  }
  validate_restore_transaction_inventory(&transaction.paths, &transaction.registration.directory_identity)
}

fn validate_authorized_restore_transaction(
  paths: &NativeRestorePaths,
  observation_directory: &Path,
  registration: &NativeRestoreRegistration,
  commit: &NativeRestoreCommit,
) -> RailResult<()> {
  let stored: NativeRestoreCommit = read_restore_record(&paths.marker, "authority marker")?;
  if stored != *commit {
    return Err(RailError::message(
      "native restore authority marker changed after authorization",
    ));
  }
  validate_restore_commit_contract(commit, paths, observation_directory)?;
  validate_restore_registration(registration, &commit.transaction_identity)?;
  if registration.transaction_id != commit.transaction_id || registration.action_key != commit.action_key {
    return Err(RailError::message(
      "native restore authority marker does not match its registration",
    ));
  }
  if native_restore_directory_identity(&paths.transaction_directory)? != commit.transaction_identity {
    return Err(RailError::message(
      "native restore transaction directory was replaced after authorization",
    ));
  }
  validate_restore_transaction_inventory(paths, &commit.transaction_identity)?;
  if restore_path_exists(&paths.transaction_directory.join(RESTORE_PENDING_COMMIT_FILE))? {
    return Err(RailError::message(
      "native restore transaction retained a pending commit after authorization",
    ));
  }
  Ok(())
}

fn remove_restore_authority_marker(paths: &NativeRestorePaths, expected: &NativeRestoreCommit) -> RailResult<()> {
  let stored: NativeRestoreCommit = read_restore_record(&paths.marker, "authority marker")?;
  if stored != *expected {
    return Err(RailError::message(
      "native restore authority marker changed before removal",
    ));
  }
  fs::remove_file(&paths.marker)?;
  Ok(())
}

fn validate_restore_registration(
  registration: &NativeRestoreRegistration,
  expected_identity: &NativeRestoreDirectoryIdentity,
) -> RailResult<()> {
  if registration.version != NATIVE_RESTORE_TRANSACTION_VERSION
    || validate_sha256(&registration.transaction_id).is_err()
    || validate_action_key(&registration.action_key).is_err()
    || &registration.directory_identity != expected_identity
  {
    return Err(RailError::message(
      "native restore registration does not match its canonical contract",
    ));
  }
  Ok(())
}

fn validate_restore_commit_contract(
  commit: &NativeRestoreCommit,
  paths: &NativeRestorePaths,
  observation_directory: &Path,
) -> RailResult<()> {
  if commit.version != NATIVE_RESTORE_TRANSACTION_VERSION
    || validate_sha256(&commit.transaction_id).is_err()
    || validate_action_key(&commit.action_key).is_err()
    || commit.transaction_directory != restore_path_string(&paths.transaction_directory)?
    || commit.members.len() != paths.output_sources.len().saturating_add(1)
    || !commit
      .members
      .windows(2)
      .all(|pair| pair[0].destination() < pair[1].destination())
  {
    return Err(RailError::message(
      "native restore authority marker does not match its canonical contract",
    ));
  }
  let mut expected_outputs = paths.output_sources.keys().cloned().collect::<BTreeSet<_>>();
  let mut observation = None;
  for member in &commit.members {
    match member {
      NativeRestoreMember::Output {
        source,
        destination,
        source_identity,
        previous_identity,
        content_digest,
      } => {
        validate_sha256(content_digest)?;
        let destination = Path::new(destination);
        let Some(expected_source) = paths.output_sources.get(destination) else {
          return Err(RailError::message(
            "native restore authority marker contains an unowned output",
          ));
        };
        if Path::new(source) != expected_source
          || !expected_outputs.remove(destination)
          || previous_identity.as_ref() == Some(source_identity)
        {
          return Err(RailError::message(
            "native restore output member does not match its exact capability",
          ));
        }
      }
      NativeRestoreMember::Observation {
        destination,
        content_digest,
        bytes: _,
        preexisting: _,
      } => {
        validate_sha256(content_digest)?;
        let destination = Path::new(destination);
        if observation.replace(destination).is_some() || paths.output_sources.contains_key(destination) {
          return Err(RailError::message(
            "native restore authority marker has an ambiguous observation",
          ));
        }
        validate_restore_observation_history_destination(destination, observation_directory)?;
      }
    }
  }
  if !expected_outputs.is_empty() || observation.is_none() {
    return Err(RailError::message(
      "native restore authority marker does not own every destination",
    ));
  }
  Ok(())
}

fn validate_restore_observation_state(destination: &Path, bytes: u64, expected_digest: &str) -> RailResult<bool> {
  match fs::symlink_metadata(destination) {
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
    Err(error) => Err(error.into()),
    Ok(metadata)
      if metadata.is_file()
        && !crate::utils::is_symlink_or_reparse(&metadata)
        && single_link(&metadata)
        && metadata.len() == bytes =>
    {
      let limit = usize::try_from(bytes)
        .map_err(|_| RailError::message("native restore observation exceeds the addressable bound"))?;
      let existing = read_bounded(destination, limit)?;
      if digest(&existing) == expected_digest {
        Ok(true)
      } else {
        Err(RailError::message(
          "native restore observation content identity collided",
        ))
      }
    }
    Ok(_) => Err(RailError::message(
      "native restore observation destination is prepositioned",
    )),
  }
}

fn cleanup_restore_member_destinations(members: &[NativeRestoreMember]) -> RailResult<()> {
  let planned = members
    .iter()
    .map(plan_restore_member_cleanup)
    .collect::<RailResult<Vec<_>>>()?;
  let mut parents = BTreeSet::new();
  for (member, planned) in members.iter().zip(planned) {
    let Some(path) = planned else {
      continue;
    };
    if plan_restore_member_cleanup(member)?.as_ref() != Some(&path) {
      return Err(RailError::message(
        "native restore destination changed immediately before recovery",
      ));
    }
    fs::remove_file(&path)?;
    if let Some(parent) = path.parent() {
      parents.insert(parent.to_path_buf());
    }
  }
  for parent in parents {
    if restore_path_exists(&parent)? {
      sync_native_directory(&parent)?;
    }
  }
  Ok(())
}

fn plan_restore_member_cleanup(member: &NativeRestoreMember) -> RailResult<Option<PathBuf>> {
  match member {
    NativeRestoreMember::Output {
      destination,
      source_identity,
      previous_identity,
      content_digest,
      ..
    } => {
      let destination = PathBuf::from(destination);
      let Some(current) = native_restore_destination_identity(&destination)? else {
        return Ok(None);
      };
      if previous_identity.as_ref() == Some(&current) {
        return Ok(None);
      }
      if &current == source_identity && restore_file_matches_digest(&destination, source_identity, content_digest)? {
        Ok(Some(destination))
      } else {
        Err(RailError::message(format!(
          "native restore output '{}' was replaced outside its transaction",
          destination.display()
        )))
      }
    }
    NativeRestoreMember::Observation {
      destination,
      content_digest,
      bytes,
      preexisting,
    } => {
      let destination = PathBuf::from(destination);
      let exists = validate_restore_observation_state(&destination, *bytes, content_digest)?;
      if *preexisting || !exists {
        Ok(None)
      } else {
        Ok(Some(destination))
      }
    }
  }
}

fn restore_file_matches_digest(
  path: &Path,
  expected_identity: &NativeRestoreFileIdentity,
  expected_digest: &str,
) -> RailResult<bool> {
  let mut file = File::open(path)?;
  if native_restore_file_identity(&file)? != *expected_identity
    || !crate::utils::private_file_matches_path(&file, path, expected_identity.bytes)?
  {
    return Ok(false);
  }
  let mut hasher = Sha256::new();
  let mut buffer = [0_u8; 64 * 1024];
  let mut bytes = 0u64;
  loop {
    let read = file.read(&mut buffer)?;
    if read == 0 {
      break;
    }
    bytes = bytes.saturating_add(read as u64);
    hasher.update(&buffer[..read]);
  }
  let actual = format!("sha256:{}", ContentDigest::from_sha256_bytes(hasher.finalize().into()));
  Ok(
    bytes == expected_identity.bytes
      && actual == expected_digest
      && native_restore_file_identity(&file)? == *expected_identity
      && crate::utils::private_file_matches_path(&file, path, expected_identity.bytes)?,
  )
}

fn native_restore_destination_identity(path: &Path) -> RailResult<Option<NativeRestoreFileIdentity>> {
  let metadata = match fs::symlink_metadata(path) {
    Ok(metadata) => metadata,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error.into()),
  };
  if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) || !single_link(&metadata) {
    return Err(RailError::message(format!(
      "native restore destination '{}' is not a private regular file",
      path.display()
    )));
  }
  let opened = File::open(path)?;
  if !crate::utils::private_file_matches_path(&opened, path, metadata.len())? {
    return Err(RailError::message(format!(
      "native restore destination '{}' changed while its identity was captured",
      path.display()
    )));
  }
  Ok(Some(native_restore_file_identity(&opened)?))
}

fn validate_restore_destination_state(
  destination: &Path,
  expected: Option<&NativeRestoreFileIdentity>,
) -> RailResult<()> {
  if native_restore_destination_identity(destination)?.as_ref() == expected {
    Ok(())
  } else {
    Err(RailError::message(format!(
      "native restore destination '{}' changed after authorization",
      destination.display()
    )))
  }
}

fn native_restore_file_identity(opened: &File) -> RailResult<NativeRestoreFileIdentity> {
  let metadata = opened.metadata()?;
  if !metadata.is_file() || !single_link(&metadata) {
    return Err(RailError::message(
      "native restore member is not a private regular file",
    ));
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt as _;
    Ok(NativeRestoreFileIdentity {
      bytes: metadata.len(),
      device: metadata.dev(),
      inode: metadata.ino(),
    })
  }
  #[cfg(windows)]
  {
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0010;
    let observation = crate::windows_fs::observe_file(opened)?;
    crate::windows_fs::prove_local_ntfs(opened, observation.volume_serial_number)?;
    if observation.file_attributes & FILE_ATTRIBUTE_DIRECTORY != 0
      || observation.number_of_links != 1
      || observation.size != metadata.len()
    {
      return Err(RailError::message(
        "native restore member handle is not a private regular file",
      ));
    }
    Ok(NativeRestoreFileIdentity {
      bytes: observation.size,
      volume_serial_number: observation.volume_serial_number,
      file_id: observation.file_id,
    })
  }
  #[cfg(not(any(unix, windows)))]
  {
    Ok(NativeRestoreFileIdentity { bytes: metadata.len() })
  }
}

fn native_restore_directory_identity(path: &Path) -> RailResult<NativeRestoreDirectoryIdentity> {
  let named = fs::symlink_metadata(path)?;
  if !named.is_dir() || crate::utils::is_symlink_or_reparse(&named) {
    return Err(RailError::message(
      "native restore transaction path is not a real directory",
    ));
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt as _;
    let opened = File::open(path)?;
    let opened = opened.metadata()?;
    let current = fs::symlink_metadata(path)?;
    if !opened.is_dir()
      || !current.is_dir()
      || crate::utils::is_symlink_or_reparse(&current)
      || opened.dev() != current.dev()
      || opened.ino() != current.ino()
    {
      return Err(RailError::message(
        "native restore transaction path changed while its identity was captured",
      ));
    }
    Ok(NativeRestoreDirectoryIdentity {
      device: opened.dev(),
      inode: opened.ino(),
    })
  }
  #[cfg(windows)]
  {
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0010;
    let opened = crate::windows_fs::open_for_observation(path)?;
    let observation = crate::windows_fs::observe_file(&opened)?;
    crate::windows_fs::prove_local_ntfs(&opened, observation.volume_serial_number)?;
    let current = crate::windows_fs::open_for_observation(path)?;
    let current_observation = crate::windows_fs::observe_file(&current)?;
    crate::windows_fs::prove_local_ntfs(&current, current_observation.volume_serial_number)?;
    if observation != current_observation || observation.file_attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
      return Err(RailError::message(
        "native restore transaction path changed while its identity was captured",
      ));
    }
    Ok(NativeRestoreDirectoryIdentity {
      volume_serial_number: observation.volume_serial_number,
      file_id: observation.file_id,
    })
  }
  #[cfg(not(any(unix, windows)))]
  {
    Ok(NativeRestoreDirectoryIdentity {})
  }
}

fn restore_transaction_inventory(paths: &NativeRestorePaths) -> (BTreeSet<PathBuf>, BTreeSet<PathBuf>) {
  let root = &paths.transaction_directory;
  let verified = root.join(RESTORE_VERIFIED_DIRECTORY);
  let materializing = root.join(RESTORE_MATERIALIZING_DIRECTORY);
  let materialized = materializing.join("output");
  let prepared = root.join(RESTORE_PREPARED_DIRECTORY);
  let directories = BTreeSet::from([
    root.clone(),
    verified.clone(),
    verified.join("target"),
    verified.join("target/outputs"),
    verified.join("target/streams"),
    materializing,
    materialized.clone(),
    materialized.join("target"),
    materialized.join("target/outputs"),
    materialized.join("target/streams"),
    prepared.clone(),
  ]);
  let mut files = BTreeSet::from([
    root.join(RESTORE_REGISTRATION_FILE),
    root.join(RESTORE_PENDING_COMMIT_FILE),
    verified.join(DEP_INFO_SLOT),
    verified.join(STDOUT_SLOT),
    verified.join(STDERR_SLOT),
    materialized.join(DEP_INFO_SLOT),
    materialized.join(STDOUT_SLOT),
    materialized.join(STDERR_SLOT),
    prepared.join(RESTORE_PREPARED_DEP_INFO_FILE),
  ]);
  for source in paths.output_sources.values() {
    let Ok(relative) = source.strip_prefix(&verified) else {
      continue;
    };
    files.insert(source.clone());
    files.insert(materialized.join(relative));
  }
  (directories, files)
}

fn restore_transaction_contains_only_registration(paths: &NativeRestorePaths) -> RailResult<bool> {
  let metadata = fs::symlink_metadata(&paths.transaction_directory)?;
  if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
    return Ok(false);
  }
  let mut entries = fs::read_dir(&paths.transaction_directory)?;
  let Some(entry) = entries.next().transpose()? else {
    return Ok(false);
  };
  let registration = paths.transaction_directory.join(RESTORE_REGISTRATION_FILE);
  if entry.path() != registration || entries.next().is_some() {
    return Ok(false);
  }
  let metadata = fs::symlink_metadata(&registration)?;
  if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) || !single_link(&metadata) {
    return Ok(false);
  }
  let opened = File::open(&registration)?;
  Ok(crate::utils::private_file_matches_path(
    &opened,
    &registration,
    metadata.len(),
  )?)
}

fn validate_restore_transaction_inventory(
  paths: &NativeRestorePaths,
  expected_identity: &NativeRestoreDirectoryIdentity,
) -> RailResult<()> {
  if native_restore_directory_identity(&paths.transaction_directory)? != *expected_identity {
    return Err(RailError::message(
      "native restore transaction directory identity changed",
    ));
  }
  let (directories, files) = restore_transaction_inventory(paths);
  for directory in &directories {
    match fs::symlink_metadata(directory) {
      Ok(metadata) if metadata.is_dir() && !crate::utils::is_symlink_or_reparse(&metadata) => {
        for entry in fs::read_dir(directory)? {
          let path = entry?.path();
          if !directories.contains(&path) && !files.contains(&path) {
            return Err(RailError::message(format!(
              "native restore transaction contains unknown member '{}'",
              path.display()
            )));
          }
        }
      }
      Ok(_) => {
        return Err(RailError::message(format!(
          "native restore transaction directory '{}' was replaced",
          directory.display()
        )));
      }
      Err(error) if error.kind() == std::io::ErrorKind::NotFound && directory != &paths.transaction_directory => {}
      Err(error) => return Err(error.into()),
    }
  }
  for path in files {
    match fs::symlink_metadata(&path) {
      Ok(metadata)
        if metadata.is_file() && !crate::utils::is_symlink_or_reparse(&metadata) && single_link(&metadata) =>
      {
        let opened = File::open(&path)?;
        if !crate::utils::private_file_matches_path(&opened, &path, metadata.len())? {
          return Err(RailError::message(format!(
            "native restore transaction member '{}' changed while it was inspected",
            path.display()
          )));
        }
      }
      Ok(_) => {
        return Err(RailError::message(format!(
          "native restore transaction member '{}' is not a private regular file",
          path.display()
        )));
      }
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
      Err(error) => return Err(error.into()),
    }
  }
  Ok(())
}

fn cleanup_restore_transaction_directory(
  paths: &NativeRestorePaths,
  expected_identity: &NativeRestoreDirectoryIdentity,
) -> RailResult<()> {
  if !restore_path_exists(&paths.transaction_directory)? {
    return Ok(());
  }
  validate_restore_transaction_inventory(paths, expected_identity)?;
  let (directories, files) = restore_transaction_inventory(paths);
  let registration = paths.transaction_directory.join(RESTORE_REGISTRATION_FILE);
  for path in files.iter().filter(|path| **path != registration) {
    remove_restore_file_if_present(path)?;
  }
  let mut nested = directories
    .into_iter()
    .filter(|path| path != &paths.transaction_directory)
    .collect::<Vec<_>>();
  nested.sort_unstable_by_key(|path| std::cmp::Reverse(path.components().count()));
  for directory in nested {
    remove_restore_directory_if_present(&directory)?;
  }
  sync_native_directory(&paths.transaction_directory)?;
  remove_restore_file_if_present(&registration)?;
  sync_native_directory(&paths.transaction_directory)?;
  fs::remove_dir(&paths.transaction_directory)?;
  sync_native_directory(&paths.output_parent)
}

fn write_restore_record<T: Serialize>(path: &Path, value: &T) -> RailResult<File> {
  let bytes = serde_json::to_vec(value)?;
  if bytes.len() > MAX_RESTORE_COMMIT_BYTES {
    return Err(RailError::message(
      "native restore transaction record exceeds its bound",
    ));
  }
  let mut file = OpenOptions::new().read(true).write(true).create_new(true).open(path)?;
  file.write_all(&bytes)?;
  file.sync_all()?;
  if !crate::utils::private_file_matches_path(&file, path, bytes.len() as u64)? {
    return Err(RailError::message(
      "native restore transaction record changed while it was written",
    ));
  }
  Ok(file)
}

fn read_restore_record<T>(path: &Path, label: &str) -> RailResult<T>
where
  T: Serialize + for<'de> Deserialize<'de>,
{
  let metadata = fs::symlink_metadata(path)?;
  if !metadata.is_file()
    || crate::utils::is_symlink_or_reparse(&metadata)
    || !single_link(&metadata)
    || metadata.len() > MAX_RESTORE_COMMIT_BYTES as u64
  {
    return Err(RailError::message(format!(
      "native restore {label} is not a bounded private file"
    )));
  }
  let mut file = File::open(path)?;
  if !crate::utils::private_file_matches_path(&file, path, metadata.len())? {
    return Err(RailError::message(format!(
      "native restore {label} changed while it was opened"
    )));
  }
  let mut bytes = Vec::with_capacity(metadata.len() as usize);
  std::io::Read::by_ref(&mut file)
    .take(MAX_RESTORE_COMMIT_BYTES as u64 + 1)
    .read_to_end(&mut bytes)?;
  if bytes.len() as u64 != metadata.len() {
    return Err(RailError::message(format!(
      "native restore {label} changed while it was read"
    )));
  }
  let value: T =
    serde_json::from_slice(&bytes).map_err(|_| RailError::message(format!("native restore {label} is malformed")))?;
  if serde_json::to_vec(&value)? != bytes {
    return Err(RailError::message(format!(
      "native restore {label} is not canonically encoded"
    )));
  }
  Ok(value)
}

fn remove_restore_file_if_present(path: &Path) -> RailResult<()> {
  match fs::remove_file(path) {
    Ok(()) => Ok(()),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
    Err(error) => Err(error.into()),
  }
}

fn remove_restore_directory_if_present(path: &Path) -> RailResult<()> {
  match fs::remove_dir(path) {
    Ok(()) => Ok(()),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
    Err(error) => Err(error.into()),
  }
}

fn restore_path_exists(path: &Path) -> RailResult<bool> {
  match fs::symlink_metadata(path) {
    Ok(_) => Ok(true),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
    Err(error) => Err(error.into()),
  }
}

fn restore_path_string(path: &Path) -> RailResult<&str> {
  path
    .to_str()
    .ok_or_else(|| RailError::message("native restore transaction path is not valid UTF-8"))
}

#[cfg(unix)]
fn sync_native_directory(path: &Path) -> RailResult<()> {
  File::open(path)?.sync_all()?;
  Ok(())
}

#[cfg(not(unix))]
fn sync_native_directory(_path: &Path) -> RailResult<()> {
  Ok(())
}

fn native_unix_nanos() -> u128 {
  SystemTime::now()
    .duration_since(SystemTime::UNIX_EPOCH)
    .map_or(0, |duration| duration.as_nanos())
}

#[cfg(debug_assertions)]
fn restore_commit_test_fault(phase: &str, index: usize, observation: &RawCompilerInvocation) -> RailResult<()> {
  if let Some(selected_crate) = std::env::var_os(RESTORE_CRATE_ENV) {
    let selected_crate = selected_crate
      .to_str()
      .ok_or_else(|| RailError::message(format!("{RESTORE_CRATE_ENV} is not valid UTF-8")))?;
    if observation.crate_name.as_deref() != Some(selected_crate) {
      return Ok(());
    }
  }
  let selected_abort = std::env::var_os(RESTORE_ABORT_ENV).and_then(|value| value.into_string().ok());
  if selected_abort
    .as_deref()
    .is_some_and(|selected| selected == phase || selected == format!("{phase}:{index}"))
  {
    std::process::abort();
  }
  let selected_cancel = std::env::var_os(RESTORE_CANCEL_ENV).and_then(|value| value.into_string().ok());
  if selected_cancel
    .as_deref()
    .is_some_and(|selected| selected == phase || selected == format!("{phase}:{index}"))
  {
    return Err(RailError::message(format!(
      "cancelled native restore-commit at {phase}:{index}"
    )));
  }
  let Some(selected) = std::env::var_os(RESTORE_FAULT_ENV) else {
    return Ok(());
  };
  let selected = selected
    .to_str()
    .ok_or_else(|| RailError::message(format!("{RESTORE_FAULT_ENV} is not valid UTF-8")))?;
  if selected == phase || selected == format!("{phase}:{index}") {
    return Err(RailError::message(format!(
      "injected native restore-commit fault at {phase}:{index}"
    )));
  }
  Ok(())
}

#[cfg(not(debug_assertions))]
fn restore_commit_test_fault(_phase: &str, _index: usize, _observation: &RawCompilerInvocation) -> RailResult<()> {
  Ok(())
}

#[cfg(debug_assertions)]
fn capture_test_pause(phase: &str, observation: &RawCompilerInvocation) -> RailResult<()> {
  let Some(selected_phase) = std::env::var_os(CAPTURE_PAUSE_PHASE_ENV) else {
    return Ok(());
  };
  let selected_phase = selected_phase
    .to_str()
    .ok_or_else(|| RailError::message(format!("{CAPTURE_PAUSE_PHASE_ENV} is not valid UTF-8")))?;
  let sequenced = selected_phase.contains(',');
  if !selected_phase.split(',').any(|selected| selected == phase) {
    return Ok(());
  }
  let selected_crate = std::env::var_os(CAPTURE_PAUSE_CRATE_ENV)
    .ok_or_else(|| RailError::message(format!("{CAPTURE_PAUSE_CRATE_ENV} is required")))?;
  let selected_crate = selected_crate
    .to_str()
    .ok_or_else(|| RailError::message(format!("{CAPTURE_PAUSE_CRATE_ENV} is not valid UTF-8")))?;
  if observation.crate_name.as_deref() != Some(selected_crate) {
    return Ok(());
  }
  let directory = std::env::var_os(CAPTURE_PAUSE_DIRECTORY_ENV)
    .map(PathBuf::from)
    .ok_or_else(|| RailError::message(format!("{CAPTURE_PAUSE_DIRECTORY_ENV} is required")))?;
  if !directory.is_absolute() {
    return Err(RailError::message(format!(
      "{CAPTURE_PAUSE_DIRECTORY_ENV} must be absolute"
    )));
  }
  let metadata = fs::symlink_metadata(&directory)?;
  if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
    return Err(RailError::message(
      "native capture pause directory is not a real directory",
    ));
  }
  let ready = directory.join(if sequenced {
    format!("ready-{phase}")
  } else {
    "ready".to_string()
  });
  write_private_command_file(&ready, b"ready\n")?;
  let continued = directory.join(if sequenced {
    format!("continue-{phase}")
  } else {
    "continue".to_string()
  });
  let started = Instant::now();
  loop {
    match fs::symlink_metadata(&continued) {
      Ok(metadata) if metadata.is_file() && !crate::utils::is_symlink_or_reparse(&metadata) => return Ok(()),
      Ok(_) => {
        return Err(RailError::message(
          "native capture pause continuation is not a real file",
        ));
      }
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
      Err(error) => return Err(error.into()),
    }
    if started.elapsed() > TEST_CAPTURE_PAUSE_TIMEOUT {
      return Err(RailError::message("native capture pause timed out"));
    }
    std::thread::park_timeout(Duration::from_millis(2));
  }
}

#[cfg(not(debug_assertions))]
fn capture_test_pause(_phase: &str, _observation: &RawCompilerInvocation) -> RailResult<()> {
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
  capture: &NativeActionCapture,
) -> RailResult<Vec<u8>> {
  let translated = translate_output_binding_bytes(bytes, validation, outputs, source_root, true)?;
  rebind_dep_info_source_roots(&translated, source_root, capture)
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
  capture: &NativeActionCapture,
) -> RailResult<Vec<u8>> {
  let portable = portable_output_binding_bytes(bytes, outputs, source_root, true)?;
  portable_dep_info_source_roots(&portable, source_root, capture)
}

fn portable_output_binding_bytes(
  bytes: &[u8],
  outputs: &NativeOutputPaths,
  source_root: &Path,
  require_replacement: bool,
) -> RailResult<Vec<u8>> {
  if bytes
    .windows(PORTABLE_OUTPUT_BINDING_PREFIX.len())
    .any(|window| window == PORTABLE_OUTPUT_BINDING_PREFIX)
  {
    return Err(RailError::message(
      "native compiler output collides with a reserved output-binding token",
    ));
  }
  let encoding = if require_replacement {
    OutputBindingEncoding::DepInfo
  } else {
    OutputBindingEncoding::Json
  };
  let replacements = output_binding_replacements(outputs, source_root, encoding, true)?;
  let (portable, replacement_count) = replace_output_bindings(bytes, &replacements, encoding);

  if require_replacement && replacement_count == 0
    || output_parent_spellings(outputs, source_root, encoding)?
      .iter()
      .any(|path| contains_path_prefix(&portable, path))
  {
    return Err(RailError::message(
      "native compiler output contains an unmodeled output-directory binding",
    ));
  }
  Ok(portable)
}

fn translate_output_binding_bytes(
  bytes: &[u8],
  validation: &NativeCompilerValidation,
  outputs: &NativeOutputPaths,
  source_root: &Path,
  require_replacement: bool,
) -> RailResult<Vec<u8>> {
  let stored_roles = validation
    .outputs
    .iter()
    .map(|output| output.role.as_str())
    .collect::<BTreeSet<_>>();
  let current_roles = native_output_bindings(outputs)
    .into_iter()
    .map(|(role, _, _)| role)
    .collect::<BTreeSet<_>>();
  if stored_roles != current_roles {
    return Err(RailError::message(
      "native compiler dep-info output roles changed during materialization",
    ));
  }

  let encoding = if require_replacement {
    OutputBindingEncoding::DepInfo
  } else {
    OutputBindingEncoding::Json
  };
  let replacements = output_binding_replacements(outputs, source_root, encoding, false)?;
  let (translated, replacement_count) = replace_output_bindings(bytes, &replacements, encoding);
  if require_replacement && replacement_count == 0
    || translated
      .windows(PORTABLE_OUTPUT_BINDING_PREFIX.len())
      .any(|window| window == PORTABLE_OUTPUT_BINDING_PREFIX)
  {
    return Err(RailError::message(
      "native compiler cached data contains an unmodeled output-directory binding",
    ));
  }
  Ok(translated)
}

fn portable_dep_info_source_roots(
  bytes: &[u8],
  source_root: &Path,
  capture: &NativeActionCapture,
) -> RailResult<Vec<u8>> {
  let mut roots = vec![(source_root, PORTABLE_SOURCE_ROOT)];
  if let Some(package) = &capture.package_binding {
    roots.push((package.spelling.as_path(), PORTABLE_PACKAGE_ROOT));
  }
  let mut replacements = Vec::new();
  for (root, token) in roots {
    for spelling in source_root_spellings(root)? {
      let escaped = escape_dep_info_path(&spelling);
      if escaped != spelling {
        replacements.push((escaped, token.as_bytes().to_vec()));
      }
      replacements.push((spelling, token.as_bytes().to_vec()));
    }
  }
  replacements.sort_unstable_by(|left, right| right.0.len().cmp(&left.0.len()).then_with(|| left.cmp(right)));
  replacements.dedup();
  Ok(replacements.iter().fold(bytes.to_vec(), |current, (from, to)| {
    replace_bytes(&current, from, to).0
  }))
}

fn rebind_dep_info_source_roots(
  bytes: &[u8],
  source_root: &Path,
  capture: &NativeActionCapture,
) -> RailResult<Vec<u8>> {
  let mut bindings = vec![(
    PORTABLE_SOURCE_ROOT.as_bytes().to_vec(),
    escape_dep_info_path(&source_root_display_bytes(source_root)),
  )];
  if let Some(package) = &capture.package_binding {
    bindings.push((
      PORTABLE_PACKAGE_ROOT.as_bytes().to_vec(),
      escape_dep_info_path(&source_root_display_bytes(&package.spelling)),
    ));
  }
  rebind_portable_source_roots(bytes, &bindings)
}

const PORTABLE_OUTPUT_BINDING_PREFIX: &[u8] = b"/cargo-rail/native-output/v3/";

#[derive(Clone, Copy)]
enum OutputBindingEncoding {
  Json,
  DepInfo,
}

fn output_binding_replacements(
  outputs: &NativeOutputPaths,
  source_root: &Path,
  encoding: OutputBindingEncoding,
  to_portable: bool,
) -> RailResult<Vec<(Vec<u8>, Vec<u8>)>> {
  let mut replacements = Vec::new();
  for (role, _, output) in native_output_bindings(outputs) {
    for (scope, path) in output_binding_paths(output, source_root)? {
      for (form, path) in output_path_forms(&path) {
        for (representation, rendered) in encoded_output_path_forms(&path, encoding) {
          let portable = format!("/cargo-rail/native-output/v3/{role}/{scope}/{form}/{representation}").into_bytes();
          replacements.push(if to_portable {
            (rendered, portable)
          } else {
            (portable, rendered)
          });
        }
      }
    }
  }
  replacements.sort_unstable_by(|left, right| right.0.len().cmp(&left.0.len()).then_with(|| left.cmp(right)));
  if replacements
    .windows(2)
    .any(|pair| pair[0].0 == pair[1].0 && pair[0].1 != pair[1].1)
  {
    return Err(RailError::message("native compiler output path spelling is ambiguous"));
  }
  replacements.dedup();
  Ok(replacements)
}

fn output_binding_paths(output: &Path, source_root: &Path) -> RailResult<Vec<(&'static str, String)>> {
  let output_parent = output
    .parent()
    .ok_or_else(|| RailError::message("native compiler output has no parent"))?;
  let file_name = output
    .file_name()
    .ok_or_else(|| RailError::message("native compiler output has no file name"))?;
  let canonical_root = crate::utils::canonicalize_existing(source_root)?;
  let canonical_output = crate::utils::canonicalize_existing(output_parent)?.join(file_name);
  let relative = canonical_output
    .strip_prefix(&canonical_root)
    .map_err(|_| RailError::message("native compiler output is outside the source root"))?;
  let mut paths = vec![
    ("selected", crate::utils::path_to_git_format(output)),
    ("canonical", crate::utils::path_to_git_format(&canonical_output)),
    ("relative", crate::utils::path_to_git_format(relative)),
  ];
  let mut seen = BTreeSet::new();
  paths.retain(|(_, path)| seen.insert(path.clone()));
  Ok(paths)
}

fn output_parent_spellings(
  outputs: &NativeOutputPaths,
  source_root: &Path,
  encoding: OutputBindingEncoding,
) -> RailResult<Vec<Vec<u8>>> {
  let output_parent = native_output_bindings(outputs)[0]
    .2
    .parent()
    .ok_or_else(|| RailError::message("native compiler output has no parent"))?;
  let canonical_root = crate::utils::canonicalize_existing(source_root)?;
  let canonical_parent = crate::utils::canonicalize_existing(output_parent)?;
  let relative_parent = canonical_parent
    .strip_prefix(&canonical_root)
    .map_err(|_| RailError::message("native compiler output parent is outside the source root"))?;
  let paths = [
    crate::utils::path_to_git_format(output_parent),
    crate::utils::path_to_git_format(&canonical_parent),
    crate::utils::path_to_git_format(relative_parent),
  ]
  .into_iter()
  .collect::<BTreeSet<_>>();
  let mut spellings = Vec::new();
  for path in paths {
    for (_, path) in output_path_forms(&path) {
      spellings.extend(
        encoded_output_path_forms(&path, encoding)
          .into_iter()
          .map(|(_, encoded)| encoded),
      );
    }
  }
  spellings.sort();
  spellings.dedup();
  Ok(spellings)
}

fn output_path_forms(path: &str) -> Vec<(&'static str, String)> {
  let forward = path.replace('\\', "/");
  let backward = forward.replace('/', "\\");
  let mut forms = vec![("forward", forward.clone()), ("backward", backward)];
  if let Some((parent, name)) = forward.rsplit_once('/') {
    forms.push(("forward-parent", format!("{parent}\\{name}")));
    forms.push(("backward-parent", format!("{}/{name}", parent.replace('/', "\\"))));
  }
  forms
}

fn encoded_output_path_forms(path: &str, encoding: OutputBindingEncoding) -> Vec<(&'static str, Vec<u8>)> {
  match encoding {
    OutputBindingEncoding::Json => vec![("json", json_string_contents(path.as_bytes()))],
    OutputBindingEncoding::DepInfo => {
      let literal = path.as_bytes().to_vec();
      let escaped = escape_dep_info_path(path.as_bytes());
      if literal == escaped {
        vec![("literal", literal)]
      } else {
        vec![("literal", literal), ("escaped", escaped)]
      }
    }
  }
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

fn replace_output_bindings(
  bytes: &[u8],
  replacements: &[(Vec<u8>, Vec<u8>)],
  encoding: OutputBindingEncoding,
) -> (Vec<u8>, usize) {
  match encoding {
    OutputBindingEncoding::Json => replace_json_artifact_values(bytes, replacements),
    OutputBindingEncoding::DepInfo => {
      replacements
        .iter()
        .fold((bytes.to_vec(), 0usize), |(current, total), (from, to)| {
          let (next, count) = replace_bytes(&current, from, to);
          (next, total.saturating_add(count))
        })
    }
  }
}

fn replace_json_artifact_values(bytes: &[u8], replacements: &[(Vec<u8>, Vec<u8>)]) -> (Vec<u8>, usize) {
  const PREFIX: &[u8] = b"\"artifact\":\"";

  let mut output = Vec::with_capacity(bytes.len());
  let mut cursor = 0usize;
  let mut count = 0usize;
  while let Some(relative) = bytes[cursor..]
    .windows(PREFIX.len())
    .position(|window| window == PREFIX)
  {
    let value = cursor + relative + PREFIX.len();
    output.extend_from_slice(&bytes[cursor..value]);
    if let Some((from, to)) = replacements
      .iter()
      .find(|(from, _)| bytes[value..].starts_with(from) && bytes.get(value + from.len()) == Some(&b'"'))
    {
      output.extend_from_slice(to);
      cursor = value + from.len();
      count = count.saturating_add(1);
    } else {
      cursor = value;
    }
  }
  output.extend_from_slice(&bytes[cursor..]);
  (output, count)
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

/// Write one file in a command-owned private temporary directory before any
/// child can observe it. These bytes are regenerable process handoff, not
/// durable cache authority, so an Apple device-wide sync would be pure stall.
fn write_private_command_file(path: &Path, bytes: &[u8]) -> RailResult<()> {
  let mut options = OpenOptions::new();
  options.write(true).create_new(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt as _;

    options.mode(0o600);
  }
  let mut file = options.open(path)?;
  file.write_all(bytes)?;
  Ok(())
}

fn private_command_directory() -> std::io::Result<tempfile::TempDir> {
  let mut builder = tempfile::Builder::new();
  builder.prefix("cargo-rail-native-cargo-");
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;

    builder.permissions(fs::Permissions::from_mode(0o700));
  }
  builder.tempdir()
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
  if !crate::utils::private_file_matches_path(&file, path, metadata.len())? {
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
  #[cfg(debug_assertions)]
  command
    .env_remove(RESTORE_FAULT_ENV)
    .env_remove(RESTORE_ABORT_ENV)
    .env_remove(RESTORE_CANCEL_ENV)
    .env_remove(RESTORE_CRATE_ENV)
    .env_remove(TEST_CAPTURE_LIMIT_ENV)
    .env_remove(CAPTURE_PAUSE_PHASE_ENV)
    .env_remove(CAPTURE_PAUSE_CRATE_ENV)
    .env_remove(CAPTURE_PAUSE_DIRECTORY_ENV);
  command
    .env_remove(SESSION_ENV)
    .env_remove(DISPOSITION_ENV)
    .env_remove(LEGACY_STORE_ENV)
    .env_remove(crate::remote_cache::TARGETS_ENV)
    .env_remove(crate::hermetic::cas::CACHE_BASE_ENV)
    .env_remove(crate::hermetic::cas::CACHE_MAX_BYTES_ENV)
    .env_remove(crate::hermetic::cas::CACHE_TRUST_DOMAIN_ENV)
    .env_remove(crate::compiler::wrapper::CACHE_WRAPPER_MARKER)
    .env_remove(crate::compiler::wrapper::WRAPPER_MARKER)
    .env_remove(crate::compiler::wrapper::INNER_WRAPPER_ENV)
    .env_remove(crate::compiler::wrapper::RUSTDOC_WRAPPER_MARKER)
    .env_remove(crate::compiler::wrapper::INNER_RUSTDOC_ENV)
    .env_remove(crate::compiler::wrapper::OBSERVATION_DIRECTORY_ENV)
    .env_remove(crate::compiler::wrapper::OBSERVATION_SOURCE_ROOT_ENV)
    .env_remove(crate::compiler::wrapper::OBSERVATION_ONLY_ENV);
}

/// Execute an intentionally uncached dependency producer while keeping its
/// remapped compiler artifacts portable and its live diagnostics root-local.
pub(crate) fn run_portable_bypass(mut command: Command, execution: PortableCompilerExecution, context: &str) -> i32 {
  let mut child = match command.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
    Ok(child) => child,
    Err(error) => {
      eprintln!("{context}: failed to execute compiler: {error}");
      return 1;
    }
  };
  let Some(stdout) = child.stdout.take() else {
    let _ = child.kill();
    let _ = child.wait();
    eprintln!("{context}: compiler stdout pipe is unavailable");
    return 1;
  };
  let Some(stderr) = child.stderr.take() else {
    let _ = child.kill();
    let _ = child.wait();
    eprintln!("{context}: compiler stderr pipe is unavailable");
    return 1;
  };
  let stdout_bindings = execution.stream_bindings.clone();
  let stdout_worker = match std::thread::Builder::new()
    .name("cargo-rail-rustc-stdout".to_string())
    .spawn(move || forward_rebound_compiler_stream(stdout, std::io::stdout(), stdout_bindings))
  {
    Ok(worker) => worker,
    Err(error) => {
      let _ = child.kill();
      let _ = child.wait();
      eprintln!("{context}: failed to forward compiler stdout: {error}");
      return 1;
    }
  };
  let stderr = forward_rebound_compiler_stream(stderr, std::io::stderr(), execution.stream_bindings);
  let status = child.wait();
  let stdout = stdout_worker
    .join()
    .map_err(|_| std::io::Error::other("compiler stdout forwarding thread panicked"))
    .and_then(|result| result);
  match status {
    Ok(status) if status.success() => match stdout.and(stderr) {
      Ok(()) => 0,
      Err(error) => {
        eprintln!("{context}: failed to forward compiler output: {error}");
        1
      }
    },
    Ok(status) => status.code().unwrap_or(1),
    Err(error) => {
      eprintln!("{context}: failed to wait for compiler: {error}");
      1
    }
  }
}

fn forward_rebound_compiler_stream<R: std::io::Read, W: std::io::Write>(
  mut source: R,
  destination: W,
  bindings: Vec<(Vec<u8>, Vec<u8>)>,
) -> std::io::Result<()> {
  let mut destination = SourceRootRebindingWriter::new(destination, bindings);
  let mut failure = None;
  let mut buffer = [0u8; 64 * 1024];
  loop {
    let read = match source.read(&mut buffer) {
      Ok(0) => break,
      Ok(read) => read,
      Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
      Err(error) => return Err(error),
    };
    if failure.is_none()
      && let Err(error) = destination.write_all(&buffer[..read])
    {
      failure = Some(error);
    }
  }
  if failure.is_none()
    && let Err(error) = destination.flush()
  {
    failure = Some(error);
  }
  failure.map_or(Ok(()), Err)
}

/// Execute one eligible cold invocation, replay its exact streams, and publish
/// only a complete successful observation.
pub(crate) fn run_and_store(
  command: Command,
  recorder: InvocationRecorder,
  mut capture: NativeActionCapture,
  base_action_key: String,
  cache_bytes_read: u64,
  trace: &mut NativeCacheWrapperTrace,
  context: &str,
) -> i32 {
  let Some(cache_context) = active_context() else {
    eprintln!("{context}: native compiler cache context disappeared before execution");
    return 2;
  };
  let source_root = &cache_context.source_root;
  let source_root_spelling = &cache_context.source_root_spelling;
  let output_paths = recorder.native_output_paths();
  let output = match run_compiler_with_live_streams(command, source_root, &capture) {
    Ok(output) => output,
    Err(error) => {
      eprintln!("{context}: failed to execute compiler: {error}");
      return 1;
    }
  };
  let CapturedCompilerOutput { status, stdout, stderr } = output;

  let capture_pause_failed =
    status.success() && capture_test_pause("after_compiler_execution", recorder.observation()).is_err();
  let mut raw = match recorder.complete(status.success()) {
    Ok(raw) => raw,
    Err(_) => return status.code().unwrap_or(1),
  };
  if !status.success() {
    let _ = publish_and_record_cold_observation(
      &mut raw,
      "compiler_execution_failed",
      None,
      None,
      0,
      cache_bytes_read,
      trace,
    );
    return status.code().unwrap_or(1);
  }
  if capture_pause_failed {
    let _ = publish_and_record_cold_observation(
      &mut raw,
      "capture_test_pause_failed",
      None,
      None,
      0,
      cache_bytes_read,
      trace,
    );
    return status.code().unwrap_or(1);
  }
  let Some(output_paths) = output_paths else {
    let _ = publish_and_record_cold_observation(
      &mut raw,
      "compiler_output_paths_unavailable",
      None,
      None,
      0,
      cache_bytes_read,
      trace,
    );
    return status.code().unwrap_or(1);
  };
  if stdout.limit_exceeded() || stderr.limit_exceeded() {
    let _ = publish_and_record_cold_observation(
      &mut raw,
      "compiler_stream_limit_exceeded",
      None,
      None,
      0,
      cache_bytes_read,
      trace,
    );
    return status.code().unwrap_or(1);
  }
  let session = NativeCompilerSession::load(&cache_context.session, source_root);
  let session = match session {
    Ok(session) => session,
    Err(_) => {
      let _ = publish_and_record_cold_observation(
        &mut raw,
        "native_cache_session_unavailable",
        None,
        None,
        0,
        cache_bytes_read,
        trace,
      );
      return status.code().unwrap_or(1);
    }
  };
  if let Some(reason) = invocation_bypass_reason(&raw, true, &session.class.host_target) {
    let bytes_hashed = cold_input_bytes(&raw, source_root, 0);
    let _ = publish_and_record_cold_observation(&mut raw, reason, None, None, bytes_hashed, cache_bytes_read, trace);
    return status.code().unwrap_or(1);
  }
  let environment_names = raw
    .environment_reads
    .iter()
    .map(|environment| environment.name.clone())
    .collect::<Vec<_>>();
  let (approved_environment, selected_environment_bytes) = match capture_approved_environment(
    source_root,
    source_root_spelling,
    &capture,
    &environment_names,
    Instant::now(),
  ) {
    Ok(environment) => environment,
    Err(_) => {
      let bytes_hashed = cold_input_bytes(&raw, source_root, 0);
      let _ = publish_and_record_cold_observation(
        &mut raw,
        "compiler_selected_environment_unavailable",
        None,
        None,
        bytes_hashed,
        cache_bytes_read,
        trace,
      );
      return status.code().unwrap_or(1);
    }
  };
  capture.approved_environment = approved_environment;
  let selected_action = match action_key(&session.identity, &session.class, &raw, &capture) {
    Ok(action) => action,
    Err(_) => {
      let bytes_hashed = cold_input_bytes(&raw, source_root, selected_environment_bytes);
      let _ = publish_and_record_cold_observation(
        &mut raw,
        "compiler_selected_action_unavailable",
        None,
        None,
        bytes_hashed,
        cache_bytes_read,
        trace,
      );
      return status.code().unwrap_or(1);
    }
  };
  let witness = match capture.witness(&raw, source_root) {
    Ok(witness) => witness,
    Err(_) => {
      let bytes_hashed = cold_input_bytes(&raw, source_root, 0);
      let _ = publish_and_record_cold_observation(
        &mut raw,
        "compiler_observation_outside_captured_action",
        Some(selected_action),
        None,
        bytes_hashed,
        cache_bytes_read,
        trace,
      );
      return status.code().unwrap_or(1);
    }
  };
  let stdout = match stdout.into_bytes() {
    Some(bytes) => bytes,
    None => {
      let _ = publish_and_record_cold_observation(
        &mut raw,
        "compiler_stdout_unavailable",
        None,
        None,
        0,
        cache_bytes_read,
        trace,
      );
      return status.code().unwrap_or(1);
    }
  };
  let stderr = match stderr.into_bytes() {
    Some(bytes) => bytes,
    None => {
      let _ = publish_and_record_cold_observation(
        &mut raw,
        "compiler_stderr_unavailable",
        None,
        None,
        0,
        cache_bytes_read,
        trace,
      );
      return status.code().unwrap_or(1);
    }
  };
  let preparation_phase = trace.start(NativeCacheWrapperPhase::ColdResultPreparation);
  let cas = LocalCas::open_initialized();
  let prepared = match &cas {
    Ok(cas) => {
      let staging = cache_context
        .publication
        .as_ref()
        .and_then(|publication| {
          publication::staging(publication, cas)
            .ok()
            .map(|staging| (staging, true))
        })
        .or_else(|| cas.native_result_staging().ok().map(|staging| (staging, false)));
      staging
        .ok_or("local_cache_staging_failed")
        .and_then(|(staging, asynchronous)| {
          prepare_cold_result(
            &session,
            &capture,
            &base_action_key,
            SelectedNativeAction {
              action_key: selected_action,
              witness,
            },
            &raw,
            &output_paths,
            CapturedCompilerStreams {
              stdout: &stdout,
              stderr: &stderr,
            },
            source_root,
            source_root_spelling,
            staging,
          )
          .map(|(prepared, proof)| (prepared, proof, asynchronous))
        })
    }
    Err(_) => Err("local_cache_open_failed"),
  };
  let preparation_bytes = selected_environment_bytes.saturating_add(
    prepared
      .as_ref()
      .map_or(0, |(_, proof, _)| proof.environment_bytes_hashed),
  );
  trace.finish(
    preparation_phase,
    NativeCacheWrapperWork {
      bytes_hashed: preparation_bytes,
      ..NativeCacheWrapperWork::default()
    },
  );
  let initial = raw.cache_wrapper.clone().or_else(metadata_from_environment);
  let base_reason = initial
    .as_ref()
    .map(CompilerCacheWrapperMetadata::reason)
    .unwrap_or("exact_action_not_found")
    .to_string();
  let remotely_shareable = capture.remotely_shareable(cache_context.remote.as_ref());
  let publication = prepared.and_then(|(prepared, proof, asynchronous)| {
    if asynchronous {
      let queued = cache_context
        .publication
        .as_ref()
        .ok_or("local_publication_coordinator_unavailable")
        .and_then(|publication| {
          publication::enqueue(
            publication,
            prepared,
            raw.emitted_outputs.clone(),
            proof,
            base_reason.clone(),
            remotely_shareable,
            cache_bytes_read,
            trace,
          )
          .map_err(|_| "local_publication_handoff_failed")
        });
      match queued {
        Ok(()) => return Err("native_result_publication_queued"),
        Err(reason) => return Err(reason),
      }
    }
    let admission_phase = trace.start(NativeCacheWrapperPhase::ColdResultAdmission);
    let mut final_capture_bytes = 0;
    let mut admission_failure = "local_cache_store_failed";
    let admitted = (|| {
      let cas = cas.as_ref().map_err(|_| "local_cache_open_failed")?;
      let (validation, stats) = cas
        .store_native_revalidated(prepared, |validation| {
          final_capture_bytes = validation
            .revalidate_publication(&session, source_root, &proof)
            .inspect_err(|_| {
              admission_failure = "cold_inputs_changed_before_admission";
            })?;
          match cas.publish_native_environment_selector(&base_action_key, &environment_names) {
            Ok(crate::hermetic::cas::NativeEnvironmentSelectorPublication::Created)
            | Ok(crate::hermetic::cas::NativeEnvironmentSelectorPublication::Converged) => Ok(()),
            Ok(crate::hermetic::cas::NativeEnvironmentSelectorPublication::Diverged) => {
              admission_failure = "environment_selector_diverged";
              Err(RailError::message("native compiler environment selector diverged"))
            }
            Err(error) => {
              admission_failure = "environment_selector_publication_failed";
              Err(error)
            }
          }
        })
        .map_err(|_| admission_failure)?;
      Ok((validation, stats.bytes_written, final_capture_bytes))
    })();
    let written = admitted.as_ref().map_or(0, |(_, written, _)| *written);
    trace.finish(
      admission_phase,
      NativeCacheWrapperWork {
        cache_bytes_written: written,
        ..NativeCacheWrapperWork::default()
      },
    );
    admitted
  });
  match publication {
    Err("native_result_publication_queued") => return status.code().unwrap_or(1),
    Ok((validation, written, final_capture_bytes)) => {
      let stored_reason = format!("{base_reason};stored_verified_result");
      let bytes_hashed = cold_input_bytes(&raw, source_root, final_capture_bytes);
      raw.cache_wrapper = Some(CompilerCacheWrapperMetadata::native(
        CompilerCacheWrapperStatus::Miss,
        &stored_reason,
        Some(validation.action_key.clone()),
        Some(validation.result_key.clone()),
        bytes_hashed,
        0,
      ));
      write_cache_event(
        CompilerCacheWrapperStatus::Miss,
        &stored_reason,
        Some(&validation.action_key),
        Some(&validation.result_key),
        remotely_shareable.then_some(base_action_key.as_str()),
        NativeCacheMetrics {
          bytes_hashed,
          cache_bytes_read,
          cache_bytes_written: written,
          bytes_restored: 0,
        },
        trace,
      );
    }
    Err(failure_reason) => {
      let reason = initial.as_ref().map(CompilerCacheWrapperMetadata::reason).map_or_else(
        || failure_reason.to_string(),
        |reason| format!("{reason};{failure_reason}"),
      );
      let bytes_hashed = cold_input_bytes(&raw, source_root, 0);
      raw.cache_wrapper = Some(CompilerCacheWrapperMetadata::native(
        CompilerCacheWrapperStatus::Bypassed,
        &reason,
        initial
          .as_ref()
          .and_then(CompilerCacheWrapperMetadata::action_key)
          .map(str::to_string),
        None,
        bytes_hashed,
        0,
      ));
      write_cache_event(
        CompilerCacheWrapperStatus::Bypassed,
        &reason,
        initial.as_ref().and_then(CompilerCacheWrapperMetadata::action_key),
        None,
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

struct CapturedCompilerOutput {
  status: ExitStatus,
  stdout: CapturedCompilerStream,
  stderr: CapturedCompilerStream,
}

enum CapturedCompilerStream {
  Complete {
    storage: tempfile::SpooledTempFile,
    bytes: usize,
  },
  LimitExceeded,
  Unavailable,
}

impl CapturedCompilerStream {
  fn limit_exceeded(&self) -> bool {
    matches!(self, Self::LimitExceeded)
  }

  fn into_bytes(self) -> Option<Vec<u8>> {
    let Self::Complete { storage, bytes } = self else {
      return None;
    };
    let captured = match storage.into_inner() {
      tempfile::SpooledData::InMemory(cursor) => cursor.into_inner(),
      tempfile::SpooledData::OnDisk(mut file) => {
        file.rewind().ok()?;
        let mut captured = Vec::with_capacity(bytes);
        file.read_to_end(&mut captured).ok()?;
        captured
      }
    };
    if captured.len() != bytes || captured.len() > MAX_STREAM_BYTES {
      return None;
    }
    Some(captured)
  }
}

/// Keep Cargo's compiler streams live while retaining one bounded replay copy.
///
/// Redirecting rustc directly to regular files delays Cargo's JSON stream and
/// materially lengthens dependency-heavy builds. One reader thread drains
/// stdout while this wrapper drains stderr, so both pipes retain their native
/// backpressure. Small streams stay in memory and unusually large streams spill
/// to an unnamed temporary file before the fixed cache limit is enforced.
fn run_compiler_with_live_streams(
  mut command: Command,
  source_root: &Path,
  capture: &NativeActionCapture,
) -> std::io::Result<CapturedCompilerOutput> {
  let mut child = command.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
  let Some(stdout) = child.stdout.take() else {
    let _ = child.kill();
    let _ = child.wait();
    return Err(std::io::Error::other("compiler stdout pipe is unavailable"));
  };
  let Some(stderr) = child.stderr.take() else {
    let _ = child.kill();
    let _ = child.wait();
    return Err(std::io::Error::other("compiler stderr pipe is unavailable"));
  };
  let stdout_roots = source_root_stream_bindings(source_root, capture);
  let stdout_worker = match std::thread::Builder::new()
    .name("cargo-rail-rustc-stdout".to_string())
    .spawn(move || {
      capture_compiler_stream(
        stdout,
        SourceRootRebindingWriter::new(std::io::stdout(), stdout_roots),
        MAX_STREAM_BYTES,
      )
    }) {
    Ok(worker) => worker,
    Err(error) => {
      let _ = child.kill();
      let _ = child.wait();
      return Err(error);
    }
  };
  let stderr = capture_compiler_stream(
    stderr,
    SourceRootRebindingWriter::new(std::io::stderr(), source_root_stream_bindings(source_root, capture)),
    MAX_STREAM_BYTES,
  );
  let status = child.wait();
  let stdout = match stdout_worker.join() {
    Ok(stdout) => stdout,
    Err(_) => CapturedCompilerStream::Unavailable,
  };
  Ok(CapturedCompilerOutput {
    status: status?,
    stdout,
    stderr,
  })
}

struct SourceRootRebindingWriter<W> {
  destination: W,
  bindings: Vec<(Vec<u8>, Vec<u8>)>,
  pending: Vec<u8>,
}

impl<W: std::io::Write> SourceRootRebindingWriter<W> {
  fn new(destination: W, bindings: Vec<(Vec<u8>, Vec<u8>)>) -> Self {
    Self {
      destination,
      bindings,
      pending: Vec::new(),
    }
  }

  fn forward(&mut self, complete: bool) -> std::io::Result<()> {
    while let Some((position, binding)) = self
      .bindings
      .iter()
      .enumerate()
      .filter_map(|(binding, (token, _))| {
        self
          .pending
          .windows(token.len())
          .position(|window| window == token)
          .map(|position| (position, binding))
      })
      .min_by_key(|(position, _)| *position)
    {
      let (token, replacement) = &self.bindings[binding];
      self.destination.write_all(&self.pending[..position])?;
      self.destination.write_all(replacement)?;
      self.pending.drain(..position + token.len());
    }
    let retained = if complete {
      0
    } else {
      self
        .bindings
        .iter()
        .map(|(token, _)| token.len())
        .max()
        .unwrap_or(1)
        .saturating_sub(1)
        .min(self.pending.len())
    };
    let ready = self.pending.len().saturating_sub(retained);
    self.destination.write_all(&self.pending[..ready])?;
    self.pending.drain(..ready);
    Ok(())
  }
}

impl<W: std::io::Write> std::io::Write for SourceRootRebindingWriter<W> {
  fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
    self.pending.extend_from_slice(bytes);
    self.forward(false)?;
    Ok(bytes.len())
  }

  fn flush(&mut self) -> std::io::Result<()> {
    self.forward(true)?;
    self.destination.flush()
  }
}

fn source_root_stream_bindings(source_root: &Path, capture: &NativeActionCapture) -> Vec<(Vec<u8>, Vec<u8>)> {
  let mut bindings = vec![(
    PORTABLE_SOURCE_ROOT.as_bytes().to_vec(),
    json_string_contents(&source_root_display_bytes(source_root)),
  )];
  if let Some(package) = &capture.package_binding {
    bindings.push((
      PORTABLE_PACKAGE_ROOT.as_bytes().to_vec(),
      json_string_contents(&source_root_display_bytes(&package.spelling)),
    ));
  }
  bindings
}

fn rebind_portable_source_roots(bytes: &[u8], bindings: &[(Vec<u8>, Vec<u8>)]) -> RailResult<Vec<u8>> {
  let rebound = bindings.iter().fold(bytes.to_vec(), |current, (token, replacement)| {
    replace_bytes(&current, token, replacement).0
  });
  if [PORTABLE_SOURCE_ROOT, PORTABLE_PACKAGE_ROOT]
    .into_iter()
    .any(|token| rebound.windows(token.len()).any(|window| window == token.as_bytes()))
  {
    return Err(RailError::message(
      "native compiler cached data retains an unbound source-root token",
    ));
  }
  Ok(rebound)
}

fn capture_compiler_stream<R: std::io::Read, W: std::io::Write>(
  mut source: R,
  mut destination: W,
  max_bytes: usize,
) -> CapturedCompilerStream {
  let mut storage = Some(tempfile::spooled_tempfile(STREAM_MEMORY_SPOOL_BYTES));
  let mut captured_bytes = 0usize;
  let mut limit_exceeded = false;
  let mut forwarding = true;
  let mut buffer = [0u8; 64 * 1024];
  loop {
    let read = match source.read(&mut buffer) {
      Ok(0) => break,
      Ok(read) => read,
      Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
      Err(_) => return CapturedCompilerStream::Unavailable,
    };
    if forwarding && destination.write_all(&buffer[..read]).is_err() {
      forwarding = false;
    }
    if let Some(next_bytes) = captured_bytes.checked_add(read)
      && next_bytes <= max_bytes
      && let Some(capture) = storage.as_mut()
    {
      if capture.write_all(&buffer[..read]).is_ok() {
        captured_bytes = next_bytes;
      } else {
        storage = None;
      }
    } else {
      storage = None;
      limit_exceeded = true;
    }
  }
  if forwarding {
    let _ = destination.flush();
  }
  if limit_exceeded {
    CapturedCompilerStream::LimitExceeded
  } else if let Some(storage) = storage {
    CapturedCompilerStream::Complete {
      storage,
      bytes: captured_bytes,
    }
  } else {
    CapturedCompilerStream::Unavailable
  }
}

fn publish_and_record_cold_observation(
  raw: &mut RawCompilerInvocation,
  reason: &'static str,
  action_key: Option<String>,
  result_key: Option<String>,
  bytes_hashed: u64,
  cache_bytes_read: u64,
  trace: &NativeCacheWrapperTrace,
) -> RailResult<()> {
  publish_cold_observation(raw, reason, action_key, result_key, bytes_hashed)?;
  let metadata = raw.cache_wrapper.as_ref();
  write_cache_event(
    CompilerCacheWrapperStatus::Bypassed,
    reason,
    metadata.and_then(CompilerCacheWrapperMetadata::action_key),
    metadata.and_then(CompilerCacheWrapperMetadata::result_key),
    None,
    NativeCacheMetrics {
      bytes_hashed,
      cache_bytes_read,
      cache_bytes_written: 0,
      bytes_restored: 0,
    },
    trace,
  );
  Ok(())
}

fn publish_cold_observation(
  raw: &mut RawCompilerInvocation,
  reason: &'static str,
  action_key: Option<String>,
  result_key: Option<String>,
  bytes_hashed: u64,
) -> RailResult<()> {
  let initial = raw.cache_wrapper.clone().or_else(metadata_from_environment);
  raw.cache_wrapper = Some(CompilerCacheWrapperMetadata::native(
    CompilerCacheWrapperStatus::Bypassed,
    reason,
    action_key.or_else(|| {
      initial
        .as_ref()
        .and_then(CompilerCacheWrapperMetadata::action_key)
        .map(str::to_string)
    }),
    result_key.or_else(|| {
      initial
        .as_ref()
        .and_then(CompilerCacheWrapperMetadata::result_key)
        .map(str::to_string)
    }),
    bytes_hashed,
    0,
  ));
  let directory = active_context()
    .map(|context| &context.observation_directory)
    .ok_or_else(|| RailError::message("compiler observation directory is unavailable"))?;
  crate::compiler::observation::publish_raw(directory, raw)
}

struct SelectedNativeAction {
  action_key: String,
  witness: NativeCompilerWitness,
}

struct CapturedCompilerStreams<'a> {
  stdout: &'a [u8],
  stderr: &'a [u8],
}

// These values are the complete semantic and physical boundaries of one cold
// result. Grouping them into a second context object would only hide which
// authority is consumed at admission.
#[allow(clippy::too_many_arguments)]
fn prepare_cold_result(
  session: &NativeCompilerSession,
  initial_capture: &NativeActionCapture,
  expected_base_action: &str,
  selected: SelectedNativeAction,
  observation: &RawCompilerInvocation,
  output_paths: &NativeOutputPaths,
  streams: CapturedCompilerStreams<'_>,
  source_root: &Path,
  source_root_spelling: &Path,
  staging: pack::NativeResultStaging,
) -> Result<(PreparedNativeResult, NativePublicationProof), &'static str> {
  let SelectedNativeAction {
    action_key: selected_action,
    witness,
  } = selected;
  let CapturedCompilerStreams { stdout, stderr } = streams;
  let durable_handoff = staging.requires_durable_handoff();
  let prepared: RailResult<_> = (|| {
    validated_output_parent(output_paths, source_root)?;
    let bindings = native_output_bindings(output_paths);
    // Rustc reports artifact paths with platform-specific spelling. Store one
    // canonical path token so a verified result can be late-bound to a different
    // Cargo output directory within this physical source root on restore.
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
    let portable_dep_info =
      portable_dep_info_output_bindings(&dep_info_bytes, output_paths, source_root, initial_capture)?;
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
        let mode = native_output_mode(&fs::symlink_metadata(path)?);
        Ok(NativeCompilerOutput {
          role: (*role).to_string(),
          slot: (*slot).to_string(),
          content_digest: observed.content_digest.clone(),
          bytes,
          mode,
        })
      })
      .collect::<RailResult<Vec<_>>>()?;
    let validation = NativeCompilerValidation::new(
      session,
      cache_observation,
      &initial_capture.approved_environment,
      pack::NativeResultDescriptor {
        action_key: selected_action,
        witness,
        outputs,
        stdout_digest: digest(&stdout),
        stdout_bytes: stdout.len() as u64,
        stderr_digest: digest(&stderr),
        stderr_bytes: stderr.len() as u64,
      },
    )?;
    let current_base_action = base_action_key(&session.identity, &session.class, observation, initial_capture)?;
    if current_base_action != expected_base_action {
      return Err(RailError::message(
        "cold compiler observation does not match the base action selected by the outer wrapper",
      ));
    }

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
        write_new_file(staged, &portable_dep_info, expected.mode, durable_handoff)?;
      } else {
        copy_regular_file(source, staged, expected.bytes, expected.mode, durable_handoff)?;
      }
      validate_staged_output(staged, observed, expected.bytes, expected.mode)?;
    }
    write_new_file(&stdout_slot, &stdout, 0o644, durable_handoff)?;
    write_new_file(&stderr_slot, &stderr, 0o644, durable_handoff)?;
    let slots = validation
      .cas_output_bindings()
      .chain(validation.cas_stream_bindings())
      .collect::<Vec<_>>();
    let manifest = crate::hermetic::manifest_from_verified_native_slots(&slots)?;
    Ok((staging, manifest, validation))
  })();
  let (staging, manifest, validation) = prepared.map_err(|_| "cold_result_preparation_failed")?;
  let environment_names = initial_capture
    .approved_environment
    .entries
    .iter()
    .map(|entry| entry.name.clone())
    .collect::<Vec<_>>();
  let (approved_environment, environment_bytes_hashed) = capture_approved_environment(
    source_root,
    source_root_spelling,
    initial_capture,
    &environment_names,
    Instant::now(),
  )
  .map_err(|_| "cold_final_capture_failed")?;
  if approved_environment != initial_capture.approved_environment {
    return Err("cold_inputs_changed_before_admission");
  }
  let proof = NativePublicationProof {
    version: 4,
    source_state: initial_capture.source_state.clone(),
    package_binding: initial_capture.package_binding.clone(),
    approved_environment,
    guard_identity: initial_capture
      .guard_identity()
      .map_err(|_| "cold_final_capture_failed")?,
    environment_bytes_hashed,
  };
  let prepared = PreparedNativeResult::from_verified_local_cas_staging(staging, manifest, validation);
  Ok((prepared, proof))
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

fn copy_regular_file(
  source: &Path,
  destination: &Path,
  expected_bytes: u64,
  expected_mode: u32,
  durable_handoff: bool,
) -> RailResult<()> {
  let before = fs::symlink_metadata(source)?;
  if !before.is_file()
    || crate::utils::is_symlink_or_reparse(&before)
    || !single_link(&before)
    || native_output_mode(&before) != expected_mode
  {
    return Err(RailError::message(format!(
      "compiler output '{}' is not a single-link regular file",
      source.display()
    )));
  }
  let input = File::open(source)?;
  let (output, copied) = if let Some(output) = try_clone_regular_file(&input, destination) {
    let copied = output.metadata()?.len();
    (output, copied)
  } else {
    let mut output = OpenOptions::new().write(true).create_new(true).open(destination)?;
    let copied = std::io::copy(&mut input.take(expected_bytes.saturating_add(1)), &mut output)?;
    (output, copied)
  };
  set_native_output_mode(destination, expected_mode)?;
  if durable_handoff {
    output.sync_all()?;
  }
  let after = fs::symlink_metadata(source)?;
  if copied != expected_bytes
    || before.len() != after.len()
    || before.modified()? != after.modified()?
    || native_output_mode(&after) != expected_mode
  {
    return Err(RailError::message(format!(
      "compiler output '{}' changed during cache staging",
      source.display()
    )));
  }
  Ok(())
}

/// Ask the filesystem for a copy-on-write snapshot before falling back to a
/// byte copy. The destination lives in private command staging and is still
/// hashed against the compiler observation before handoff.
#[cfg(target_vendor = "apple")]
fn try_clone_regular_file(source: &File, destination: &Path) -> Option<File> {
  let parent = File::open(destination.parent()?).ok()?;
  let name = destination.file_name()?;
  match rustix::fs::fclonefileat(
    source,
    &parent,
    name,
    rustix::fs::CloneFlags::NOFOLLOW | rustix::fs::CloneFlags::NOOWNERCOPY,
  ) {
    Ok(()) => OpenOptions::new().read(true).write(true).open(destination).ok(),
    Err(_) => {
      let _ = fs::remove_file(destination);
      None
    }
  }
}

#[cfg(all(target_os = "linux", not(any(target_arch = "sparc", target_arch = "sparc64"))))]
fn try_clone_regular_file(source: &File, destination: &Path) -> Option<File> {
  let output = OpenOptions::new()
    .read(true)
    .write(true)
    .create_new(true)
    .open(destination)
    .ok()?;
  match rustix::fs::ioctl_ficlone(&output, source) {
    Ok(()) => Some(output),
    Err(_) => {
      drop(output);
      let _ = fs::remove_file(destination);
      None
    }
  }
}

#[cfg(not(any(
  target_vendor = "apple",
  all(target_os = "linux", not(any(target_arch = "sparc", target_arch = "sparc64")))
)))]
fn try_clone_regular_file(_source: &File, _destination: &Path) -> Option<File> {
  None
}

fn validate_staged_output(
  path: &Path,
  expected: &FileObservation,
  expected_bytes: u64,
  expected_mode: u32,
) -> RailResult<()> {
  let metadata = fs::symlink_metadata(path)?;
  let staged = FileObservation::capture(path, path.parent().unwrap_or(Path::new("/")), Path::new("/"))?;
  if metadata.len() != expected_bytes
    || native_output_mode(&metadata) != expected_mode
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

fn write_new_file(path: &Path, bytes: &[u8], mode: u32, durable_handoff: bool) -> RailResult<()> {
  let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
  file.write_all(bytes)?;
  set_native_output_mode(path, mode)?;
  if durable_handoff {
    file.sync_all()?;
  }
  Ok(())
}

#[cfg(unix)]
fn native_output_mode(metadata: &fs::Metadata) -> u32 {
  use std::os::unix::fs::PermissionsExt as _;

  metadata.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn native_output_mode(metadata: &fs::Metadata) -> u32 {
  if metadata.permissions().readonly() {
    0o444
  } else {
    0o644
  }
}

#[cfg(unix)]
fn set_native_output_mode(path: &Path, mode: u32) -> RailResult<()> {
  use std::os::unix::fs::PermissionsExt as _;

  if !valid_native_output_mode(mode) {
    return Err(RailError::message("native compiler output mode is unsupported"));
  }
  fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
  Ok(())
}

#[cfg(not(unix))]
fn set_native_output_mode(path: &Path, mode: u32) -> RailResult<()> {
  if !valid_native_output_mode(mode) {
    return Err(RailError::message("native compiler output mode is unsupported"));
  }
  let mut permissions = fs::metadata(path)?.permissions();
  permissions.set_readonly(mode == 0o444);
  fs::set_permissions(path, permissions)?;
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
  action_key: Option<&'a str>,
  result_key: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  base_action_key: Option<&'a str>,
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
  action_key: Option<String>,
  result_key: Option<String>,
  base_action_key: Option<String>,
  bytes_hashed: u64,
  cache_bytes_read: u64,
  cache_bytes_written: u64,
  bytes_restored: u64,
  wrapper_trace: Option<NativeCacheWrapperTraceSnapshot>,
}

fn write_cache_event(
  status: CompilerCacheWrapperStatus,
  reason: &str,
  action_key: Option<&str>,
  result_key: Option<&str>,
  remote_base_action_key: Option<&str>,
  metrics: NativeCacheMetrics,
  trace: &NativeCacheWrapperTrace,
) {
  let Some(directory) = active_context().map(|context| context.observation_directory.join("native-cache-events"))
  else {
    return;
  };
  write_cache_event_at(
    &directory,
    status,
    reason,
    action_key,
    result_key,
    remote_base_action_key,
    metrics,
    trace.snapshot(),
  );
}

#[allow(clippy::too_many_arguments)]
fn write_cache_event_at(
  directory: &Path,
  status: CompilerCacheWrapperStatus,
  reason: &str,
  action_key: Option<&str>,
  result_key: Option<&str>,
  remote_base_action_key: Option<&str>,
  metrics: NativeCacheMetrics,
  wrapper_trace: Option<NativeCacheWrapperTraceSnapshot>,
) {
  if fs::create_dir_all(directory).is_err() {
    return;
  }
  let event = NativeCacheEvent {
    version: NATIVE_CACHE_RUN_EVENT_VERSION,
    status,
    reason,
    action_key,
    result_key,
    base_action_key: remote_base_action_key,
    bytes_hashed: metrics.bytes_hashed,
    cache_bytes_read: metrics.cache_bytes_read,
    cache_bytes_written: metrics.cache_bytes_written,
    bytes_restored: metrics.bytes_restored,
    wrapper_trace,
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
  write_unique_cache_event(directory, &stem, &bytes);
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

pub(crate) fn validate_action_key(value: &str) -> RailResult<()> {
  validate_identity(value, ACTION_KEY_PREFIX).map(|_| ())
}

pub(crate) fn validate_base_action_key(value: &str) -> RailResult<()> {
  validate_identity(value, BASE_ACTION_KEY_PREFIX).map(|_| ())
}

pub(crate) fn validate_result_key(value: &str) -> RailResult<()> {
  validate_identity(value, RESULT_KEY_PREFIX).map(|_| ())
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

  #[cfg(unix)]
  #[test]
  fn command_context_is_owner_only_at_creation() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = private_command_directory().expect("private command directory");
    assert_eq!(
      directory
        .path()
        .metadata()
        .expect("directory metadata")
        .permissions()
        .mode()
        & 0o077,
      0
    );

    let context = directory.path().join("context.json");
    write_private_command_file(&context, b"private").expect("private context");
    assert_eq!(
      context.metadata().expect("context metadata").permissions().mode() & 0o077,
      0
    );
  }

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
        "--error-format=json",
        "src/lib.rs",
        "--crate-type",
        "lib",
        "--emit=dep-info,metadata",
        "-C",
        "metadata=0123456789abcdef",
        "-Cextra-filename=-0123456789abcdef",
        "--out-dir",
        "target/debug/deps",
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
    };
    let capability_identity = digest(b"toolchain-capability");
    let compiler_process_environment_identity = digest(b"compiler-process-environment");
    let execution_contract = DIAGNOSTIC_EXECUTION_CONTRACT.to_string();
    let identity = session_identity(
      &class,
      &capability_identity,
      &compiler_process_environment_identity,
      &execution_contract,
      NativeSessionAuthority::Exact,
    )
    .expect("session identity");
    NativeCompilerSession {
      version: NATIVE_COMPILER_SESSION_VERSION,
      identity,
      source_root_identity,
      class,
      capability_identity,
      compiler_process_environment_identity,
      execution_contract,
      authority: NativeSessionAuthority::Exact,
    }
  }

  fn synthetic_capture(observation: &RawCompilerInvocation) -> NativeActionCapture {
    let declared = observation.declared_inputs.first().expect("declared source");
    let file_name = match &declared.path {
      ObservationPath::Repository(path) | ObservationPath::Host(path) => Path::new(path)
        .file_name()
        .and_then(OsStr::to_str)
        .expect("source file name")
        .to_string(),
    };
    let mut environment = observation
      .environment_reads
      .iter()
      .map(|entry| ApprovedEnvEntry {
        name: entry.name.clone(),
        value_digest: entry.value_digest.clone(),
        root_mapped: false,
      })
      .collect::<Vec<_>>();
    environment.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    NativeActionCapture {
      source_root: PathBuf::from("/workspace/src"),
      source_root_spelling: PathBuf::from("/workspace/src"),
      crate_root: file_name.clone(),
      package_binding: None,
      source_state: NativeSourceState {
        version: 1,
        root: ObservationPath::Repository("src".to_string()),
        entries: vec![
          NativeSourceEntry {
            path: String::new(),
            kind: NativeSourceEntryKind::Directory { mode: 0o755 },
          },
          NativeSourceEntry {
            path: file_name,
            kind: NativeSourceEntryKind::RegularFile {
              bytes: 1,
              content_digest: declared.content_digest.clone(),
              mode: 0o644,
            },
          },
        ],
      },
      approved_environment: ApprovedEnvState {
        version: 3,
        entries: environment,
      },
      guard: NativeCaptureGuard { entries: Vec::new() },
      bytes_hashed: 0,
    }
  }

  fn synthetic_witness(observation: &RawCompilerInvocation) -> NativeCompilerWitness {
    let mut dependency_names = observation
      .dependency_artifacts
      .iter()
      .map(|(name, _)| name.clone())
      .collect::<Vec<_>>();
    dependency_names.sort_unstable();
    NativeCompilerWitness {
      version: 1,
      complete: true,
      source_paths: vec!["lib.rs".to_string()],
      dependency_names,
      environment_names: observation
        .environment_reads
        .iter()
        .map(|entry| entry.name.clone())
        .collect(),
    }
  }

  fn graduated_validation_with_streams(
    observation: RawCompilerInvocation,
    stdout: &[u8],
    stderr: &[u8],
  ) -> NativeCompilerValidation {
    let session = graduated_session(digest(b"source-root"));
    let capture = synthetic_capture(&observation);
    let action = action_key(&session.identity, &session.class, &observation, &capture).expect("action");
    let witness = synthetic_witness(&observation);
    let outputs = vec![
      NativeCompilerOutput {
        role: "dep_info".to_string(),
        slot: DEP_INFO_SLOT.to_string(),
        content_digest: observation.emitted_outputs[0].content_digest.clone(),
        bytes: 8,
        mode: 0o644,
      },
      NativeCompilerOutput {
        role: "metadata".to_string(),
        slot: METADATA_SLOT.to_string(),
        content_digest: observation.emitted_outputs[1].content_digest.clone(),
        bytes: 8,
        mode: 0o644,
      },
    ];
    NativeCompilerValidation::new(
      &session,
      observation,
      &capture.approved_environment,
      pack::NativeResultDescriptor {
        action_key: action,
        witness,
        outputs,
        stdout_digest: digest(stdout),
        stdout_bytes: stdout.len() as u64,
        stderr_digest: digest(stderr),
        stderr_bytes: stderr.len() as u64,
      },
    )
    .expect("graduated validation")
  }

  fn graduated_validation(observation: RawCompilerInvocation) -> NativeCompilerValidation {
    graduated_validation_with_streams(observation, b"", b"")
  }

  pub(crate) fn cas_validation_with_stdout(stdout: &[u8]) -> NativeCompilerValidation {
    graduated_validation_with_streams(graduated_observation(), stdout, b"")
  }

  pub(crate) fn cas_validation_with_base_action(stdout: &[u8]) -> (NativeCompilerValidation, String) {
    let observation = graduated_observation();
    let session = graduated_session(digest(b"source-root"));
    let capture = synthetic_capture(&observation);
    let base_action =
      base_action_key(&session.identity, &session.class, &observation, &capture).expect("fixture base action");
    (graduated_validation_with_streams(observation, stdout, b""), base_action)
  }

  #[test]
  fn remote_publication_requires_the_base_bound_by_the_action() {
    let (validation, base_action) = cas_validation_with_base_action(b"portable stdout");
    assert!(
      validation
        .remote_publication_environment_names(&base_action)
        .expect("bound base action")
        .is_empty()
    );

    let unrelated_base = sha256_identity(
      BASE_ACTION_KEY_PREFIX,
      b"cargo-rail-native-unrelated-base-action\0",
      &[],
    );
    validation
      .remote_publication_environment_names(&unrelated_base)
      .expect_err("an unrelated base action must not authorize remote publication");
  }

  #[test]
  fn remote_publication_revalidates_the_complete_compiler_environment() {
    let mut observation = graduated_observation();
    observation.environment_reads.insert(EnvironmentObservation {
      name: "CARGO_INCREMENTAL".to_string(),
      value_digest: Some(digest(b"0")),
      secret_capability: false,
    });
    let validation = graduated_validation(observation);

    assert!(validation.remote_environment_is_approved(&["CARGO_INCREMENTAL".to_string()]));
    assert!(!validation.remote_environment_is_approved(&[]));

    let mut malformed = validation;
    malformed.compiler_environment_names = vec!["Z".to_string(), "A".to_string()];
    malformed
      .validate_object()
      .expect_err("compiler environment authority must remain canonical");
  }

  #[test]
  fn native_validation_requires_one_exact_environment_name_set() {
    let mut observation = graduated_observation();
    observation.environment_reads.insert(EnvironmentObservation {
      name: "CARGO_INCREMENTAL".to_string(),
      value_digest: Some(digest(b"0")),
      secret_capability: false,
    });
    let validation = graduated_validation(observation);
    validation.validate_object().expect("matching environment authority");

    let mut missing_selector_name = validation.clone();
    missing_selector_name.compiler_environment_names.clear();
    missing_selector_name
      .validate_object()
      .expect_err("the compiler selector cannot omit an observed environment name");

    let mut missing_witness_name = validation.clone();
    missing_witness_name.witness.environment_names.clear();
    missing_witness_name.result_key = result_key(
      &missing_witness_name.action_key,
      &missing_witness_name.witness,
      &missing_witness_name.outputs,
      &missing_witness_name.stdout_digest,
      missing_witness_name.stdout_bytes,
      &missing_witness_name.stderr_digest,
      missing_witness_name.stderr_bytes,
    )
    .expect("mutated result identity");
    missing_witness_name
      .validate_object()
      .expect_err("the witness cannot omit an observed environment name");

    let mut missing_observation_name = validation;
    missing_observation_name.observation.environment_reads.clear();
    missing_observation_name
      .validate_object()
      .expect_err("the observation cannot omit an authoritative environment name");
  }

  #[test]
  fn empty_environment_selectors_are_authoritative() {
    for (revision, first, second) in [
      (1_u8, Vec::new(), vec!["CARGO_INCREMENTAL".to_string()]),
      (2_u8, vec!["CARGO_INCREMENTAL".to_string()], Vec::new()),
    ] {
      let cache = tempfile::tempdir().expect("cache base");
      let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
      let key = sha256_identity(
        BASE_ACTION_KEY_PREFIX,
        b"cargo-rail-native-empty-selector-test\0",
        &[(b"revision", &[revision])],
      );

      assert_eq!(cas.native_environment_selector(&key).expect("missing selector"), None);
      assert_eq!(
        cas
          .publish_native_environment_selector(&key, &first)
          .expect("initial selector publication"),
        crate::hermetic::cas::NativeEnvironmentSelectorPublication::Created
      );
      assert_eq!(
        cas.native_environment_selector(&key).expect("published selector"),
        Some(first)
      );
      assert_eq!(
        cas
          .publish_native_environment_selector(&key, &second)
          .expect("divergent selector publication"),
        crate::hermetic::cas::NativeEnvironmentSelectorPublication::Diverged
      );
      cas
        .native_environment_selector(&key)
        .expect_err("an empty/nonempty selector change must fail closed");
    }
  }

  pub(crate) fn prepared_cas_fixture(validation: NativeCompilerValidation) -> PreparedNativeResult {
    let staging = tempfile::tempdir().expect("native result staging");
    for (slot, bytes) in [
      (DEP_INFO_SLOT, b"dep-info".as_slice()),
      (METADATA_SLOT, b"metadata".as_slice()),
      (STDOUT_SLOT, b"portable stdout".as_slice()),
      (STDERR_SLOT, b"".as_slice()),
    ] {
      let path = staging.path().join(slot);
      fs::create_dir_all(path.parent().expect("slot parent")).expect("slot directory");
      fs::write(path, bytes).expect("slot bytes");
    }
    let paths = [DEP_INFO_SLOT, METADATA_SLOT, STDOUT_SLOT, STDERR_SLOT]
      .into_iter()
      .map(|slot| staging.path().join(slot))
      .collect::<Vec<_>>();
    let manifest =
      crate::hermetic::capture_native_compiler_outputs(staging.path(), &paths).expect("native result manifest");
    PreparedNativeResult::from_verified_staging(staging, manifest, validation)
  }

  #[test]
  fn revalidated_store_runs_selector_publication_before_action_commit() {
    let cache = tempfile::tempdir().expect("cache base");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let validation = graduated_validation_with_streams(graduated_observation(), b"portable stdout", b"");
    let action = validation.action_key().to_string();
    let base_action = sha256_identity(BASE_ACTION_KEY_PREFIX, b"cargo-rail-native-selector-order-test\0", &[]);
    let mut revalidated = false;

    cas
      .store_native_revalidated(prepared_cas_fixture(validation), |_| {
        assert!(matches!(
          cas.native_action(&action).expect("pre-commit action lookup"),
          crate::hermetic::cas::NativeActionLookup::Miss(_)
        ));
        assert_eq!(
          cas
            .native_environment_selector(&base_action)
            .expect("pre-publication selector lookup"),
          None
        );
        assert_eq!(
          cas
            .publish_native_environment_selector(&base_action, &[])
            .expect("empty selector publication"),
          crate::hermetic::cas::NativeEnvironmentSelectorPublication::Created
        );
        revalidated = true;
        Ok(())
      })
      .expect("revalidated native admission");

    assert!(revalidated);
    assert_eq!(
      cas
        .native_environment_selector(&base_action)
        .expect("committed selector lookup"),
      Some(Vec::new())
    );
    assert!(matches!(
      cas.native_action(&action).expect("committed action lookup"),
      crate::hermetic::cas::NativeActionLookup::Hit(_)
    ));
  }

  #[test]
  fn aborted_revalidated_store_leaves_only_safe_selector_authority() {
    let cache = tempfile::tempdir().expect("cache base");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let validation = graduated_validation_with_streams(graduated_observation(), b"portable stdout", b"");
    let action = validation.action_key().to_string();
    let base_action = sha256_identity(BASE_ACTION_KEY_PREFIX, b"cargo-rail-native-selector-abort-test\0", &[]);

    let error = cas
      .store_native_revalidated(prepared_cas_fixture(validation), |_| {
        assert_eq!(
          cas
            .publish_native_environment_selector(&base_action, &[])
            .expect("empty selector publication"),
          crate::hermetic::cas::NativeEnvironmentSelectorPublication::Created
        );
        Err(RailError::message("stop before action authority"))
      })
      .expect_err("failed final revalidation must abort admission");

    assert!(error.to_string().contains("stop before action authority"), "{error}");
    assert_eq!(
      cas
        .native_environment_selector(&base_action)
        .expect("selector-only state"),
      Some(Vec::new())
    );
    assert!(matches!(
      cas.native_action(&action).expect("aborted action lookup"),
      crate::hermetic::cas::NativeActionLookup::Miss(_)
    ));
  }

  #[test]
  fn restore_rejects_absent_mismatched_and_conflicted_selectors() {
    let validate = |first: Option<Vec<String>>, second: Option<Vec<String>>| -> Result<(), RestorePublishFailure> {
      let cache = tempfile::tempdir().expect("cache base");
      let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("local CAS");
      let observation = graduated_observation();
      let capture = synthetic_capture(&observation);
      let session = graduated_session(digest(b"source-root"));
      let base_action =
        base_action_key(&session.identity, &session.class, &observation, &capture).expect("base action");
      let validation = graduated_validation_with_streams(observation.clone(), b"portable stdout", b"");
      let action = validation.action_key().to_string();
      cas
        .store_native(prepared_cas_fixture(validation))
        .expect("native result admission");
      if let Some(first) = first {
        cas
          .publish_native_environment_selector(&base_action, &first)
          .expect("first selector publication");
      }
      if let Some(second) = second {
        assert_eq!(
          cas
            .publish_native_environment_selector(&base_action, &second)
            .expect("second selector publication"),
          crate::hermetic::cas::NativeEnvironmentSelectorPublication::Diverged
        );
      }
      let crate::hermetic::cas::NativeActionLookup::Hit(hit) =
        cas.native_action(&action).expect("native action lookup")
      else {
        panic!("stored native action must be authoritative");
      };
      validate_restore_environment_authority(&hit, &capture, &observation).map(|_| ())
    };

    assert!(
      validate(Some(Vec::new()), None).is_ok(),
      "matching selector must retain restore authority"
    );
    for (result, expected) in [
      (validate(None, None), "absent"),
      (validate(Some(vec!["P73_OTHER".to_string()]), None), "does not match"),
      (
        validate(Some(Vec::new()), Some(vec!["P73_OTHER".to_string()])),
        "durably conflicted",
      ),
    ] {
      let Err(RestorePublishFailure::Operational(error)) = result else {
        panic!("invalid selector authority must reject restore operationally");
      };
      assert!(error.to_string().contains(expected), "{error}");
    }
  }

  #[test]
  fn discovery_results_require_exact_session_rebinding_before_cas_authority() {
    let source_root = tempfile::tempdir().expect("source root");
    fs::create_dir(source_root.path().join("src")).expect("source directory");
    fs::write(source_root.path().join("src/lib.rs"), b"pub fn value() -> u8 { 1 }\n").expect("source file");
    let rustc = "rustc 1.97.1 (test)\nhost: aarch64-apple-darwin\n";
    let environment = digest(b"compiler-process-environment");
    let discovery = NativeCompilerSession::capture_discovery(
      source_root.path(),
      &digest(b"discovery-capability"),
      &environment,
      DIRECT_EXECUTION_CONTRACT,
    )
    .expect("discovery session");
    let exact = NativeCompilerSession::capture(
      source_root.path(),
      rustc,
      &digest(b"exact-capability"),
      &environment,
      DIRECT_EXECUTION_CONTRACT,
      NativeSessionAuthority::Exact,
    )
    .expect("exact session");
    assert_ne!(discovery.class, exact.class);
    let mut observation = graduated_observation();
    let capture = NativeActionCapture::capture(&observation, source_root.path()).expect("action capture");
    let discovery_action =
      action_key(&discovery.identity, &discovery.class, &observation, &capture).expect("discovery action");
    observation.cache_wrapper = Some(CompilerCacheWrapperMetadata::native(
      CompilerCacheWrapperStatus::Miss,
      "empty_local_authority",
      Some(discovery_action.clone()),
      None,
      capture.bytes_hashed,
      0,
    ));
    let validation = NativeCompilerValidation::new(
      &discovery,
      observation.clone(),
      &capture.approved_environment,
      pack::NativeResultDescriptor {
        action_key: discovery_action.clone(),
        witness: capture
          .witness(&observation, source_root.path())
          .expect("compiler witness"),
        outputs: vec![
          NativeCompilerOutput {
            role: "dep_info".to_string(),
            slot: DEP_INFO_SLOT.to_string(),
            content_digest: digest(b"dep-info"),
            bytes: 8,
            mode: 0o644,
          },
          NativeCompilerOutput {
            role: "metadata".to_string(),
            slot: METADATA_SLOT.to_string(),
            content_digest: digest(b"metadata"),
            bytes: 8,
            mode: 0o644,
          },
        ],
        stdout_digest: digest(b"portable stdout"),
        stdout_bytes: 15,
        stderr_digest: digest(b""),
        stderr_bytes: 0,
      },
    )
    .expect("discovery validation");
    let proof = NativePublicationProof {
      version: 4,
      source_state: capture.source_state.clone(),
      package_binding: capture.package_binding.clone(),
      approved_environment: capture.approved_environment.clone(),
      guard_identity: capture.guard_identity().expect("guard identity"),
      environment_bytes_hashed: 0,
    };

    let cas_base = tempfile::tempdir().expect("CAS base");
    let cas = LocalCas::open_at(cas_base.path(), 1024 * 1024).expect("local CAS");
    cas
      .store_native(prepared_cas_fixture(validation.clone()))
      .expect_err("discovery validation must never become authoritative");
    assert!(matches!(
      cas.native_action(&discovery_action).expect("discovery lookup"),
      crate::hermetic::cas::NativeActionLookup::Miss(_)
    ));

    let rebound = validation
      .rebind_discovery_session(&discovery, &exact, &proof, source_root.path())
      .expect("exact session rebinding");
    assert!(rebound.is_authoritative());
    assert_ne!(rebound.action_key(), discovery_action);

    let mut forged = proof;
    let NativeSourceEntryKind::RegularFile { content_digest, .. } = &mut forged.source_state.entries[1].kind else {
      panic!("source fixture lost its regular file");
    };
    *content_digest = digest(b"forged source");
    validation
      .rebind_discovery_session(&discovery, &exact, &forged, source_root.path())
      .expect_err("forged discovery key material must fail closed");
  }

  #[test]
  fn fixed_pack_moves_one_result_between_independent_authority_roots() {
    let first_base = tempfile::tempdir().expect("first CAS base");
    let second_base = tempfile::tempdir().expect("second CAS base");
    let first = LocalCas::open_at(first_base.path(), 1024 * 1024).expect("first CAS");
    let validation = graduated_validation_with_streams(graduated_observation(), b"portable stdout", b"");
    let action = validation.action_key().to_string();
    let result = validation.result_key().to_string();
    first
      .store_native(prepared_cas_fixture(validation))
      .expect("local admission");
    let crate::hermetic::cas::NativeActionLookup::Hit(hit) = first.native_action(&action).expect("local lookup") else {
      panic!("locally admitted result should be authoritative");
    };
    let mut pack_bytes = Vec::new();
    let export = hit.export_pack(&mut pack_bytes).expect("fixed pack export");
    assert_eq!(export.content_length, pack_bytes.len() as u64);
    assert_eq!(export.bytes_written, pack_bytes.len() as u64);
    drop(hit);

    let decoded = pack::decode(
      pack_bytes.as_slice(),
      &action,
      &result,
      Some(pack_bytes.len() as u64),
      None,
    )
    .expect("fixed pack import");
    let observation = graduated_observation();
    let capture = synthetic_capture(&observation);
    let session = graduated_session(digest(b"source-root"));
    let outputs = NativeOutputPaths {
      dep_info: PathBuf::from("/workspace/target/debug/deps/fixture-0123456789abcdef.d"),
      metadata: PathBuf::from("/workspace/target/debug/deps/libfixture-0123456789abcdef.rmeta"),
      rlib: None,
    };
    let authority = RemoteAuthorityId::for_test("fixed-pack-independent-authority").expect("remote authority");
    let (prepared, bytes_read) = prepare_authenticated_native_pack(
      decoded,
      authority.clone(),
      &session,
      &capture,
      &observation,
      &outputs,
      Path::new("/workspace"),
    )
    .expect("live pack binding");
    assert_eq!(bytes_read, pack_bytes.len() as u64);
    let second = LocalCas::open_at(second_base.path(), 1024 * 1024).expect("second CAS");
    let (imported, _) = second.store_native(prepared).expect("remote admission");
    assert_eq!(imported.action_key(), action);
    assert_eq!(imported.result_key(), result);

    assert!(matches!(
      second.native_action(&action).expect("unscoped lookup"),
      crate::hermetic::cas::NativeActionLookup::Miss(_)
    ));
    let crate::hermetic::cas::NativeActionLookup::Hit(imported_hit) = second
      .native_action_for_authority(&action, Some(&authority))
      .expect("accepted remote lookup")
    else {
      panic!("accepted remote origin should authorize the imported result");
    };
    let mut second_pack = Vec::new();
    imported_hit
      .export_pack(&mut second_pack)
      .expect("re-export imported result");
    assert_eq!(
      second_pack, pack_bytes,
      "pack bytes must not depend on private CAS layout"
    );
    let restore_parent = tempfile::tempdir().expect("restore parent");
    let restored = restore_parent.path().join("restored");
    assert!(matches!(
      imported_hit.restore(&restored),
      crate::hermetic::cas::NativeCacheLookup::Hit(_)
    ));
    assert_eq!(fs::read(restored.join(DEP_INFO_SLOT)).expect("dep-info"), b"dep-info");
    assert_eq!(fs::read(restored.join(METADATA_SLOT)).expect("metadata"), b"metadata");
    assert_eq!(
      fs::read(restored.join(STDOUT_SLOT)).expect("stdout"),
      b"portable stdout"
    );
    let association = imported_hit.association().expect("imported association");
    let wrong_action = format!("{ACTION_KEY_PREFIX}{}", "0".repeat(64));
    assert!(pack::decode_association(association.bytes(), &wrong_action, &result).is_err());
    drop(imported_hit);

    let repeated = pack::decode(
      pack_bytes.as_slice(),
      &action,
      &result,
      Some(pack_bytes.len() as u64),
      Some(second.native_result_staging().expect("guarded import staging")),
    )
    .expect("repeated fixed pack import");
    let (prepared, _) = prepare_authenticated_native_pack(
      repeated,
      authority.clone(),
      &session,
      &capture,
      &observation,
      &outputs,
      Path::new("/workspace"),
    )
    .expect("repeated live pack binding");
    let (_, repeated_stats) = second.store_native(prepared).expect("idempotent remote admission");
    assert_eq!(repeated_stats.bytes_written, 0, "existing semantic bytes must converge");
    assert!(matches!(
      second
        .native_action_for_authority(&action, Some(&authority))
        .expect("repeated accepted lookup"),
      crate::hermetic::cas::NativeActionLookup::Hit(_)
    ));

    let mut corrupt = pack_bytes.clone();
    let middle = corrupt.len() / 2;
    corrupt[middle] ^= 0x80;
    assert!(pack::decode(corrupt.as_slice(), &action, &result, Some(corrupt.len() as u64), None).is_err());
    assert!(
      pack::decode(
        &pack_bytes[..pack_bytes.len() - 1],
        &action,
        &result,
        Some(pack_bytes.len() as u64 - 1),
        None,
      )
      .is_err()
    );
    assert!(
      pack::decode(
        pack_bytes.as_slice(),
        &action,
        &result,
        Some(pack_bytes.len() as u64 + 1),
        None,
      )
      .is_err()
    );
  }

  #[test]
  fn action_binds_complete_state_while_result_binds_selected_witness() {
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

    let capture = synthetic_capture(&base);
    let action = action_key(&session.identity, &session.class, &base, &capture).expect("action");
    assert_ne!(
      action,
      action_key(
        &session.identity,
        &session.class,
        &environment_changed,
        &synthetic_capture(&environment_changed),
      )
      .expect("environment action")
    );
    assert_eq!(
      action,
      action_key(&session.identity, &session.class, &observed_changed, &capture).expect("selection-neutral action"),
      "post-execution selection is not a pre-executable action input"
    );
    let base_validation = graduated_validation(base);
    let mut changed_witness = base_validation.witness.clone();
    changed_witness.environment_names.push("P73_SELECTED".to_string());
    let changed_result = result_key(
      &base_validation.action_key,
      &changed_witness,
      &base_validation.outputs,
      &base_validation.stdout_digest,
      base_validation.stdout_bytes,
      &base_validation.stderr_digest,
      base_validation.stderr_bytes,
    )
    .expect("changed result");
    assert_ne!(base_validation.result_key, changed_result);
  }

  #[test]
  fn complete_action_capture_hashes_bytes_instead_of_trusting_size() {
    let root = tempfile::tempdir().expect("source root");
    fs::create_dir(root.path().join("src")).expect("source directory");
    let source = root.path().join("src/lib.rs");
    fs::write(&source, b"pub const VALUE: u8 = 1;\n").expect("source");
    let captured = FileObservation::capture(&source, root.path(), root.path()).expect("source observation");
    let mut observation = graduated_observation();
    observation.declared_inputs = vec![captured.clone()];
    observation.observed_reads = vec![captured];
    let session = graduated_session(path_identity(root.path()).expect("root identity"));
    let initial = NativeActionCapture::capture(&observation, root.path()).expect("initial capture");
    let initial_action = action_key(&session.identity, &session.class, &observation, &initial).expect("initial action");

    fs::write(&source, b"pub const VALUE: u8 = 2;\n").expect("same-size mutation");
    let changed = FileObservation::capture(&source, root.path(), root.path()).expect("changed source observation");
    let mut current = observation;
    current.declared_inputs = vec![changed];
    let recaptured = NativeActionCapture::capture(&current, root.path()).expect("changed capture");
    assert!(!recaptured.unchanged_from(&initial));
    assert_ne!(
      action_key(&session.identity, &session.class, &current, &recaptured).expect("changed action"),
      initial_action
    );
  }

  #[test]
  fn source_capture_limits_accept_the_exact_boundary_and_reject_one_more() {
    let limits = NativeCaptureLimits {
      entries: 1,
      depth: 2,
      path_bytes: 3,
      bytes_hashed: 4,
      elapsed: Duration::from_nanos(5),
    };

    let mut entries = NativeCaptureBudget::new(limits);
    entries.account_entry("abc").expect("exact entry and path limits");
    assert!(entries.account_entry("").is_err(), "entry limit +1 must fail");

    let mut paths = NativeCaptureBudget::new(limits);
    assert!(paths.account_entry("abcd").is_err(), "path-byte limit +1 must fail");

    let mut bytes = NativeCaptureBudget::new(limits);
    bytes.account_hashed_bytes(4).expect("exact byte limit");
    assert!(bytes.account_hashed_bytes(1).is_err(), "byte limit +1 must fail");

    let checks = NativeCaptureBudget::new(limits);
    checks
      .check(2, Duration::from_nanos(5))
      .expect("exact depth and time limits");
    assert!(
      checks.check(3, Duration::from_nanos(5)).is_err(),
      "depth limit +1 must fail"
    );
    assert!(
      checks.check(2, Duration::from_nanos(6)).is_err(),
      "time limit +1 must fail"
    );
  }

  #[cfg(debug_assertions)]
  #[test]
  fn test_capture_limit_override_is_one_strict_bounded_profile() {
    for (name, expected) in [
      ("entries", TestCaptureLimit::Entries),
      ("depth", TestCaptureLimit::Depth),
      ("path_bytes", TestCaptureLimit::PathBytes),
      ("bytes_hashed", TestCaptureLimit::BytesHashed),
      ("elapsed", TestCaptureLimit::Elapsed),
    ] {
      assert_eq!(
        parse_test_capture_limit(&format!("wrapper_app/{name}")).expect("canonical test limit"),
        ("wrapper_app", expected)
      );
    }
    for malformed in [
      "",
      "wrapper_app",
      "/entries",
      "wrapper-app/entries",
      "wrapper_app/Entries",
      "wrapper_app/entries/extra",
      "wrapper_app/unknown",
    ] {
      assert!(
        parse_test_capture_limit(malformed).is_err(),
        "accepted malformed test capture limit {malformed:?}"
      );
    }
    assert!(
      parse_test_capture_limit(&format!("{}/entries", "a".repeat(MAX_TEST_CAPTURE_LIMIT_BYTES))).is_err(),
      "accepted an unbounded test capture limit"
    );
  }

  #[test]
  fn bounded_directory_collection_stops_consuming_at_the_first_limit_failure() {
    struct Children {
      consumed: usize,
    }

    impl Iterator for Children {
      type Item = std::io::Result<OsString>;

      fn next(&mut self) -> Option<Self::Item> {
        self.consumed += 1;
        match self.consumed {
          1 => Some(Ok(OsString::from("first"))),
          2 => Some(Ok(OsString::from("over-limit"))),
          _ => panic!("directory iterator was consumed after its entry bound failed"),
        }
      }
    }

    let limits = NativeCaptureLimits {
      entries: 2,
      depth: 1,
      path_bytes: usize::MAX,
      bytes_hashed: u64::MAX,
      elapsed: Duration::from_secs(1),
    };
    let mut budget = NativeCaptureBudget::new(limits);
    budget.account_entry("").expect("root entry");
    let error =
      collect_native_directory_children(Children { consumed: 0 }, Path::new(""), 1, Instant::now(), &mut budget)
        .expect_err("second child must exceed the entry bound");
    assert!(error.to_string().contains("entry bound exceeded"), "{error}");
  }

  #[test]
  fn bounded_directory_collection_checks_elapsed_after_exhaustion() {
    let limits = NativeCaptureLimits {
      entries: usize::MAX,
      depth: usize::MAX,
      path_bytes: usize::MAX,
      bytes_hashed: u64::MAX,
      elapsed: Duration::ZERO,
    };
    let mut budget = NativeCaptureBudget::new(limits);
    let started = Instant::now()
      .checked_sub(Duration::from_secs(1))
      .expect("monotonic clock supports a one-second lookback");
    let error = collect_native_directory_children(
      std::iter::empty::<std::io::Result<OsString>>(),
      Path::new(""),
      1,
      started,
      &mut budget,
    )
    .expect_err("elapsed capture must fail after the final directory entry");
    assert!(error.to_string().contains("time bound exceeded"), "{error}");
  }

  #[cfg(debug_assertions)]
  #[test]
  fn capture_pause_controls_are_private_compiler_capabilities() {
    let controls = [
      RESTORE_FAULT_ENV,
      RESTORE_ABORT_ENV,
      RESTORE_CANCEL_ENV,
      RESTORE_CRATE_ENV,
      TEST_CAPTURE_LIMIT_ENV,
      CAPTURE_PAUSE_PHASE_ENV,
      CAPTURE_PAUSE_CRATE_ENV,
      CAPTURE_PAUSE_DIRECTORY_ENV,
    ];
    for control in controls {
      assert!(private_compiler_environment(OsStr::new(control)));
    }

    let mut command = Command::new("rustc");
    for control in controls {
      command.env(control, "must-not-reach-rustc");
    }
    remove_private_environment(&mut command);
    for control in controls {
      assert!(
        command
          .get_envs()
          .any(|(name, value)| name == OsStr::new(control) && value.is_none()),
        "{control} was not removed from the compiler child"
      );
    }
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

    let initial = NativeActionCapture::capture(&observation, root.path()).expect("stable inputs");

    fs::write(&dependency, b"dependency-two").expect("same-size dependency mutation");
    assert!(
      NativeActionCapture::capture(&observation, root.path()).is_err(),
      "a dependency that changed since observation must prevent publication"
    );
    assert_eq!(
      initial.guard.entries.len(),
      3,
      "the source namespace and dependency must all be guarded"
    );
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
    assert_bypass("compiler_diagnostic_format_not_graduated", |value| {
      *value
        .compiler_arguments
        .iter_mut()
        .find(|argument| argument.starts_with("--error-format="))
        .expect("diagnostic format") = "--error-format=human".to_string();
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
        .push("--remap-path-prefix=/workspace=/other".to_string());
    });
    assert_bypass("compiler_flag_not_graduated", |value| {
      value.compiler_arguments.push("--remap-path-scope=all".to_string());
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

  #[cfg(any(unix, windows))]
  #[test]
  fn restore_commit_moves_the_verified_cas_copy_without_a_second_hash_or_copy() {
    let root = tempfile::tempdir().expect("restore root");
    let staging = root.path().join("private-staging");
    let output = root.path().join("target/debug/deps");
    fs::create_dir_all(&staging).expect("staging directory");
    fs::create_dir_all(&output).expect("output directory");
    let source = staging.join("libfixture.rmeta");
    let destination = output.join("libfixture.rmeta");
    fs::write(&source, b"verified metadata").expect("verified CAS copy");
    let expected = NativeCompilerOutput {
      role: "metadata".to_string(),
      slot: METADATA_SLOT.to_string(),
      content_digest: digest(b"verified metadata"),
      bytes: 17,
      mode: 0o644,
    };

    let prepared = prepare_restore_output(&source, &destination, &expected, root.path()).expect("prepared restore");
    let before = prepared.source_identity.clone();
    let member = NativeRestoreMember::Output {
      source: source.to_str().expect("UTF-8 source").to_string(),
      destination: destination.to_str().expect("UTF-8 destination").to_string(),
      source_identity: before.clone(),
      previous_identity: None,
      content_digest: expected.content_digest.clone(),
    };
    assert_eq!(prepared.bytes_hashed, 0);
    let published = publish_prepared_restore_output(prepared, &member).expect("atomic restore publication");
    published.sync().expect("durable restored output");
    published.revalidate().expect("registered restored output");

    assert!(!source.exists());
    assert_eq!(fs::read(&destination).expect("published bytes"), b"verified metadata");
    let published = File::open(&destination).expect("published output");
    assert_eq!(
      native_restore_file_identity(&published).expect("published identity"),
      before
    );
  }

  #[cfg(any(unix, windows))]
  #[test]
  fn restore_commit_rejects_a_destination_created_after_authorization() {
    let root = tempfile::tempdir().expect("restore root");
    let staging = root.path().join("private-staging");
    let output = root.path().join("target/debug/deps");
    fs::create_dir_all(&staging).expect("staging directory");
    fs::create_dir_all(&output).expect("output directory");
    let source = staging.join("libfixture.rmeta");
    let destination = output.join("libfixture.rmeta");
    fs::write(&source, b"verified metadata").expect("verified CAS copy");
    let expected = NativeCompilerOutput {
      role: "metadata".to_string(),
      slot: METADATA_SLOT.to_string(),
      content_digest: digest(b"verified metadata"),
      bytes: 17,
      mode: 0o644,
    };
    let prepared = prepare_restore_output(&source, &destination, &expected, root.path()).expect("prepared restore");
    let member = NativeRestoreMember::Output {
      source: source.to_str().expect("UTF-8 source").to_string(),
      destination: destination.to_str().expect("UTF-8 destination").to_string(),
      source_identity: prepared.source_identity.clone(),
      previous_identity: None,
      content_digest: expected.content_digest,
    };
    fs::write(&destination, b"unregistered").expect("prepositioned destination");

    let error = publish_prepared_restore_output(prepared, &member).expect_err("replacement must fail closed");

    assert!(error.to_string().contains("changed after authorization"), "{error}");
    assert_eq!(fs::read(&destination).expect("destination bytes"), b"unregistered");
    assert_eq!(
      fs::read(&source).expect("registered source bytes"),
      b"verified metadata"
    );
  }

  #[test]
  fn restore_recovery_discards_partial_private_records_before_authority() {
    let root = tempfile::tempdir().expect("restore root");
    let output = root.path().join("target/debug/deps");
    let observations = root.path().join("observations");
    fs::create_dir_all(&output).expect("output directory");
    fs::create_dir(&observations).expect("observation directory");
    let outputs = NativeOutputPaths {
      dep_info: output.join("fixture.d"),
      metadata: output.join("libfixture.rmeta"),
      rlib: None,
    };
    let paths = restore_commit_paths(&outputs, root.path()).expect("restore paths");
    fs::create_dir(&paths.transaction_directory).expect("unregistered transaction");
    fs::write(paths.transaction_directory.join(RESTORE_REGISTRATION_FILE), b"{").expect("partial registration");
    recover_restore_commit(&outputs, root.path(), &observations).expect("partial registration recovery");
    assert!(!paths.transaction_directory.exists());

    let action_key = format!("{ACTION_KEY_PREFIX}{}", "a".repeat(64));
    let transaction =
      begin_restore_transaction(&outputs, root.path(), &observations, &action_key).expect("registered transaction");
    fs::write(
      transaction
        .paths
        .transaction_directory
        .join(RESTORE_PENDING_COMMIT_FILE),
      b"{",
    )
    .expect("partial pending commit");
    let transaction_directory = transaction.paths.transaction_directory.clone();
    drop(transaction);

    recover_restore_commit(&outputs, root.path(), &observations).expect("partial pending-commit recovery");
    assert!(!transaction_directory.exists());
  }

  #[test]
  fn restore_transaction_rejects_an_rlib_for_a_metadata_only_action() {
    let root = tempfile::tempdir().expect("restore root");
    let output = root.path().join("target/debug/deps");
    let observations = root.path().join("observations");
    fs::create_dir_all(&output).expect("output directory");
    fs::create_dir(&observations).expect("observation directory");
    let outputs = NativeOutputPaths {
      dep_info: output.join("fixture.d"),
      metadata: output.join("libfixture.rmeta"),
      rlib: None,
    };
    let action_key = format!("{ACTION_KEY_PREFIX}{}", "b".repeat(64));
    let mut transaction =
      begin_restore_transaction(&outputs, root.path(), &observations, &action_key).expect("registered transaction");
    let unowned = transaction
      .paths
      .transaction_directory
      .join(RESTORE_VERIFIED_DIRECTORY)
      .join(RLIB_SLOT);
    fs::create_dir_all(unowned.parent().expect("unowned member parent")).expect("verified output directories");
    fs::write(&unowned, b"unowned rlib").expect("unowned rlib");

    let error = transaction.rollback().expect_err("unowned rlib must fail closed");

    assert!(error.to_string().contains("unknown member"), "{error}");
    assert_eq!(fs::read(unowned).expect("preserved unowned member"), b"unowned rlib");
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
      assert!(class.is_valid(), "{platform}/{host_target}/{release}");
    }

    let mut invalid = session.class;
    invalid.host_target = "unknown".to_string();
    assert!(!invalid.is_valid());
  }

  #[test]
  fn session_identity_changes_with_exact_compiler_authority() {
    let session = graduated_session(digest(b"source-root"));
    let identity = |capability: &str, environment: &str, contract: &str| {
      session_identity(
        &session.class,
        capability,
        environment,
        contract,
        NativeSessionAuthority::Exact,
      )
      .expect("session identity")
    };
    assert_ne!(
      identity(
        &digest(b"changed-capability"),
        &session.compiler_process_environment_identity,
        &session.execution_contract,
      ),
      session.identity
    );
    assert_ne!(
      identity(
        &session.capability_identity,
        &digest(b"changed-environment"),
        &session.execution_contract,
      ),
      session.identity
    );
    assert_ne!(
      identity(
        &session.capability_identity,
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
      &session.compiler_process_environment_identity,
      &session.execution_contract,
      session.authority,
    )
    .expect("session identity");

    assert_ne!(session.identity, original_identity);
    session.validate_object().expect("exact compiler identity");
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
        source_root_spelling: source_root.path(),
        session: graduated_session(digest(b"source-root")),
        deferred_session: None,
        wrapper_plan: plan,
        setup_bytes_hashed: 0,
        l2_alias: None,
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
      Some("compiler-action-v9-sha256-aaaa".to_string()),
      Some("compiler-result-v6-sha256-1111".to_string()),
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
          action_key: Some("compiler-action-v9-sha256-bbbb"),
          result_key: Some("compiler-result-v6-sha256-2222"),
          base_action_key: None,
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
          action_key: Some("compiler-action-v9-sha256-aaaa"),
          result_key: Some("compiler-result-v6-sha256-1111"),
          base_action_key: None,
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
          action_key: None,
          result_key: None,
          base_action_key: None,
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
      publication: None,
      observations,
      cargo_config: OsString::new(),
      setup_bytes_hashed: 40,
      remote: None,
      remote_configuration_failed: false,
      publication_configuration_failed: false,
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
      Some("compiler-action-v9-sha256-aaaa")
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
      Some("compiler-action-v9-sha256-bbbb")
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
  fn session_identity_is_portable_while_the_session_file_remains_root_bound() {
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
  fn cold_execution_injects_the_versioned_portable_root_contract() {
    let directory = tempfile::tempdir().expect("source parent");
    let source_root = directory.path().join("root=with-value");
    fs::create_dir(&source_root).expect("source root");
    let capture = synthetic_capture(&graduated_observation());
    let mut command = Command::new("rustc");
    let compiler_arguments = [OsString::from("src/lib.rs")];
    prepare_observed_cold_child(
      &mut command,
      OsStr::new("rustc"),
      &compiler_arguments,
      &source_root,
      &source_root,
      &capture,
      false,
    )
    .expect("portable cold command");
    let arguments = command.get_args().collect::<Vec<_>>();
    assert_eq!(arguments[0], "src/lib.rs");
    assert_eq!(arguments[1], "--remap-path-prefix");
    assert_eq!(arguments[2], "src/lib.rs=/cargo-rail/native-source/v2/src/lib.rs");
    assert_eq!(arguments[3], "--remap-path-prefix");
    let mut expected = source_root.as_os_str().to_os_string();
    expected.push("=");
    expected.push(PORTABLE_SOURCE_ROOT);
    assert_eq!(arguments[4], expected);
    assert_eq!(arguments[5], "--remap-path-scope=all");
  }

  #[test]
  fn root_bearing_environment_is_portable_only_while_unobserved() {
    let first = tempfile::tempdir().expect("first source root");
    let second = tempfile::tempdir().expect("second source root");
    let first_value = first.path().join("crate/out");
    let second_value = second.path().join("crate/out");
    let (first_normalized, first_mapped) = replace_source_root_spellings(
      first_value.as_os_str().as_encoded_bytes(),
      &source_root_spellings(first.path()).expect("first root spellings"),
      PORTABLE_SOURCE_ROOT,
    );
    let (second_normalized, second_mapped) = replace_source_root_spellings(
      second_value.as_os_str().as_encoded_bytes(),
      &source_root_spellings(second.path()).expect("second root spellings"),
      PORTABLE_SOURCE_ROOT,
    );
    assert!(first_mapped && second_mapped);
    assert_eq!(first_normalized, second_normalized);

    let mut observation = graduated_observation();
    observation.environment_reads.insert(EnvironmentObservation {
      name: "ROOT_VALUE".to_string(),
      value_digest: Some(digest(first_value.as_os_str().as_encoded_bytes())),
      secret_capability: false,
    });
    let mut capture = synthetic_capture(&observation);
    capture.approved_environment.entries[0] = ApprovedEnvEntry {
      name: "ROOT_VALUE".to_string(),
      value_digest: Some(digest(&first_normalized)),
      root_mapped: true,
    };
    let mut environment_only = observation.clone();
    environment_only.observed_reads.clear();
    assert!(capture.witness(&environment_only, Path::new("/workspace")).is_err());
    assert!(!capture.validates_witness(&synthetic_witness(&observation), &observation));
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
      mode: 0o644,
    })
    .collect::<Vec<_>>();
    let capture = synthetic_capture(&observation);
    let action = action_key(&session.identity, &session.class, &observation, &capture).expect("action");
    let witness = synthetic_witness(&observation);
    let validation = NativeCompilerValidation::new(
      &session,
      observation,
      &capture.approved_environment,
      pack::NativeResultDescriptor {
        action_key: action,
        witness,
        outputs,
        stdout_digest: digest(b""),
        stdout_bytes: 0,
        stderr_digest: digest(b""),
        stderr_bytes: 0,
      },
    )
    .expect("metadata/rlib validation");
    validation.validate_object().expect("valid rlib binding");

    let mut forged = validation;
    forged.outputs[2].content_digest = digest(b"same-size-forgery");
    forged.validate_object().expect_err("rlib bytes remain action-bound");
  }

  #[test]
  fn every_pre_execution_mutation_changes_the_action_identity() {
    let session = graduated_session(digest(b"source-root"));
    let baseline = graduated_observation();
    let baseline_key = action_key(
      &session.identity,
      &session.class,
      &baseline,
      &synthetic_capture(&baseline),
    )
    .expect("baseline action");
    let assert_changed = |observation: RawCompilerInvocation, label: &str| {
      assert_ne!(
        action_key(
          &session.identity,
          &session.class,
          &observation,
          &synthetic_capture(&observation),
        )
        .expect(label),
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
  fn output_and_dependency_directories_do_not_partition_action_identity() {
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

    let first_key =
      action_key(&session.identity, &session.class, &first, &synthetic_capture(&first)).expect("first action");
    assert_eq!(
      action_key(&session.identity, &session.class, &second, &synthetic_capture(&second),).expect("second action"),
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
      action_key(
        &session.identity,
        &session.class,
        &external_search,
        &synthetic_capture(&external_search),
      )
      .expect("first external search action"),
      action_key(
        &session.identity,
        &session.class,
        &changed_external_search,
        &synthetic_capture(&changed_external_search),
      )
      .expect("changed external search action"),
      "only Cargo's exact output directory may be erased from dependency search identity"
    );

    second.dependency_artifacts[0].1.content_digest = digest(b"changed dependency");
    assert_ne!(
      action_key(&session.identity, &session.class, &second, &synthetic_capture(&second),)
        .expect("changed dependency action"),
      first_key
    );
  }

  #[test]
  fn dep_info_materialization_rebinds_only_verified_output_paths() {
    let source_root = tempfile::tempdir().expect("source root");
    let observation = graduated_observation();
    let capture = synthetic_capture(&observation);
    let validation = graduated_validation(observation);
    let original_directory = source_root.path().join("build-one/debug/deps");
    fs::create_dir_all(&original_directory).expect("original output directory");
    let original_outputs = NativeOutputPaths {
      dep_info: original_directory.join("fixture-0123456789abcdef.d"),
      metadata: original_directory.join("libfixture-0123456789abcdef.rmeta"),
      rlib: None,
    };
    let output_directory = source_root.path().join("build-two/debug/deps");
    fs::create_dir_all(&output_directory).expect("current output directory");
    let outputs = NativeOutputPaths {
      dep_info: output_directory.join("fixture-0123456789abcdef.d"),
      metadata: output_directory.join("libfixture-0123456789abcdef.rmeta"),
      rlib: None,
    };
    let portable = portable_dep_info_output_bindings(
      b"build-one/debug/deps/libfixture-0123456789abcdef.rmeta: src/lib.rs\n",
      &original_outputs,
      source_root.path(),
      &capture,
    )
    .expect("canonical dep-info");
    let translated = translate_dep_info_output_bindings(&portable, &validation, &outputs, source_root.path(), &capture)
      .expect("portable dep-info");
    assert_eq!(
      translated,
      b"build-two/debug/deps/libfixture-0123456789abcdef.rmeta: src/lib.rs\n"
    );

    let windows_stream = portable_stream_output_bindings(
      br#"{"artifact":"build-one/debug/deps/libfixture-0123456789abcdef.rmeta","emit":"metadata"}"#,
      &original_outputs,
      source_root.path(),
    )
    .expect("canonical stream");
    let translated_stream =
      translate_output_binding_bytes(&windows_stream, &validation, &outputs, source_root.path(), false)
        .expect("Windows mixed-separator stream");
    assert_eq!(
      translated_stream,
      br#"{"artifact":"build-two/debug/deps/libfixture-0123456789abcdef.rmeta","emit":"metadata"}"#
    );

    let generated = b"build-one/debug/deps/libfixture-0123456789abcdef.rmeta: build-one/debug/deps/out/generated.rs\n";
    portable_dep_info_output_bindings(generated, &original_outputs, source_root.path(), &capture)
      .expect_err("unmodeled output-directory inputs must not be rebound");
  }

  #[test]
  fn stream_output_paths_use_canonical_tokens_on_windows() {
    let source_root = tempfile::tempdir().expect("source root");
    let output_paths = |directory: &str| {
      let directory = source_root.path().join(directory);
      fs::create_dir_all(&directory).expect("output directory");
      NativeOutputPaths {
        dep_info: directory.join("fixture-0123456789abcdef.d"),
        metadata: directory.join("libfixture-0123456789abcdef.rmeta"),
        rlib: None,
      }
    };
    let cold = br#"{"artifact":"build-one\\debug\\deps\\libfixture-0123456789abcdef.rmeta"}"#;
    let portable = portable_stream_output_bindings(cold, &output_paths("build-one/debug/deps"), source_root.path())
      .expect("portable stream");
    assert!(
      portable
        .windows(PORTABLE_OUTPUT_BINDING_PREFIX.len())
        .any(|window| { window == PORTABLE_OUTPUT_BINDING_PREFIX })
    );

    let restored = translate_output_binding_bytes(
      &portable,
      &graduated_validation(graduated_observation()),
      &output_paths("build-two/debug/deps"),
      source_root.path(),
      false,
    )
    .expect("restored stream");
    assert_eq!(
      restored,
      br#"{"artifact":"build-two\\debug\\deps\\libfixture-0123456789abcdef.rmeta"}"#
    );
  }

  #[test]
  fn compiler_stream_rejects_reserved_output_binding_tokens() {
    let source_root = tempfile::tempdir().expect("source root");
    let directory = source_root.path().join("build-one/debug/deps");
    let outputs = NativeOutputPaths {
      dep_info: directory.join("fixture-0123456789abcdef.d"),
      metadata: directory.join("libfixture-0123456789abcdef.rmeta"),
      rlib: None,
    };
    let stream = br#"{"artifact":"build-one/debug/deps/libfixture-0123456789abcdef.rmeta","message":"/cargo-rail/native-output/v3/metadata/relative/forward/literal"}"#;

    let error = portable_stream_output_bindings(stream, &outputs, source_root.path())
      .expect_err("compiler output must not collide with reserved CAS tokens");
    assert!(error.to_string().contains("reserved output-binding token"));
  }

  #[test]
  fn compiler_stream_rejects_output_paths_outside_artifact_fields() {
    let source_root = tempfile::tempdir().expect("source root");
    let directory = source_root.path().join("build-one/debug/deps");
    fs::create_dir_all(&directory).expect("output directory");
    let outputs = NativeOutputPaths {
      dep_info: directory.join("fixture-0123456789abcdef.d"),
      metadata: directory.join("libfixture-0123456789abcdef.rmeta"),
      rlib: None,
    };
    let stream = br#"{"artifact":"build-one/debug/deps/libfixture-0123456789abcdef.rmeta","message":"build-one/debug/deps/libfixture-0123456789abcdef.rmeta"}"#;

    let error = portable_stream_output_bindings(stream, &outputs, source_root.path())
      .expect_err("an output path in diagnostic text must not be rewritten");
    assert!(error.to_string().contains("unmodeled output-directory binding"));
  }

  #[test]
  fn dep_info_output_rebinding_preserves_windows_path_spelling() {
    let source_root = tempfile::tempdir().expect("source root");
    let observation = graduated_observation();
    let capture = synthetic_capture(&observation);
    let first_directory = source_root.path().join("build one/debug/deps");
    fs::create_dir_all(&first_directory).expect("first output directory");
    let first_outputs = NativeOutputPaths {
      dep_info: first_directory.join("fixture-0123456789abcdef.d"),
      metadata: first_directory.join("libfixture-0123456789abcdef.rmeta"),
      rlib: None,
    };
    let cold = br"build\ one\\debug\\deps\\libfixture-0123456789abcdef.rmeta: src\\lib.rs\n";
    let portable = portable_dep_info_output_bindings(cold, &first_outputs, source_root.path(), &capture)
      .expect("portable Windows dep-info");

    let validation = graduated_validation(observation);
    let second_directory = source_root.path().join("build two/debug/deps");
    fs::create_dir_all(&second_directory).expect("second output directory");
    let second_outputs = NativeOutputPaths {
      dep_info: second_directory.join("fixture-0123456789abcdef.d"),
      metadata: second_directory.join("libfixture-0123456789abcdef.rmeta"),
      rlib: None,
    };
    let restored =
      translate_dep_info_output_bindings(&portable, &validation, &second_outputs, source_root.path(), &capture)
        .expect("restored Windows dep-info");

    assert_eq!(
      restored,
      br"build\ two\\debug\\deps\\libfixture-0123456789abcdef.rmeta: src\\lib.rs\n"
    );

    let literal = br"build one\debug\deps\libfixture-0123456789abcdef.rmeta: src\lib.rs\n";
    let portable = portable_dep_info_output_bindings(literal, &first_outputs, source_root.path(), &capture)
      .expect("portable literal Windows dep-info");
    let restored =
      translate_dep_info_output_bindings(&portable, &validation, &second_outputs, source_root.path(), &capture)
        .expect("restored literal Windows dep-info");
    assert_eq!(
      restored,
      br"build two\debug\deps\libfixture-0123456789abcdef.rmeta: src\lib.rs\n"
    );
  }

  #[test]
  fn dep_info_cas_bytes_do_not_depend_on_cargo_output_directory() {
    let source_root = tempfile::tempdir().expect("source root");
    let capture = synthetic_capture(&graduated_observation());
    let output_paths = |directory: &str| {
      let directory = source_root.path().join(directory);
      fs::create_dir_all(&directory).expect("output directory");
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
      &capture,
    )
    .expect("first portable dep-info");
    let second = portable_dep_info_output_bindings(
      b"build-two/debug/deps/libfixture-0123456789abcdef.rmeta: src/lib.rs\n",
      &output_paths("build-two/debug/deps"),
      source_root.path(),
      &capture,
    )
    .expect("second portable dep-info");
    assert_eq!(first, second);
  }

  #[test]
  fn compiler_stream_capture_forwards_and_retains_exact_bytes_across_spill() {
    let input = vec![b'x'; STREAM_MEMORY_SPOOL_BYTES * 2 + 17];
    let mut forwarded = Vec::new();
    let captured = capture_compiler_stream(std::io::Cursor::new(&input), &mut forwarded, input.len());

    assert_eq!(forwarded, input);
    assert_eq!(captured.into_bytes().expect("bounded captured stream"), input);
  }

  #[test]
  fn compiler_stream_rebinds_a_source_root_split_across_reads() {
    let mut forwarded = Vec::new();
    {
      let mut writer = SourceRootRebindingWriter::new(
        &mut forwarded,
        vec![(PORTABLE_SOURCE_ROOT.as_bytes().to_vec(), b"/live/root".to_vec())],
      );
      writer
        .write_all(b"before /cargo-rail/native-")
        .expect("first stream part");
      writer
        .write_all(b"source/v2/src/lib.rs after")
        .expect("second stream part");
      writer.flush().expect("stream flush");
    }
    assert_eq!(forwarded, b"before /live/root/src/lib.rs after");
  }

  #[test]
  fn portable_bypass_drains_the_compiler_pipe_but_reports_a_forwarding_failure() {
    struct RejectingWriter;

    impl std::io::Write for RejectingWriter {
      fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(
          std::io::ErrorKind::BrokenPipe,
          "rejected compiler stream",
        ))
      }

      fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
      }
    }

    let bytes = vec![b'x'; 128 * 1024];
    let mut source = std::io::Cursor::new(&bytes);
    let error = forward_rebound_compiler_stream(&mut source, RejectingWriter, Vec::new())
      .expect_err("forwarding failure must be visible");
    assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    assert_eq!(source.position(), bytes.len() as u64, "compiler pipe was not drained");
  }

  #[test]
  fn portable_bypass_requires_json_diagnostics_for_exact_rebinding() {
    let mut observation = graduated_observation();
    let host_target = "x86_64-unknown-test";
    assert!(portable_dependency_producer(&observation, host_target));
    let mut cross_target = observation.clone();
    cross_target.target_argument = Some("aarch64-unknown-test".to_string());
    assert!(!portable_dependency_producer(&cross_target, host_target));
    *observation
      .compiler_arguments
      .iter_mut()
      .find(|argument| argument.starts_with("--error-format="))
      .expect("diagnostic format") = "--error-format=human".to_string();
    assert!(!portable_dependency_producer(&observation, host_target));
  }

  #[cfg(unix)]
  #[test]
  fn source_root_rebinding_preserves_literal_unix_backslashes() {
    let source_root = Path::new("/tmp/source\\root");
    let capture = synthetic_capture(&graduated_observation());

    assert_eq!(
      source_root_stream_bindings(source_root, &capture)[0].1,
      br#"/tmp/source\\root"#
    );
    assert_eq!(
      rebind_dep_info_source_roots(PORTABLE_SOURCE_ROOT.as_bytes(), source_root, &capture)
        .expect("rebound dep-info root"),
      br"/tmp/source\\root"
    );
    assert_eq!(
      source_root_path_spellings(source_root),
      vec![br"/tmp/source\root".to_vec()]
    );
  }

  #[test]
  fn compiler_stream_capture_reuses_the_in_memory_spool_allocation() {
    let input = b"ordinary compiler stream";
    let mut forwarded = Vec::new();
    let captured = capture_compiler_stream(std::io::Cursor::new(input), &mut forwarded, input.len());

    assert_eq!(forwarded, input);
    assert!(matches!(
      &captured,
      CapturedCompilerStream::Complete { storage, .. } if !storage.is_rolled()
    ));
    assert_eq!(captured.into_bytes().expect("in-memory captured stream"), input);
  }

  #[test]
  fn compiler_stream_capture_forwards_bytes_beyond_the_cache_limit() {
    let input = b"compiler output beyond the cache limit";
    let mut forwarded = Vec::new();
    let captured = capture_compiler_stream(std::io::Cursor::new(input), &mut forwarded, 8);

    assert_eq!(forwarded, input);
    assert!(captured.limit_exceeded());
  }

  #[test]
  fn remote_service_and_credential_faults_allow_cold_fallback() {
    use crate::remote_cache::RemoteStoreFault;

    for fault in [
      RemoteStoreFault::Unavailable,
      RemoteStoreFault::Authentication,
      RemoteStoreFault::Configuration,
    ] {
      assert!(remote_fault_allows_cold_fallback(fault), "{fault:?} must compile cold");
    }
    let fault = RemoteStoreFault::Integrity;
    assert!(
      !remote_fault_allows_cold_fallback(fault),
      "{fault:?} must remain an operational failure"
    );
  }

  #[test]
  fn remote_conflict_is_terminal_and_never_falls_back_to_rustc() {
    let cache = tempfile::tempdir().expect("local cache base");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("local CAS");
    let first = pack::association(&cas_validation_with_stdout(b"first")).expect("first association");
    let second = pack::association(&cas_validation_with_stdout(b"second")).expect("second association");
    assert_eq!(first.action_key(), second.action_key());

    let outcome = remote_conflict_failure(&cas, first.action_key(), first.result_key(), second.result_key());
    assert!(matches!(outcome, RemoteReuseOutcome::OperationalFailure(_)));
    let crate::hermetic::cas::NativeActionLookup::Miss(miss) =
      cas.native_action(first.action_key()).expect("terminal local lookup")
    else {
      panic!("remote conflict must never authorize one local result");
    };
    assert_eq!(miss.reason, "action_conflicted");
  }

  #[test]
  fn failed_remote_admission_rechecks_terminal_action_state_before_cold_fallback() {
    let prepared = |validation: NativeCompilerValidation, stdout: &[u8]| {
      let staging = tempfile::tempdir().expect("native result staging");
      for (slot, bytes) in [
        (DEP_INFO_SLOT, b"dep-info".as_slice()),
        (METADATA_SLOT, b"metadata".as_slice()),
        (STDOUT_SLOT, stdout),
        (STDERR_SLOT, b"".as_slice()),
      ] {
        let path = staging.path().join(slot);
        fs::create_dir_all(path.parent().expect("slot parent")).expect("slot directory");
        fs::write(path, bytes).expect("slot bytes");
      }
      let paths = [DEP_INFO_SLOT, METADATA_SLOT, STDOUT_SLOT, STDERR_SLOT]
        .into_iter()
        .map(|slot| staging.path().join(slot))
        .collect::<Vec<_>>();
      let manifest =
        crate::hermetic::capture_native_compiler_outputs(staging.path(), &paths).expect("native result manifest");
      PreparedNativeResult::from_verified_staging(staging, manifest, validation)
    };
    let cache = tempfile::tempdir().expect("local cache base");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("local CAS");
    let first = cas_validation_with_stdout(b"first");
    let second = cas_validation_with_stdout(b"second");
    assert_eq!(first.action_key(), second.action_key());
    cas
      .store_native(prepared(first.clone(), b"first"))
      .expect("first local result");
    let admission_error = cas
      .store_native(prepared(second, b"second"))
      .expect_err("distinct result must make the action terminal");
    let authority = RemoteAuthorityId::for_test("failed-admission-authority").expect("remote authority");
    assert!(matches!(
      recover_failed_remote_admission(&cas, &authority, first.action_key(), admission_error),
      RemoteAdmissionRecovery::OperationalFailure(_)
    ));

    let missing = format!("{ACTION_KEY_PREFIX}{}", "f".repeat(64));
    assert!(matches!(
      recover_failed_remote_admission(
        &cas,
        &authority,
        &missing,
        RailError::message("simulated pre-authority admission failure")
      ),
      RemoteAdmissionRecovery::Cold
    ));
  }

  #[test]
  fn selector_divergence_reason_requires_one_exact_segment() {
    assert!(native_cache_reason_contains(
      "exact_action_not_found;environment_selector_diverged",
      "environment_selector_diverged"
    ));
    assert!(!native_cache_reason_contains(
      "exact_action_not_found;environment_selector_diverged_later",
      "environment_selector_diverged"
    ));
    assert!(!native_cache_reason_contains(
      "exact_action_not_found_environment_selector_diverged",
      "environment_selector_diverged"
    ));
  }
}
