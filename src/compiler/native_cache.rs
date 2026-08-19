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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::cache::cas::LocalCas;
use crate::cache::cas::NativeCacheLookup;
use crate::cache::result::OutputManifest;
use crate::compiler::observation::{
  CompilerCacheWrapperMetadata, CompilerCacheWrapperStatus, CompilerMode, EnvironmentObservation, FileObservation,
  InvocationRecorder, NativeOutputPaths, NativeOutputRole, ObservationPath, PreparedRawPublication,
  RawCompilerInvocation,
};
use crate::error::{RailError, RailResult};
use crate::source::ContentDigest;

pub(crate) mod pack;

pub(crate) const ACTION_KEY_PREFIX: &str = "compiler-action-v16-sha256-";
pub(crate) const RESULT_KEY_PREFIX: &str = "compiler-result-v10-sha256-";
pub(crate) const BASE_ACTION_KEY_PREFIX: &str = "compiler-base-v10-sha256-";
pub(crate) const CANDIDATE_SELECTOR_PREFIX: &str = "compiler-candidate-v7-sha256-";
pub(crate) const SESSION_ENV: &str = "CARGO_RAIL_NATIVE_COMPILER_CACHE_SESSION";
pub(crate) const DISPOSITION_ENV: &str = "CARGO_RAIL_NATIVE_COMPILER_CACHE_DISPOSITION";
const BENCH_COVERAGE_DIRECTORY_ENV: &str = "CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY";
const LEGACY_STORE_ENV: &str = "CARGO_RAIL_NATIVE_COMPILER_CACHE_STORE";
pub(crate) const APPLE_LINK_ADAPTER_ENV: &str = "CARGO_RAIL_APPLE_LINK_ADAPTER";
pub(crate) const APPLE_LINK_DRIVER_ENV: &str = "CARGO_RAIL_APPLE_LINK_DRIVER";
pub(crate) const APPLE_LINK_CERTIFICATE_ENV: &str = "CARGO_RAIL_APPLE_LINK_CERTIFICATE";
pub(crate) const APPLE_LINK_DRIVER_INPUTS_ENV: &str = "CARGO_RAIL_APPLE_LINK_DRIVER_INPUTS";
pub(crate) const ELF_LINK_ADAPTER_ENV: &str = "CARGO_RAIL_ELF_LINK_ADAPTER";
pub(crate) const ELF_LINK_DRIVER_ENV: &str = "CARGO_RAIL_ELF_LINK_DRIVER";
pub(crate) const ELF_LINK_DEPENDENCIES_ENV: &str = "CARGO_RAIL_ELF_LINK_DEPENDENCIES";
pub(crate) const ELF_LINK_DRIVER_INPUTS_ENV: &str = "CARGO_RAIL_ELF_LINK_DRIVER_INPUTS";
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
pub(crate) const DIAGNOSTIC_EXECUTION_CONTRACT: &str = "diagnostic-workspace-wrapper-v13";
pub(crate) const DIRECT_EXECUTION_CONTRACT: &str = "direct-global-wrapper-v14";
#[cfg(not(windows))]
const DIRECT_WRAPPER_NAME: &str = "cargo-rail-native-rustc-wrapper";
#[cfg(windows)]
const DIRECT_WRAPPER_NAME: &str = "cargo-rail-native-rustc-wrapper.exe";
#[cfg(not(windows))]
const DIRECT_WORKER_NAME: &str = "cargo-rail-native-rustc-worker";
#[cfg(windows)]
const DIRECT_WORKER_NAME: &str = "cargo-rail-native-rustc-worker.exe";
#[cfg(not(windows))]
const DISTRIBUTED_WORKER_NAME: &str = "cargo-rail-distributed-worker";
#[cfg(windows)]
const DISTRIBUTED_WORKER_NAME: &str = "cargo-rail-distributed-worker.exe";
const DIRECT_LAUNCHER_ENV: &str = "CARGO_RAIL_DIRECT_CACHE_LAUNCHER";
const GRADUATED_NATIVE_CACHE_CLASS: &str = "exact_rustc_result";
const NATIVE_CACHE_CAPABILITY_SCHEMA_VERSION: u32 = 11;
const NATIVE_CACHE_IDENTITY_CONTRACT_VERSION: u32 = 16;
const NATIVE_COMPILER_SESSION_VERSION: u32 = 16;
const MAX_SESSION_BYTES: u64 = 64 * 1024;
const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;
const MAX_BENCH_COVERAGE_EVENT_BYTES: usize = 1024 * 1024;
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
#[cfg(target_os = "macos")]
const MAX_APPLE_LINK_CERTIFICATE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_LINK_INPUTS: usize = 16 * 1024;
const MAX_LINK_PATH_BYTES: usize = 16 * 1024 * 1024;
#[cfg(target_os = "linux")]
const MAX_ELF_LINK_DEPENDENCY_BYTES: u64 = 8 * 1024 * 1024;
const DEP_INFO_SLOT: &str = "target/outputs/dep-info";
const METADATA_SLOT: &str = "target/outputs/metadata";
const RLIB_SLOT: &str = "target/outputs/rlib";
const EXECUTABLE_SLOT: &str = "target/outputs/executable";
const PROC_MACRO_SLOT: &str = "target/outputs/proc-macro";
const DYLIB_SLOT: &str = "target/outputs/dylib";
const CDYLIB_SLOT: &str = "target/outputs/cdylib";
const STATICLIB_SLOT: &str = "target/outputs/staticlib";
const STDOUT_SLOT: &str = "target/streams/stdout";
const STDERR_SLOT: &str = "target/streams/stderr";
const APPLE_LINK_CERTIFICATE_FILE: &str = "apple-linker-dependencies.bin";
const APPLE_LINK_DRIVER_INPUTS_FILE: &str = "apple-linker-driver-inputs.json";
const ELF_LINK_DEPENDENCIES_FILE: &str = "elf-linker-dependencies.d";
const ELF_LINK_DRIVER_INPUTS_FILE: &str = "elf-linker-driver-inputs.json";
#[cfg(target_os = "macos")]
const APPLE_LINK_DRIVER_EVIDENCE_VERSION: u32 = 2;
const PORTABLE_SOURCE_ROOT: &str = "/cargo-rail/native-source/v2";
const PORTABLE_PACKAGE_ROOT: &str = "/cargo-rail/native-package/v2";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeCompilerSession {
  version: u32,
  identity: String,
  /// Exact workspace-root binding for this session and every reusable action.
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

pub(crate) struct NativeCacheContext {
  session: NativeCacheSession,
  source_root: PathBuf,
  source_root_spelling: PathBuf,
  observation_directory: PathBuf,
  local_cas: Option<LocalCas>,
  remote: Option<crate::remote_cache::RemoteCacheSelection>,
  remote_store: OnceLock<Result<crate::remote_cache::RemoteStore, crate::remote_cache::RemoteStoreError>>,
  installation: Option<crate::cache::installation::InstallationReceipt>,
  _runtime: Option<tempfile::TempDir>,
}

enum NativeCacheSession {
  Prepared(NativeCompilerSession),
  Persisted(PathBuf),
}

impl NativeCacheSession {
  fn load(&self, source_root: &Path) -> RailResult<NativeCompilerSession> {
    match self {
      Self::Prepared(session) => Ok(session.clone()),
      Self::Persisted(path) => NativeCompilerSession::load(path, source_root),
    }
  }
}

static ACTIVE_CONTEXT: OnceLock<NativeCacheContext> = OnceLock::new();
static BENCH_COVERAGE_DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
static BENCH_DURABILITY_COUNTERS: OnceLock<NativeDurabilityCounters> = OnceLock::new();

const NATIVE_DURABILITY_PHASE_COUNT: usize = 8;

/// Benchmark-only ownership phases at synchronous cache durability boundaries.
#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum NativeDurabilityPhase {
  L1FileSync,
  L1DirectorySync,
  OutputFileSync,
  OutputDirectorySync,
  CasLockWait,
  CasCommit,
  RestoreLockWait,
  RestoreTransaction,
}

struct NativeDurabilityCounters {
  counts: [AtomicU64; NATIVE_DURABILITY_PHASE_COUNT],
  elapsed_ns: [AtomicU64; NATIVE_DURABILITY_PHASE_COUNT],
}

impl NativeDurabilityCounters {
  const fn new() -> Self {
    Self {
      counts: [const { AtomicU64::new(0) }; NATIVE_DURABILITY_PHASE_COUNT],
      elapsed_ns: [const { AtomicU64::new(0) }; NATIVE_DURABILITY_PHASE_COUNT],
    }
  }

  fn snapshot(&self) -> NativeDurabilitySnapshot {
    NativeDurabilitySnapshot {
      l1_file_sync: self.measurement(NativeDurabilityPhase::L1FileSync),
      l1_directory_sync: self.measurement(NativeDurabilityPhase::L1DirectorySync),
      output_file_sync: self.measurement(NativeDurabilityPhase::OutputFileSync),
      output_directory_sync: self.measurement(NativeDurabilityPhase::OutputDirectorySync),
      cas_lock_wait: self.measurement(NativeDurabilityPhase::CasLockWait),
      cas_commit: self.measurement(NativeDurabilityPhase::CasCommit),
      restore_lock_wait: self.measurement(NativeDurabilityPhase::RestoreLockWait),
      restore_transaction: self.measurement(NativeDurabilityPhase::RestoreTransaction),
    }
  }

  fn measurement(&self, phase: NativeDurabilityPhase) -> NativePhaseMeasurement {
    NativePhaseMeasurement {
      count: self.counts[phase as usize].load(Ordering::Relaxed),
      elapsed_ns: self.elapsed_ns[phase as usize].load(Ordering::Relaxed),
    }
  }
}

/// One source-free phase counter shared by every native timing snapshot.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub(crate) struct NativePhaseMeasurement {
  pub(crate) count: u64,
  pub(crate) elapsed_ns: u64,
}

impl NativePhaseMeasurement {
  pub(crate) fn record(&mut self, started: Instant) {
    self.count = self.count.saturating_add(1);
    self.elapsed_ns = self.elapsed_ns.saturating_add(elapsed_nanos(started));
  }
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
struct NativeDurabilitySnapshot {
  l1_file_sync: NativePhaseMeasurement,
  l1_directory_sync: NativePhaseMeasurement,
  output_file_sync: NativePhaseMeasurement,
  output_directory_sync: NativePhaseMeasurement,
  cas_lock_wait: NativePhaseMeasurement,
  cas_commit: NativePhaseMeasurement,
  restore_lock_wait: NativePhaseMeasurement,
  restore_transaction: NativePhaseMeasurement,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
struct NativeRemoteTimingSnapshot {
  total: NativePhaseMeasurement,
  store_connect: NativePhaseMeasurement,
  lookup: NativePhaseMeasurement,
  decode: NativePhaseMeasurement,
  validation: NativePhaseMeasurement,
  l1_admission: NativePhaseMeasurement,
  output_restore: NativePhaseMeasurement,
}

/// Active timer that is a no-op unless benchmark coverage was explicitly enabled.
pub(crate) struct NativeDurabilityGuard {
  phase: NativeDurabilityPhase,
  started: Option<Instant>,
}

impl Drop for NativeDurabilityGuard {
  fn drop(&mut self) {
    let (Some(counters), Some(started)) = (BENCH_DURABILITY_COUNTERS.get(), self.started) else {
      return;
    };
    let index = self.phase as usize;
    counters.counts[index].fetch_add(1, Ordering::Relaxed);
    counters.elapsed_ns[index].fetch_add(elapsed_nanos(started), Ordering::Relaxed);
  }
}

pub(crate) fn native_durability_phase(phase: NativeDurabilityPhase) -> NativeDurabilityGuard {
  NativeDurabilityGuard {
    phase,
    started: BENCH_DURABILITY_COUNTERS.get().map(|_| Instant::now()),
  }
}

fn native_durability_snapshot() -> NativeDurabilitySnapshot {
  BENCH_DURABILITY_COUNTERS
    .get()
    .map_or_else(NativeDurabilitySnapshot::default, NativeDurabilityCounters::snapshot)
}

fn elapsed_nanos(started: Instant) -> u64 {
  u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

pub(crate) const fn native_cache_class() -> &'static str {
  GRADUATED_NATIVE_CACHE_CLASS
}

pub(crate) const fn native_cache_execution_contract() -> &'static str {
  DIRECT_EXECUTION_CONTRACT
}

pub(crate) const fn native_cache_transported_work_boundary() -> &'static str {
  "moved_root_compiler_work_product_validation_unavailable"
}

pub(crate) const fn native_cache_capability_schema_version() -> u32 {
  NATIVE_CACHE_CAPABILITY_SCHEMA_VERSION
}

#[derive(Clone, Copy, Debug, Default)]
struct NativeCacheMetrics {
  bytes_hashed: u64,
  cache_bytes_read: u64,
  remote_started: Option<Instant>,
  remote_timing: NativeRemoteTimingSnapshot,
  /// Present only for a verified distributed execution, so ordinary cache
  /// evidence never carries an empty distributed phase block.
  distributed_timing: Option<crate::compiler::distributed::DistributedTiming>,
}

impl NativeCacheMetrics {
  fn begin_remote(&mut self) {
    if self.remote_started.is_none() {
      self.remote_started = Some(Instant::now());
    }
  }

  fn finish_remote(&mut self) {
    if let Some(started) = self.remote_started.take() {
      self.remote_timing.total.record(started);
    }
  }
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
  generated_paths: Vec<String>,
  dependency_names: Vec<String>,
  environment_names: Vec<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  linker: Option<LinkerWitness>,
}

/// Exact platform-specific closure selected by one linked compiler action.
///
/// Provider lookup rules remain separate, while the enum makes it impossible
/// for one action to claim multiple linker authorities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "witness", rename_all = "snake_case")]
enum LinkerWitness {
  Apple(AppleLinkerWitness),
  Elf(ElfLinkerWitness),
}

/// Revalidatable closure emitted by one certified Apple linker execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppleLinkerWitness {
  version: u32,
  certificate_version: String,
  driver: LinkFileWitness,
  linker: LinkFileWitness,
  found: Vec<LinkFileWitness>,
  missing: Vec<String>,
  endogenous_objects: u32,
  endogenous_archives: u32,
  dependency_archives: Vec<String>,
}

/// Revalidatable closure emitted by one GNU ELF linker dependency file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ElfLinkerWitness {
  version: u32,
  driver: LinkFileWitness,
  linker: LinkFileWitness,
  found: Vec<LinkFileWitness>,
  missing: Vec<String>,
  endogenous_objects: u32,
  dependency_archives: Vec<String>,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ElfLinkDriverEvidence {
  version: u32,
  current_directory: String,
  driver: String,
  linker: String,
  tool_inputs: Vec<String>,
  search_directories: Vec<String>,
  direct_inputs: Vec<String>,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppleLinkDriverEvidence {
  version: u32,
  direct_inputs: Vec<String>,
  temporary_directories: Vec<String>,
  preexisting_paths: Vec<String>,
  generated_inputs: Vec<String>,
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct CertifiedAppleLinkInputs {
  direct: BTreeSet<PathBuf>,
  generated: BTreeSet<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkFileWitness {
  /// Exact absolute path spelling reported by the linker.
  path: String,
  /// Canonical file selected through that exact path at capture time.
  canonical_path: String,
  content_digest: String,
  bytes: u64,
  mode: u32,
}

/// Private, non-semantic proof that one installation may reuse linker-input
/// digests while the underlying files retain their exact local generations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkerGenerationWitness {
  version: u32,
  installation_authority: String,
  driver: String,
  linker: String,
  found: Vec<String>,
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
    #[cfg(not(windows))]
    sync_native_before_commit(&self.opened)?;
    // Windows restore sources are flushed through their original writable
    // handles before the write-through rename publishes them.
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
}

struct NativeRestorePaths {
  identity: ContentDigest,
  output_parent: PathBuf,
  marker: PathBuf,
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
  _lock: crate::cache::cas::NativeRestoreLock,
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
  generated: Option<NativeNamespaceCapture>,
  native_searches: Vec<NativeNamespaceCapture>,
  pathless_extern_searches: Vec<NativePathlessExternSearchCapture>,
  approved_environment: ApprovedEnvState,
  guard: NativeCaptureGuard,
  bytes_hashed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NativeNamespaceCapture {
  root: PathBuf,
  root_spelling: PathBuf,
  state: NativeSourceState,
  guard: NativeCaptureGuard,
}

/// Matching CLI search-path candidates for one toolchain-owned pathless extern.
///
/// The semantic key retains candidate names and content. Physical roots remain
/// local revalidation capabilities; the root-bound session remains the reuse
/// authority while this witness prevents an unobserved `-L` shadow candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NativePathlessExternSearchCapture {
  root: PathBuf,
  root_spelling: PathBuf,
  entries: Vec<NativeSourceEntry>,
  guard: NativeCaptureGuard,
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

struct NativePublicationRevalidationFailure {
  reason: &'static str,
  error: RailError,
}

impl NativePublicationRevalidationFailure {
  fn new(reason: &'static str, error: RailError) -> Self {
    Self { reason, error }
  }
}

/// One privately staged native result whose semantic identities were derived
/// from a live action capture rather than supplied by a storage caller.
pub(crate) struct PreparedNativeResult {
  staging: tempfile::TempDir,
  staging_lock: Option<File>,
  verified_generations: BTreeMap<PathBuf, Vec<u8>>,
  manifest: OutputManifest,
  validation: NativeCompilerValidation,
  move_preverified_blobs: bool,
}

pub(crate) struct PreparedNativeParts {
  pub(crate) staging: tempfile::TempDir,
  pub(crate) staging_lock: Option<File>,
  pub(crate) verified_generations: BTreeMap<PathBuf, Vec<u8>>,
  pub(crate) manifest: OutputManifest,
  pub(crate) validation: NativeCompilerValidation,
  pub(crate) move_preverified_blobs: bool,
}

/// Identity of one deployment-pinned authenticated remote authority tuple.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct RemoteAuthorityId(String);

impl RemoteAuthorityId {
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
    manifest: OutputManifest,
    validation: NativeCompilerValidation,
  ) -> Self {
    Self {
      staging,
      staging_lock: None,
      verified_generations: BTreeMap::new(),
      manifest,
      validation,
      move_preverified_blobs: false,
    }
  }

  fn from_verified_local_cas_staging(
    staging: pack::NativeResultStaging,
    manifest: OutputManifest,
    validation: NativeCompilerValidation,
  ) -> Self {
    let (staging, staging_lock, verified_generations) = staging.into_parts();
    let move_preverified_blobs = staging_lock.is_some();
    Self {
      staging,
      staging_lock,
      verified_generations,
      manifest,
      validation,
      move_preverified_blobs,
    }
  }

  pub(crate) fn into_parts(self) -> PreparedNativeParts {
    PreparedNativeParts {
      staging: self.staging,
      staging_lock: self.staging_lock,
      verified_generations: self.verified_generations,
      manifest: self.manifest,
      validation: self.validation,
      move_preverified_blobs: self.move_preverified_blobs,
    }
  }
}

fn prepare_authenticated_native_handoff(
  decoded: pack::DecodedNativePack,
  session: &NativeCompilerSession,
  initial_capture: &NativeActionCapture,
  current_observation: &RawCompilerInvocation,
  output_paths: &NativeOutputPaths,
  source_root: &Path,
) -> RailResult<(NativeCompilerValidation, pack::NativePackHandoff, u64)> {
  let (staging, _manifest, validation, bytes_read) = authenticate_native_pack(
    decoded,
    session,
    initial_capture,
    current_observation,
    output_paths,
    source_root,
  )?;
  Ok((validation, staging.into_output_handoff(), bytes_read))
}

/// Bind one authenticated, byte-verified pack to the current live action.
fn authenticate_native_pack(
  decoded: pack::DecodedNativePack,
  session: &NativeCompilerSession,
  initial_capture: &NativeActionCapture,
  current_observation: &RawCompilerInvocation,
  output_paths: &NativeOutputPaths,
  source_root: &Path,
) -> RailResult<(pack::NativeResultStaging, OutputManifest, NativeCompilerValidation, u64)> {
  let pack::DecodedNativePack {
    staging,
    descriptor,
    bytes_read,
  } = decoded;
  let pre_link_action = action_key(&session.identity, &session.class, current_observation, initial_capture)?;
  let (live_action, _) =
    revalidate_selected_action(current_observation, &descriptor.witness, None, &pre_link_action, None)?;
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
      executable: expected.mode & 0o111 != 0,
      symlink_target: None,
    })
    .collect();
  observation.emitted_outputs.sort();
  observation.success = true;
  observation.cache_wrapper = None;

  let validation = NativeCompilerValidation::new(
    session,
    observation,
    &initial_capture.approved_environment,
    None,
    descriptor,
  )?;
  let slots = validation
    .cas_output_bindings()
    .chain(validation.cas_stream_bindings())
    .collect::<Vec<_>>();
  let manifest = crate::cache::result::manifest_from_verified_native_slots(&slots)?;
  Ok((staging, manifest, validation, bytes_read))
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
  if !witness.generated_paths.is_empty() {
    let generated = capture
      .generated
      .as_ref()
      .ok_or_else(|| RailError::message("native result witness has no current generated namespace"))?;
    observed.extend(
      witness
        .generated_paths
        .iter()
        .map(|relative| {
          let index = generated
            .state
            .entries
            .binary_search_by(|entry| entry.path.as_str().cmp(relative))
            .map_err(|_| RailError::message("native result witness is absent from current generated state"))?;
          let NativeSourceEntryKind::RegularFile {
            content_digest, mode, ..
          } = &generated.state.entries[index].kind
          else {
            return Err(RailError::message(
              "native result witness selected a non-file generated capability",
            ));
          };
          Ok(FileObservation {
            path: ObservationPath::capture(&generated.root_spelling.join(relative), source_root, source_root),
            content_digest: content_digest.clone(),
            executable: source_mode_executable(*mode),
            symlink_target: None,
          })
        })
        .collect::<RailResult<Vec<_>>>()?,
    );
  }
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
  fn capture(observation: &RawCompilerInvocation, source_root: &Path) -> RailResult<Self> {
    Self::capture_with_environment(observation, source_root, None, None)
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
    let source_exclusions = compiler_owned_source_exclusions(source_root)?;
    let (source_state, guard) = capture_native_source_namespace(
      &namespace,
      Some(&crate_root),
      &source_exclusions,
      source_root,
      started,
      &mut budget,
    )?;
    let generated = capture_native_generated_namespace(observation, source_root, started, &mut budget)?;
    let native_searches = capture_native_search_namespaces(
      &observation.compiler_arguments,
      generated.as_ref(),
      source_root,
      started,
      &mut budget,
    )?;
    let pathless_extern_searches =
      capture_pathless_extern_searches(&observation.compiler_arguments, source_root, started, &mut budget)?;
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
      generated,
      native_searches,
      pathless_extern_searches,
      approved_environment,
      guard: NativeCaptureGuard { entries: guard_entries },
      bytes_hashed: budget.bytes_hashed.saturating_add(environment_bytes),
    })
  }

  #[cfg(test)]
  fn unchanged_from(&self, initial: &Self) -> bool {
    self.crate_root == initial.crate_root
      && self.package_binding == initial.package_binding
      && self.source_state == initial.source_state
      && self.generated == initial.generated
      && self.native_searches == initial.native_searches
      && self.pathless_extern_searches == initial.pathless_extern_searches
      && self.approved_environment == initial.approved_environment
      && self.guard == initial.guard
  }

  /// Revalidate the exact live generations retained by the initial byte capture.
  ///
  /// The initial capture remains the content authority for the action key. This
  /// check only closes the interval between that capture and restore publication,
  /// so re-reading and hashing the same bytes would add work without strengthening
  /// the retained generation proof.
  fn revalidate_before_restore_commit(
    &self,
    observation: &RawCompilerInvocation,
    workspace_root: &Path,
    workspace_root_spelling: &Path,
  ) -> RailResult<u64> {
    let [declared] = observation.declared_inputs.as_slice() else {
      return Err(RailError::message(
        "native restore revalidation requires one declared crate root",
      ));
    };
    if declared.symlink_target.is_some() {
      return Err(RailError::message("native crate root must not be a symlink"));
    }
    let crate_root_spelling = declared.path.resolve(workspace_root);
    let namespace_spelling = crate_root_spelling
      .parent()
      .ok_or_else(|| RailError::message("native crate root has no source namespace"))?;
    let namespace = crate::utils::canonicalize_existing(namespace_spelling)?;
    let crate_root = crate::utils::canonicalize_existing(&crate_root_spelling)?;
    let crate_root_relative = native_relative_path(
      crate_root
        .strip_prefix(&namespace)
        .map_err(|_| RailError::message("native crate root escaped its source namespace"))?,
    )?;
    if namespace != self.source_root
      || namespace_spelling != self.source_root_spelling
      || crate_root.parent() != Some(namespace.as_path())
      || crate_root_relative != self.crate_root
      || ObservationPath::capture(&namespace, workspace_root, workspace_root) != self.source_state.root
    {
      return Err(RailError::message(
        "native source namespace changed before the restore commit",
      ));
    }

    let package_binding = match &self.source_state.root {
      ObservationPath::Repository(_) => None,
      ObservationPath::Host(_) => Some(NativePackageBinding::capture(&namespace, namespace_spelling)?),
    };
    if package_binding != self.package_binding {
      return Err(RailError::message(
        "native package binding changed before the restore commit",
      ));
    }

    let mut dependencies = Vec::with_capacity(observation.dependency_artifacts.len());
    for (name, artifact) in &observation.dependency_artifacts {
      if artifact.symlink_target.is_some() {
        return Err(RailError::message("native dependency artifact must not be a symlink"));
      }
      let path = artifact.path.resolve(workspace_root);
      dependencies.push((
        format!("dependency:{name}:{}", artifact_name(&path)?),
        path,
        artifact.executable,
      ));
    }
    dependencies.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    if dependencies.windows(2).any(|pair| pair[0].0 == pair[1].0)
      || self.guard.entries.len()
        != self
          .source_state
          .entries
          .len()
          .checked_add(dependencies.len())
          .ok_or_else(|| RailError::message("native restore guard count overflowed"))?
    {
      return Err(RailError::message("native restore guard paths are invalid"));
    }

    for expected in &self.guard.entries {
      let (path, executable) = match self
        .source_state
        .entries
        .binary_search_by(|entry| entry.path.as_str().cmp(&expected.path))
      {
        Ok(_) => (self.source_root.join(&expected.path), None),
        Err(_) => {
          let index = dependencies
            .binary_search_by(|entry| entry.0.as_str().cmp(&expected.path))
            .map_err(|_| RailError::message("native restore guard path is not an action input"))?;
          (dependencies[index].1.clone(), Some(dependencies[index].2))
        }
      };
      let metadata = fs::symlink_metadata(&path)?;
      if crate::utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::message(
          "native action input became a symlink before the restore commit",
        ));
      }
      let current = native_metadata_guard(&path, &metadata)?;
      if current != expected.metadata
        || executable.is_some_and(|expected| expected != executable_mode_from_guard(&current))
      {
        return Err(RailError::message(
          "native action input changed before the restore commit",
        ));
      }
    }

    self.revalidate_generated_before_restore_commit(workspace_root, std::env::var_os("OUT_DIR"))?;
    self.revalidate_native_searches_before_restore_commit(observation, workspace_root)?;
    self.revalidate_pathless_extern_searches_before_restore_commit(observation, workspace_root)?;

    let environment_names = self
      .approved_environment
      .entries
      .iter()
      .map(|entry| entry.name.clone())
      .collect::<Vec<_>>();
    let (environment, bytes_hashed) = capture_approved_environment(
      workspace_root,
      workspace_root_spelling,
      self,
      &environment_names,
      Instant::now(),
    )?;
    if environment != self.approved_environment {
      return Err(RailError::message(
        "native compiler environment changed before the restore commit",
      ));
    }
    Ok(bytes_hashed)
  }

  fn revalidate_generated_before_restore_commit(
    &self,
    workspace_root: &Path,
    current_root: Option<OsString>,
  ) -> RailResult<()> {
    match (&self.generated, current_root) {
      (None, None) => {}
      (Some(generated), Some(root)) => {
        let root_spelling = PathBuf::from(root);
        let metadata = fs::symlink_metadata(&root_spelling)?;
        if !root_spelling.is_absolute()
          || !metadata.is_dir()
          || crate::utils::is_symlink_or_reparse(&metadata)
          || root_spelling != generated.root_spelling
        {
          return Err(RailError::message(
            "native generated namespace changed before the restore commit",
          ));
        }
        let root = crate::utils::canonicalize_existing(&root_spelling)?;
        if root != generated.root
          || ObservationPath::capture(&root, workspace_root, workspace_root) != generated.state.root
        {
          return Err(RailError::message(
            "native generated namespace changed before the restore commit",
          ));
        }
        revalidate_captured_namespace(generated, "generated")?;
      }
      _ => {
        return Err(RailError::message(
          "native generated namespace changed before the restore commit",
        ));
      }
    }
    Ok(())
  }

  fn revalidate_native_searches_before_restore_commit(
    &self,
    observation: &RawCompilerInvocation,
    workspace_root: &Path,
  ) -> RailResult<()> {
    let current_directory = std::env::current_dir()?;
    let mut roots = Vec::<(PathBuf, PathBuf)>::new();
    for spelling in native_search_paths(&observation.compiler_arguments, &current_directory, workspace_root)? {
      let metadata = fs::symlink_metadata(&spelling)?;
      if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::message(
          "native library search namespace changed before the restore commit",
        ));
      }
      let root = crate::utils::canonicalize_existing(&spelling)?;
      if self.generated.as_ref().is_some_and(|generated| generated.root == root)
        || roots.iter().any(|(captured, _)| captured == &root)
      {
        continue;
      }
      roots.push((root, spelling));
    }
    if roots.len() != self.native_searches.len() {
      return Err(RailError::message(
        "native library search namespaces changed before the restore commit",
      ));
    }
    for ((root, spelling), captured) in roots.iter().zip(&self.native_searches) {
      if root != &captured.root
        || spelling != &captured.root_spelling
        || ObservationPath::capture(root, workspace_root, workspace_root) != captured.state.root
      {
        return Err(RailError::message(
          "native library search namespace changed before the restore commit",
        ));
      }
      revalidate_captured_namespace(captured, "native library search")?;
    }
    Ok(())
  }

  fn revalidate_pathless_extern_searches_before_restore_commit(
    &self,
    observation: &RawCompilerInvocation,
    workspace_root: &Path,
  ) -> RailResult<()> {
    if pathless_extern_names(&observation.compiler_arguments).is_empty() {
      return if self.pathless_extern_searches.is_empty() {
        Ok(())
      } else {
        Err(RailError::message(
          "pathless compiler extern search namespaces changed before the restore commit",
        ))
      };
    }
    let current_directory = std::env::current_dir()?;
    let roots = pathless_extern_search_roots(&observation.compiler_arguments, &current_directory, workspace_root)?;
    if roots.len() != self.pathless_extern_searches.len() {
      return Err(RailError::message(
        "pathless compiler extern search namespaces changed before the restore commit",
      ));
    }
    for ((root, spelling), captured) in roots.iter().zip(&self.pathless_extern_searches) {
      if root != &captured.root || spelling != &captured.root_spelling {
        return Err(RailError::message(
          "pathless compiler extern search namespace changed before the restore commit",
        ));
      }
      revalidate_pathless_extern_search(captured)?;
    }
    Ok(())
  }

  fn guard_identity(&self) -> RailResult<String> {
    Ok(format!(
      "sha256:{}",
      ContentDigest::sha256(&serde_json::to_vec(&(
        &self.guard,
        self.generated.as_ref().map(|generated| &generated.guard),
        self
          .native_searches
          .iter()
          .map(|captured| &captured.guard)
          .collect::<Vec<_>>(),
        self
          .pathless_extern_searches
          .iter()
          .map(|captured| &captured.guard)
          .collect::<Vec<_>>(),
      ))?)
    ))
  }

  /// Bind exact compiler outputs to the physical source namespace rustc observed.
  fn compilation_root_identity(&self) -> String {
    let mut framed = Vec::from(&b"cargo-rail-native-compilation-root\0"[..]);
    append_frame(
      &mut framed,
      b"source-root",
      self.source_root.as_os_str().as_encoded_bytes(),
    );
    append_frame(
      &mut framed,
      b"source-root-spelling",
      self.source_root_spelling.as_os_str().as_encoded_bytes(),
    );
    if let Some(package) = &self.package_binding {
      append_frame(
        &mut framed,
        b"package-root",
        package.root.as_os_str().as_encoded_bytes(),
      );
      append_frame(
        &mut framed,
        b"package-root-spelling",
        package.spelling.as_os_str().as_encoded_bytes(),
      );
    }
    if let Some(generated) = &self.generated {
      append_frame(
        &mut framed,
        b"generated-root",
        generated.root.as_os_str().as_encoded_bytes(),
      );
      append_frame(
        &mut framed,
        b"generated-root-spelling",
        generated.root_spelling.as_os_str().as_encoded_bytes(),
      );
    }
    for native in &self.native_searches {
      append_frame(
        &mut framed,
        b"native-search-root",
        native.root.as_os_str().as_encoded_bytes(),
      );
      append_frame(
        &mut framed,
        b"native-search-root-spelling",
        native.root_spelling.as_os_str().as_encoded_bytes(),
      );
    }
    crate::instrumentation::record_hash(framed.len());
    format!("sha256:{}", ContentDigest::sha256(&framed))
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
    let mut generated_paths = Vec::new();
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
      let source_relative = absolute.strip_prefix(&self.source_root).ok();
      let generated_relative = self
        .generated
        .as_ref()
        .and_then(|generated| absolute.strip_prefix(&generated.root).ok());
      let (relative, state, selected_paths) = match (source_relative, generated_relative, &self.generated) {
        (Some(_), Some(_), _) => {
          return Err(RailError::message(
            "compiler-selected input belongs to overlapping source and generated namespaces",
          ));
        }
        (Some(relative), None, _) => (relative, &self.source_state, &mut source_paths),
        (None, Some(relative), Some(generated)) => (relative, &generated.state, &mut generated_paths),
        _ => {
          return Err(RailError::message(format!(
            "compiler selected input '{}' outside its complete namespaces",
            absolute.display()
          )));
        }
      };
      let relative = native_relative_path(relative)?;
      let index = state
        .entries
        .binary_search_by(|entry| entry.path.as_str().cmp(&relative))
        .map_err(|_| RailError::message("compiler-selected input is absent from its captured namespace"))?;
      let NativeSourceEntryKind::RegularFile {
        content_digest, mode, ..
      } = &state.entries[index].kind
      else {
        return Err(RailError::message("compiler selected a non-file input entry"));
      };
      if content_digest != &observed.content_digest
        || source_mode_executable(*mode) != observed.executable
        || observed.symlink_target.is_some()
      {
        return Err(RailError::message(
          "compiler-selected source does not match captured SourceState",
        ));
      }
      selected_paths.push(relative);
    }
    source_paths.sort_unstable();
    source_paths.dedup();
    generated_paths.sort_unstable();
    generated_paths.dedup();

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
      version: 5,
      complete: true,
      source_paths,
      generated_paths,
      dependency_names,
      environment_names,
      linker: None,
    })
  }

  fn validates_witness(&self, witness: &NativeCompilerWitness, observation: &RawCompilerInvocation) -> bool {
    let mut dependencies = observation
      .dependency_artifacts
      .iter()
      .map(|(name, _)| name.as_str())
      .collect::<Vec<_>>();
    dependencies.sort_unstable();
    witness.version == 5
      && witness.complete
      && !witness.source_paths.is_empty()
      && strictly_sorted_unique_strings(&witness.source_paths)
      && strictly_sorted_unique_strings(&witness.generated_paths)
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
      && witness.generated_paths.iter().all(|path| {
        self.generated.as_ref().is_some_and(|generated| {
          generated
            .state
            .entries
            .binary_search_by(|entry| entry.path.as_str().cmp(path))
            .ok()
            .is_some_and(|index| {
              matches!(
                generated.state.entries[index].kind,
                NativeSourceEntryKind::RegularFile { .. }
              )
            })
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
      && witness
        .linker
        .as_ref()
        .is_none_or(|witness| validate_linker_witness(witness).is_ok())
  }
}

fn validate_linker_witness(witness: &LinkerWitness) -> RailResult<()> {
  match witness {
    LinkerWitness::Apple(witness) => validate_apple_linker_witness(witness),
    LinkerWitness::Elf(witness) => validate_elf_linker_witness(witness),
  }
}

fn compiler_owned_source_exclusions(source_root: &Path) -> RailResult<Vec<PathBuf>> {
  let target = source_root.join("target");
  match fs::symlink_metadata(&target) {
    Ok(metadata) if metadata.is_dir() && !crate::utils::is_symlink_or_reparse(&metadata) => {
      Ok(vec![crate::utils::canonicalize_existing(&target)?])
    }
    Ok(_) => Err(RailError::message(
      "standard Cargo target root is not one real directory",
    )),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
    Err(error) => Err(error.into()),
  }
}

fn validate_apple_linker_witness(witness: &AppleLinkerWitness) -> RailResult<()> {
  if witness.version != 5
    || witness.certificate_version.is_empty()
    || witness.certificate_version.len() > 256
    || witness.certificate_version.as_bytes().contains(&0)
    || witness.found.len() > MAX_LINK_INPUTS
    || witness.missing.len() > MAX_LINK_INPUTS
    || witness.endogenous_objects == 0
    || !strictly_sorted_unique_strings(&witness.missing)
    || !strictly_sorted_unique_strings(&witness.dependency_archives)
  {
    return Err(RailError::message("Apple linker witness is invalid"));
  }
  let mut path_bytes = 0usize;
  let mut previous = None::<&str>;
  for file in std::iter::once(&witness.driver)
    .chain(std::iter::once(&witness.linker))
    .chain(&witness.found)
  {
    if file.path.is_empty()
      || !Path::new(&file.path).is_absolute()
      || file.path.as_bytes().contains(&0)
      || file.canonical_path.is_empty()
      || !Path::new(&file.canonical_path).is_absolute()
      || file.canonical_path.as_bytes().contains(&0)
      || file.bytes == 0
      || validate_sha256(&file.content_digest).is_err()
      || file.mode & !0o777 != 0
      || file.mode & 0o400 == 0
    {
      return Err(RailError::message("Apple linker witness contains an invalid file"));
    }
    path_bytes = path_bytes
      .saturating_add(file.path.len())
      .saturating_add(file.canonical_path.len());
  }
  for file in &witness.found {
    if previous.is_some_and(|previous| previous >= file.path.as_str()) {
      return Err(RailError::message(
        "Apple linker found inputs are not strictly sorted and unique",
      ));
    }
    previous = Some(&file.path);
  }
  for path in &witness.missing {
    if !Path::new(path).is_absolute() || path.as_bytes().contains(&0) {
      return Err(RailError::message(
        "Apple linker witness contains an invalid missing path",
      ));
    }
    path_bytes = path_bytes.saturating_add(path.len());
  }
  if path_bytes > MAX_LINK_PATH_BYTES
    || witness
      .dependency_archives
      .iter()
      .any(|name| name.is_empty() || name.as_bytes().contains(&0))
  {
    return Err(RailError::message("Apple linker witness exceeds its bounds"));
  }
  Ok(())
}

fn validate_apple_linker_generations(
  generations: &LinkerGenerationWitness,
  witness: &AppleLinkerWitness,
) -> RailResult<()> {
  if generations.version != 1
    || generations.found.len() != witness.found.len()
    || validate_sha256(&generations.installation_authority).is_err()
    || std::iter::once(&generations.driver)
      .chain(std::iter::once(&generations.linker))
      .chain(&generations.found)
      .any(|generation| validate_sha256(generation).is_err())
  {
    return Err(RailError::message("Apple linker generation witness is invalid"));
  }
  Ok(())
}

fn validate_elf_linker_witness(witness: &ElfLinkerWitness) -> RailResult<()> {
  if witness.version != 1
    || witness.found.len() > MAX_LINK_INPUTS
    || witness.missing.len() > MAX_LINK_INPUTS
    || witness.endogenous_objects == 0
    || !strictly_sorted_unique_strings(&witness.missing)
    || !strictly_sorted_unique_strings(&witness.dependency_archives)
  {
    return Err(RailError::message("ELF linker witness is invalid"));
  }
  let mut path_bytes = 0usize;
  let mut previous = None::<&str>;
  for file in std::iter::once(&witness.driver)
    .chain(std::iter::once(&witness.linker))
    .chain(&witness.found)
  {
    validate_link_file(file)?;
    path_bytes = path_bytes
      .saturating_add(file.path.len())
      .saturating_add(file.canonical_path.len());
  }
  for file in &witness.found {
    if previous.is_some_and(|previous| previous >= file.path.as_str()) {
      return Err(RailError::message(
        "ELF linker found inputs are not strictly sorted and unique",
      ));
    }
    previous = Some(&file.path);
  }
  for path in &witness.missing {
    if !Path::new(path).is_absolute() || path.as_bytes().contains(&0) {
      return Err(RailError::message(
        "ELF linker witness contains an invalid missing path",
      ));
    }
    path_bytes = path_bytes.saturating_add(path.len());
  }
  if path_bytes > MAX_LINK_PATH_BYTES
    || witness
      .dependency_archives
      .iter()
      .any(|name| name.is_empty() || name.as_bytes().contains(&0))
  {
    return Err(RailError::message("ELF linker witness exceeds its bounds"));
  }
  Ok(())
}

fn validate_link_file(file: &LinkFileWitness) -> RailResult<()> {
  if file.path.is_empty()
    || !Path::new(&file.path).is_absolute()
    || file.path.as_bytes().contains(&0)
    || file.canonical_path.is_empty()
    || !Path::new(&file.canonical_path).is_absolute()
    || file.canonical_path.as_bytes().contains(&0)
    || file.bytes == 0
    || validate_sha256(&file.content_digest).is_err()
    || file.mode & !0o777 != 0
    || file.mode & 0o400 == 0
  {
    return Err(RailError::message("linker witness contains an invalid file"));
  }
  Ok(())
}

fn validate_elf_linker_generations(
  generations: &LinkerGenerationWitness,
  witness: &ElfLinkerWitness,
) -> RailResult<()> {
  if generations.version != 1
    || generations.found.len() != witness.found.len()
    || validate_sha256(&generations.installation_authority).is_err()
    || std::iter::once(&generations.driver)
      .chain(std::iter::once(&generations.linker))
      .chain(&generations.found)
      .any(|generation| validate_sha256(generation).is_err())
  {
    return Err(RailError::message("ELF linker generation witness is invalid"));
  }
  Ok(())
}

fn platform_linker_witness_is_valid(
  observation: &RawCompilerInvocation,
  witness: &NativeCompilerWitness,
  generations: Option<&LinkerGenerationWitness>,
) -> bool {
  if apple_linked_observation(observation) {
    match (&witness.linker, generations) {
      (Some(LinkerWitness::Apple(linker)), Some(generations)) => {
        validate_apple_linker_witness(linker).is_ok() && validate_apple_linker_generations(generations, linker).is_ok()
      }
      (Some(LinkerWitness::Apple(linker)), None) => validate_apple_linker_witness(linker).is_ok(),
      _ => false,
    }
  } else if elf_linked_observation(observation) {
    match (&witness.linker, generations) {
      (Some(LinkerWitness::Elf(linker)), Some(generations)) => {
        validate_elf_linker_witness(linker).is_ok() && validate_elf_linker_generations(generations, linker).is_ok()
      }
      (Some(LinkerWitness::Elf(linker)), None) => validate_elf_linker_witness(linker).is_ok(),
      _ => false,
    }
  } else {
    witness.linker.is_none() && generations.is_none()
  }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn installation_authority_identity(authority: &str) -> String {
  format!("sha256:{authority}")
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn linker_generation_identity(path: &Path) -> Option<String> {
  crate::utils::stable_file_generation(path).map(|generation| {
    sha256_identity(
      "sha256:",
      b"cargo-rail-linker-file-generation\0",
      &[(b"generation", &generation)],
    )
  })
}

#[cfg(target_os = "macos")]
fn capture_apple_linker_witness(
  observation: &RawCompilerInvocation,
  output_paths: &NativeOutputPaths,
  certificate: &Path,
  driver_inputs: &Path,
  installation_authority: Option<&str>,
) -> RailResult<(AppleLinkerWitness, Option<LinkerGenerationWitness>, u64)> {
  let bytes = read_bounded(certificate, MAX_APPLE_LINK_CERTIFICATE_BYTES as usize)?;
  let (certificate_version, entries) = parse_apple_link_certificate(&bytes)?;
  let driver_inputs = read_apple_link_driver_inputs(driver_inputs)?;
  let linked = output_paths
    .artifacts
    .iter()
    .filter(|artifact| !matches!(artifact.role, NativeOutputRole::Metadata | NativeOutputRole::Rlib))
    .collect::<Vec<_>>();
  let [linked] = linked.as_slice() else {
    return Err(RailError::message(
      "Apple linker evidence requires one linked compiler output",
    ));
  };
  let linked_path = crate::utils::canonicalize_existing(&linked.path)?;
  let linked_name = linked_path
    .file_name()
    .and_then(OsStr::to_str)
    .ok_or_else(|| RailError::message("Apple linked output has no UTF-8 file name"))?;
  let object_prefix = apple_rustc_object_prefix(linked.role, linked_name)
    .ok_or_else(|| RailError::message("Apple linked output has no rustc object prefix"))?;
  let mut dependency_by_file = BTreeMap::new();
  for (name, artifact) in &observation.dependency_artifacts {
    let file_name = observation_path_basename(&artifact.path)
      .ok_or_else(|| RailError::message("rustc dependency artifact has no UTF-8 file name"))?;
    if dependency_by_file
      .insert(file_name.to_string(), name.as_str())
      .is_some()
    {
      return Err(RailError::message(
        "Apple linked action has ambiguous dependency artifact names",
      ));
    }
  }

  let mut found_paths = BTreeSet::new();
  let mut missing = BTreeSet::new();
  let mut outputs = Vec::new();
  let mut endogenous_objects = 0u32;
  let mut endogenous_archives = 0u32;
  let mut dependency_archives = BTreeSet::new();
  for (opcode, value) in entries {
    let path = PathBuf::from(&value);
    match opcode {
      0x10 => {
        if !path.is_absolute() {
          return Err(RailError::message("Apple linker reported a relative found input"));
        }
        let file_name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
        let selected_by_rustc = observation.compiler_arguments.iter().any(|argument| {
          argument
            .as_bytes()
            .windows(value.len())
            .any(|window| window == value.as_bytes())
        });
        let certified_driver_input = driver_inputs.direct.contains(&path) && !selected_by_rustc;
        let certified_generated_input = driver_inputs.generated.contains(&path) && !selected_by_rustc;
        let generated_object = path.extension() == Some(OsStr::new("o"))
          && (certified_driver_input || certified_generated_input)
          && (path.parent() == linked_path.parent() && file_name.starts_with(&object_prefix)
            || path
              .parent()
              .and_then(Path::file_name)
              .and_then(OsStr::to_str)
              .is_some_and(is_rustc_temporary_name));
        if generated_object {
          endogenous_objects = endogenous_objects
            .checked_add(1)
            .ok_or_else(|| RailError::message("Apple linker endogenous input count overflow"))?;
          continue;
        }
        let temporary_archive = path
          .parent()
          .and_then(Path::file_name)
          .and_then(OsStr::to_str)
          .is_some_and(is_rustc_temporary_name)
          && matches!(path.extension().and_then(OsStr::to_str), Some("rlib" | "a"));
        if temporary_archive {
          if !certified_driver_input {
            return Err(RailError::message(
              "Apple linker temporary archive was not certified by the selected driver invocation",
            ));
          }
          if let Some(dependency) = dependency_by_file.get(file_name) {
            dependency_archives.insert((*dependency).to_string());
          } else {
            endogenous_archives = endogenous_archives
              .checked_add(1)
              .ok_or_else(|| RailError::message("Apple linker endogenous archive count overflow"))?;
          }
          continue;
        }
        if value.as_bytes().contains(&0) {
          return Err(RailError::message("Apple linker found input path is invalid"));
        }
        found_paths.insert(path);
      }
      0x11 => {
        if !path.is_absolute()
          || value.as_bytes().contains(&0)
          || !fs::symlink_metadata(&path).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
          return Err(RailError::message("Apple linker missing input is not currently absent"));
        }
        missing.insert(value);
      }
      0x40 => outputs.push(path),
      _ => {
        return Err(RailError::message(
          "Apple linker certificate contains an unknown opcode",
        ));
      }
    }
  }
  if outputs.len() != 1 || outputs[0] != linked_path || endogenous_objects == 0 {
    return Err(RailError::message(
      "Apple linker certificate does not bind the exact linked output",
    ));
  }
  if found_paths.len().saturating_add(missing.len()) > MAX_LINK_INPUTS {
    return Err(RailError::message("Apple linker certificate exceeds its input bound"));
  }

  let started = Instant::now();
  let mut budget = NativeCaptureBudget::new(NATIVE_CAPTURE_LIMITS);
  let (driver, driver_generation) = capture_link_file(Path::new("/usr/bin/cc"), started, &mut budget)?;
  let linker_path = resolve_selected_apple_linker()?;
  let (linker, linker_generation) = capture_link_file(&linker_path, started, &mut budget)?;
  let found = found_paths
    .into_iter()
    .map(|path| {
      capture_link_file(&path, started, &mut budget).map_err(|error| {
        RailError::message(format!(
          "Apple linker input '{}' is unavailable: {error}",
          path.display()
        ))
      })
    })
    .collect::<RailResult<Vec<_>>>()?;
  let (found, found_generations): (Vec<_>, Vec<_>) = found.into_iter().unzip();
  let witness = AppleLinkerWitness {
    version: 5,
    certificate_version,
    driver,
    linker,
    found,
    missing: missing.into_iter().collect(),
    endogenous_objects,
    endogenous_archives,
    dependency_archives: dependency_archives.into_iter().collect(),
  };
  validate_apple_linker_witness(&witness)?;
  let generations = installation_authority.and_then(|authority| {
    Some(LinkerGenerationWitness {
      version: 1,
      installation_authority: installation_authority_identity(authority),
      driver: driver_generation?,
      linker: linker_generation?,
      found: found_generations.into_iter().collect::<Option<Vec<_>>>()?,
    })
  });
  if let Some(generations) = &generations {
    validate_apple_linker_generations(generations, &witness)?;
  }
  Ok((witness, generations, budget.bytes_hashed))
}

#[cfg(target_os = "linux")]
fn capture_elf_linker_witness(
  observation: &RawCompilerInvocation,
  output_paths: &NativeOutputPaths,
  dependencies: &Path,
  driver_inputs: &Path,
  installation_authority: Option<&str>,
) -> RailResult<(ElfLinkerWitness, Option<LinkerGenerationWitness>, u64)> {
  let dependency_metadata = fs::metadata(dependencies).map_err(|error| {
    RailError::message(format!(
      "ELF linker dependency file '{}' is unavailable: {error}",
      dependencies.display()
    ))
  })?;
  if dependency_metadata.len() > MAX_ELF_LINK_DEPENDENCY_BYTES {
    return Err(RailError::message("ELF linker dependency file exceeds its byte bound"));
  }
  let evidence = read_elf_link_driver_evidence(driver_inputs).map_err(|error| {
    RailError::message(format!(
      "ELF linker driver evidence '{}' is unavailable: {error}",
      driver_inputs.display()
    ))
  })?;
  let current_directory = Path::new(&evidence.current_directory);
  let (target, dependencies) =
    crate::compiler::observation::makefile_dependency_paths(dependencies, current_directory)?;
  let linked = output_paths
    .artifacts
    .iter()
    .filter(|artifact| !matches!(artifact.role, NativeOutputRole::Metadata | NativeOutputRole::Rlib))
    .collect::<Vec<_>>();
  let [linked] = linked.as_slice() else {
    return Err(RailError::message(
      "ELF linker evidence requires one linked compiler output",
    ));
  };
  let linked_path = crate::utils::canonicalize_existing(&linked.path).map_err(|error| {
    RailError::message(format!(
      "ELF linked output '{}' is unavailable: {error}",
      linked.path.display()
    ))
  })?;
  let dependency_target = crate::utils::canonicalize_existing(&target).map_err(|error| {
    RailError::message(format!(
      "ELF linker dependency target '{}' is unavailable: {error}",
      target.display()
    ))
  })?;
  if dependency_target != linked_path {
    return Err(RailError::message(
      "ELF linker dependency file does not bind the exact linked output",
    ));
  }
  let linked_parent = linked_path
    .parent()
    .ok_or_else(|| RailError::message("ELF linked output has no parent directory"))?;

  let direct_inputs = evidence
    .direct_inputs
    .iter()
    .map(PathBuf::from)
    .collect::<BTreeSet<_>>();
  let mut dependency_by_file = BTreeMap::new();
  for (name, artifact) in &observation.dependency_artifacts {
    let file_name = observation_path_basename(&artifact.path)
      .ok_or_else(|| RailError::message("rustc dependency artifact has no UTF-8 file name"))?;
    if dependency_by_file
      .insert(file_name.to_string(), name.as_str())
      .is_some()
    {
      return Err(RailError::message(
        "ELF linked action has ambiguous dependency artifact names",
      ));
    }
  }

  let mut found_paths = evidence.tool_inputs.iter().map(PathBuf::from).collect::<BTreeSet<_>>();
  let mut dependency_archives = BTreeSet::new();
  let mut endogenous_objects = 0u32;
  for dependency in dependencies {
    let spelling = if dependency.is_absolute() {
      dependency
    } else {
      current_directory.join(dependency)
    };
    let file_name = spelling.file_name().and_then(OsStr::to_str).unwrap_or_default();
    let rustc_temporary_parent = spelling
      .parent()
      .and_then(Path::file_name)
      .and_then(OsStr::to_str)
      .is_some_and(is_rustc_temporary_name);
    let rustc_temporary_input = rustc_temporary_parent
      && spelling
        .parent()
        .and_then(Path::parent)
        .and_then(|parent| crate::utils::canonicalize_existing(parent).ok())
        .is_some_and(|parent| parent == linked_parent);
    let rustc_codegen_object = file_name.ends_with(".rcgu.o")
      && direct_inputs.contains(&spelling)
      && spelling
        .parent()
        .and_then(|parent| crate::utils::canonicalize_existing(parent).ok())
        .is_some_and(|parent| parent == linked_parent);
    if rustc_temporary_input || rustc_codegen_object {
      endogenous_objects = endogenous_objects
        .checked_add(1)
        .ok_or_else(|| RailError::message("ELF linker endogenous input count overflow"))?;
      continue;
    }
    let canonical = crate::utils::canonicalize_existing(&spelling).map_err(|error| {
      RailError::message(format!(
        "ELF linker dependency '{}' is unavailable: {error}",
        spelling.display()
      ))
    })?;
    if canonical == linked_path {
      continue;
    }
    if matches!(spelling.extension().and_then(OsStr::to_str), Some("rlib" | "a"))
      && let Some(dependency) = dependency_by_file.get(file_name)
    {
      dependency_archives.insert((*dependency).to_string());
      continue;
    }
    found_paths.insert(spelling);
  }
  if endogenous_objects == 0 {
    return Err(RailError::message(
      "ELF linker dependency file contains no certified rustc object",
    ));
  }

  // Positive linker dependency files do not record failed search attempts.
  // Bind every same-name candidate in every selected driver/linker search
  // directory and record every absence. Any later path-selection change then
  // invalidates the witness before outputs can be restored.
  let selected_names = found_paths
    .iter()
    .filter_map(|path| path.file_name().and_then(OsStr::to_str))
    .map(str::to_string)
    .collect::<BTreeSet<_>>();
  let mut missing = BTreeSet::new();
  for directory in evidence.search_directories.iter().map(PathBuf::from) {
    for name in &selected_names {
      let candidate = directory.join(name);
      match fs::symlink_metadata(&candidate) {
        Ok(metadata) if metadata.is_file() && !crate::utils::is_symlink_or_reparse(&metadata) => {
          found_paths.insert(candidate);
        }
        Ok(metadata) if crate::utils::is_symlink_or_reparse(&metadata) => {
          found_paths.insert(candidate);
        }
        Ok(_) => {
          return Err(RailError::message("ELF linker search candidate is not a file"));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
          let value = candidate
            .to_str()
            .ok_or_else(|| RailError::message("ELF linker search path is not valid UTF-8"))?;
          missing.insert(value.to_string());
        }
        Err(error) => return Err(error.into()),
      }
      if found_paths.len().saturating_add(missing.len()) > MAX_LINK_INPUTS {
        return Err(RailError::message("ELF linker witness exceeds its input bound"));
      }
    }
  }

  let started = Instant::now();
  let mut budget = NativeCaptureBudget::new(NATIVE_CAPTURE_LIMITS);
  let capture_elf_file = |path: &Path, budget: &mut NativeCaptureBudget| {
    capture_link_file(path, started, budget)
      .map_err(|error| RailError::message(format!("ELF linker input '{}' is unavailable: {error}", path.display())))
  };
  let (driver, driver_generation) = capture_elf_file(Path::new(&evidence.driver), &mut budget)?;
  let (linker, linker_generation) = capture_elf_file(Path::new(&evidence.linker), &mut budget)?;
  found_paths.remove(Path::new(&evidence.driver));
  found_paths.remove(Path::new(&evidence.linker));
  let found = found_paths
    .into_iter()
    .map(|path| capture_elf_file(&path, &mut budget))
    .collect::<RailResult<Vec<_>>>()?;
  let (mut found, mut found_generations): (Vec<_>, Vec<_>) = found.into_iter().unzip();
  let mut ordered = found.into_iter().zip(found_generations).collect::<Vec<_>>();
  ordered.sort_unstable_by(|left, right| left.0.path.cmp(&right.0.path));
  (found, found_generations) = ordered.into_iter().unzip();
  let witness = ElfLinkerWitness {
    version: 1,
    driver,
    linker,
    found,
    missing: missing.into_iter().collect(),
    endogenous_objects,
    dependency_archives: dependency_archives.into_iter().collect(),
  };
  validate_elf_linker_witness(&witness)?;
  let generations = installation_authority.and_then(|authority| {
    Some(LinkerGenerationWitness {
      version: 1,
      installation_authority: installation_authority_identity(authority),
      driver: driver_generation?,
      linker: linker_generation?,
      found: found_generations.into_iter().collect::<Option<Vec<_>>>()?,
    })
  });
  if let Some(generations) = &generations {
    validate_elf_linker_generations(generations, &witness)?;
  }
  Ok((witness, generations, budget.bytes_hashed))
}

#[cfg(target_os = "linux")]
fn read_elf_link_driver_evidence(path: &Path) -> RailResult<ElfLinkDriverEvidence> {
  let bytes = read_bounded(path, MAX_ELF_LINK_DEPENDENCY_BYTES as usize)?;
  let evidence: ElfLinkDriverEvidence = serde_json::from_slice(&bytes)?;
  let path_count = evidence
    .direct_inputs
    .len()
    .saturating_add(evidence.tool_inputs.len())
    .saturating_add(evidence.search_directories.len());
  if evidence.version != 1
    || path_count > MAX_LINK_INPUTS
    || serde_json::to_vec(&evidence)? != bytes
    || !Path::new(&evidence.current_directory).is_absolute()
    || !Path::new(&evidence.driver).is_absolute()
    || !Path::new(&evidence.linker).is_absolute()
    || !strictly_sorted_unique_strings(&evidence.direct_inputs)
    || !strictly_sorted_unique_strings(&evidence.tool_inputs)
    || !strictly_sorted_unique_strings(&evidence.search_directories)
    || evidence
      .direct_inputs
      .iter()
      .chain(&evidence.tool_inputs)
      .chain(&evidence.search_directories)
      .any(|path| !Path::new(path).is_absolute() || path.as_bytes().contains(&0))
  {
    return Err(RailError::message("ELF linker driver evidence is invalid"));
  }
  Ok(evidence)
}

fn complete_linked_witness(
  observation: &RawCompilerInvocation,
  output_paths: &NativeOutputPaths,
  certificate: &Path,
  driver_inputs: &Path,
  pre_link_action: &str,
  witness: &mut NativeCompilerWitness,
  installation_authority: Option<&str>,
) -> RailResult<(Option<String>, Option<LinkerGenerationWitness>, u64)> {
  if !linked_observation(observation) {
    return Ok((None, None, 0));
  }
  #[cfg(target_os = "macos")]
  {
    let (apple_linker, generations, bytes_hashed) = capture_apple_linker_witness(
      observation,
      output_paths,
      certificate,
      driver_inputs,
      installation_authority,
    )?;
    witness.linker = Some(LinkerWitness::Apple(apple_linker));
    Ok((
      Some(link_candidate_selector(pre_link_action)?),
      generations,
      bytes_hashed,
    ))
  }
  #[cfg(target_os = "linux")]
  {
    let (elf_linker, generations, bytes_hashed) = capture_elf_linker_witness(
      observation,
      output_paths,
      certificate,
      driver_inputs,
      installation_authority,
    )?;
    witness.linker = Some(LinkerWitness::Elf(elf_linker));
    Ok((
      Some(link_candidate_selector(pre_link_action)?),
      generations,
      bytes_hashed,
    ))
  }
  #[cfg(not(any(target_os = "macos", target_os = "linux")))]
  {
    let _ = (
      output_paths,
      certificate,
      driver_inputs,
      pre_link_action,
      witness,
      installation_authority,
    );
    Err(RailError::message(
      "linked compiler action is unavailable on this platform",
    ))
  }
}

#[cfg(target_os = "macos")]
fn read_apple_link_driver_evidence(path: &Path) -> RailResult<AppleLinkDriverEvidence> {
  let bytes = read_bounded(path, MAX_APPLE_LINK_CERTIFICATE_BYTES as usize)?;
  let evidence: AppleLinkDriverEvidence = serde_json::from_slice(&bytes)?;
  let path_count = evidence
    .direct_inputs
    .len()
    .saturating_add(evidence.temporary_directories.len())
    .saturating_add(evidence.preexisting_paths.len())
    .saturating_add(evidence.generated_inputs.len());
  if evidence.version != APPLE_LINK_DRIVER_EVIDENCE_VERSION
    || path_count > MAX_LINK_INPUTS
    || !strictly_sorted_unique_strings(&evidence.direct_inputs)
    || !strictly_sorted_unique_strings(&evidence.temporary_directories)
    || !strictly_sorted_unique_strings(&evidence.preexisting_paths)
    || !strictly_sorted_unique_strings(&evidence.generated_inputs)
    || serde_json::to_vec(&evidence)? != bytes
  {
    return Err(RailError::message("Apple linker driver-input certificate is invalid"));
  }
  let mut path_bytes = 0usize;
  for input in evidence
    .direct_inputs
    .iter()
    .chain(&evidence.temporary_directories)
    .chain(&evidence.preexisting_paths)
    .chain(&evidence.generated_inputs)
  {
    if input.is_empty() || input.as_bytes().contains(&0) || !Path::new(input).is_absolute() {
      return Err(RailError::message(
        "Apple linker driver-input certificate contains an invalid path",
      ));
    }
    path_bytes = path_bytes.saturating_add(input.len());
  }
  if path_bytes > MAX_LINK_PATH_BYTES {
    return Err(RailError::message(
      "Apple linker driver-input certificate exceeds its path bound",
    ));
  }
  let temporary_directories = evidence
    .temporary_directories
    .iter()
    .map(PathBuf::from)
    .collect::<BTreeSet<_>>();
  let preexisting_paths = evidence
    .preexisting_paths
    .iter()
    .map(PathBuf::from)
    .collect::<BTreeSet<_>>();
  let direct_inputs = evidence
    .direct_inputs
    .iter()
    .map(PathBuf::from)
    .collect::<BTreeSet<_>>();
  for directory in &temporary_directories {
    if !directory
      .file_name()
      .and_then(OsStr::to_str)
      .is_some_and(is_rustc_temporary_name)
    {
      return Err(RailError::message(
        "Apple linker driver-input certificate contains an invalid temporary directory",
      ));
    }
  }
  if preexisting_paths.iter().any(|path| {
    path
      .parent()
      .is_none_or(|parent| !temporary_directories.contains(parent))
  }) || evidence.generated_inputs.iter().map(Path::new).any(|path| {
    path.extension() != Some(OsStr::new("o"))
      || path
        .parent()
        .is_none_or(|parent| !temporary_directories.contains(parent))
      || direct_inputs.contains(path)
      || preexisting_paths.contains(path)
  }) {
    return Err(RailError::message(
      "Apple linker driver-input certificate contains invalid generated inputs",
    ));
  }
  Ok(evidence)
}

#[cfg(target_os = "macos")]
fn read_apple_link_driver_inputs(path: &Path) -> RailResult<CertifiedAppleLinkInputs> {
  let evidence = read_apple_link_driver_evidence(path)?;
  Ok(CertifiedAppleLinkInputs {
    direct: evidence.direct_inputs.into_iter().map(PathBuf::from).collect(),
    generated: evidence.generated_inputs.into_iter().map(PathBuf::from).collect(),
  })
}

#[cfg(target_os = "macos")]
fn parse_apple_link_certificate(bytes: &[u8]) -> RailResult<(String, Vec<(u8, String)>)> {
  if bytes.first() != Some(&0) {
    return Err(RailError::message(
      "Apple linker certificate has an unsupported version tag",
    ));
  }
  let mut offset = 1usize;
  let version = read_apple_link_c_string(bytes, &mut offset)?;
  if !version.starts_with("@(#)PROGRAM:ld PROJECT:ld-") {
    return Err(RailError::message(
      "Apple linker certificate has an unsupported linker identity",
    ));
  }
  let mut entries = Vec::new();
  while offset < bytes.len() {
    if entries.len() >= MAX_LINK_INPUTS.saturating_add(1) {
      return Err(RailError::message("Apple linker certificate has too many entries"));
    }
    let opcode = bytes[offset];
    offset = offset.saturating_add(1);
    entries.push((opcode, read_apple_link_c_string(bytes, &mut offset)?));
  }
  Ok((version, entries))
}

#[cfg(target_os = "macos")]
fn read_apple_link_c_string(bytes: &[u8], offset: &mut usize) -> RailResult<String> {
  let remaining = bytes
    .get(*offset..)
    .ok_or_else(|| RailError::message("Apple linker certificate offset is invalid"))?;
  let length = remaining
    .iter()
    .position(|byte| *byte == 0)
    .ok_or_else(|| RailError::message("Apple linker certificate contains an unterminated string"))?;
  let value = std::str::from_utf8(&remaining[..length])
    .map_err(|_| RailError::message("Apple linker certificate path is not valid UTF-8"))?
    .to_string();
  *offset = offset.saturating_add(length).saturating_add(1);
  Ok(value)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn is_rustc_temporary_name(name: &str) -> bool {
  name.len() == 11 && name.starts_with("rustc") && name[5..].bytes().all(|byte| byte.is_ascii_alphanumeric())
}

#[cfg(target_os = "macos")]
fn apple_rustc_object_prefix(role: NativeOutputRole, linked_name: &str) -> Option<String> {
  let stem = Path::new(linked_name).file_stem()?.to_str()?;
  let stem = if matches!(
    role,
    NativeOutputRole::ProcMacro | NativeOutputRole::Dylib | NativeOutputRole::Cdylib
  ) {
    stem.strip_prefix("lib")?
  } else {
    stem
  };
  (!stem.is_empty()).then(|| format!("{stem}."))
}

#[cfg(target_os = "macos")]
fn resolve_selected_apple_linker() -> RailResult<PathBuf> {
  let output = Command::new("/usr/bin/xcrun").args(["-f", "ld"]).output()?;
  if !output.status.success() || !output.stderr.is_empty() {
    return Err(RailError::message("xcrun could not resolve the selected Apple linker"));
  }
  let path = std::str::from_utf8(&output.stdout)
    .map_err(|_| RailError::message("xcrun returned a non-UTF-8 Apple linker path"))?
    .trim();
  if path.is_empty() || !Path::new(path).is_absolute() {
    return Err(RailError::message("xcrun returned an invalid Apple linker path"));
  }
  Ok(crate::utils::canonicalize_existing(Path::new(path))?)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn capture_link_file(
  path: &Path,
  started: Instant,
  budget: &mut NativeCaptureBudget,
) -> RailResult<(LinkFileWitness, Option<String>)> {
  if !path.is_absolute() || path.as_os_str().as_encoded_bytes().contains(&0) {
    return Err(RailError::message("Apple linker input path is invalid"));
  }
  let canonical = crate::utils::canonicalize_existing(path)?;
  let generation_before = linker_generation_identity(&canonical);
  let (content_digest, metadata, _) = capture_guarded_file(&canonical, started, budget)?;
  let generation_after = linker_generation_identity(&canonical);
  if generation_before != generation_after || crate::utils::canonicalize_existing(path)? != canonical {
    return Err(RailError::message(
      "Apple linker input path changed while it was captured",
    ));
  }
  Ok((
    LinkFileWitness {
      path: path
        .to_str()
        .ok_or_else(|| RailError::message("Apple linker input path is not valid UTF-8"))?
        .to_string(),
      canonical_path: canonical
        .to_str()
        .ok_or_else(|| RailError::message("Apple linker canonical input path is not valid UTF-8"))?
        .to_string(),
      content_digest,
      bytes: metadata.len,
      #[cfg(unix)]
      mode: metadata.mode & 0o777,
      #[cfg(not(unix))]
      mode: if metadata.readonly { 0o444 } else { 0o644 },
    },
    generation_after,
  ))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn revalidate_link_file(
  expected: &LinkFileWitness,
  generation: Option<&str>,
  started: Instant,
  budget: &mut NativeCaptureBudget,
) -> RailResult<()> {
  if let Some(generation) = generation {
    let canonical = crate::utils::canonicalize_existing(Path::new(&expected.path))?;
    let generation_before = linker_generation_identity(&canonical);
    if canonical == Path::new(&expected.canonical_path)
      && generation_before.as_deref() == Some(generation)
      && crate::utils::canonicalize_existing(Path::new(&expected.path))? == canonical
      && linker_generation_identity(&canonical) == generation_before
    {
      return Ok(());
    }
  }
  let (current, _) = capture_link_file(Path::new(&expected.path), started, budget)?;
  if current != *expected {
    return Err(RailError::message("Apple linker found input changed"));
  }
  Ok(())
}

#[cfg(target_os = "macos")]
fn revalidate_apple_linker_witness(
  witness: &AppleLinkerWitness,
  generations: Option<&LinkerGenerationWitness>,
  installation_authority: Option<&str>,
) -> RailResult<u64> {
  validate_apple_linker_witness(witness)?;
  if let Some(generations) = generations {
    validate_apple_linker_generations(generations, witness)?;
  }
  let trusted_generations = generations.filter(|generations| {
    installation_authority.map(installation_authority_identity).as_ref() == Some(&generations.installation_authority)
  });
  let current_linker = resolve_selected_apple_linker()?;
  if current_linker != Path::new(&witness.linker.path) {
    return Err(RailError::message("selected Apple linker changed"));
  }
  let started = Instant::now();
  let mut budget = NativeCaptureBudget::new(NATIVE_CAPTURE_LIMITS);
  revalidate_link_file(
    &witness.driver,
    trusted_generations.map(|generations| generations.driver.as_str()),
    started,
    &mut budget,
  )?;
  revalidate_link_file(
    &witness.linker,
    trusted_generations.map(|generations| generations.linker.as_str()),
    started,
    &mut budget,
  )?;
  for (index, expected) in witness.found.iter().enumerate() {
    revalidate_link_file(
      expected,
      trusted_generations.and_then(|generations| generations.found.get(index).map(String::as_str)),
      started,
      &mut budget,
    )?;
  }
  for missing in &witness.missing {
    if !fs::symlink_metadata(missing).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) {
      return Err(RailError::message("Apple linker missing input appeared"));
    }
  }
  Ok(budget.bytes_hashed)
}

#[cfg(target_os = "linux")]
fn revalidate_elf_linker_witness(
  witness: &ElfLinkerWitness,
  generations: Option<&LinkerGenerationWitness>,
  installation_authority: Option<&str>,
) -> RailResult<u64> {
  validate_elf_linker_witness(witness)?;
  if let Some(generations) = generations {
    validate_elf_linker_generations(generations, witness)?;
  }
  let current_directory = std::env::current_dir()?;
  let current_driver = crate::executable::resolve_executable_path(OsStr::new("cc"), &current_directory)?;
  if current_driver != Path::new(&witness.driver.canonical_path) {
    return Err(RailError::message("selected ELF linker driver changed"));
  }
  let current_linker = resolve_selected_elf_linker(&current_driver, &current_directory)?;
  if current_linker != Path::new(&witness.linker.canonical_path)
    || !elf_linker_supports_dependency_file(&current_linker)?
  {
    return Err(RailError::message("selected ELF linker changed"));
  }
  let trusted_generations = generations.filter(|generations| {
    installation_authority.map(installation_authority_identity).as_ref() == Some(&generations.installation_authority)
  });
  let started = Instant::now();
  let mut budget = NativeCaptureBudget::new(NATIVE_CAPTURE_LIMITS);
  revalidate_link_file(
    &witness.driver,
    trusted_generations.map(|generations| generations.driver.as_str()),
    started,
    &mut budget,
  )?;
  revalidate_link_file(
    &witness.linker,
    trusted_generations.map(|generations| generations.linker.as_str()),
    started,
    &mut budget,
  )?;
  for (index, expected) in witness.found.iter().enumerate() {
    revalidate_link_file(
      expected,
      trusted_generations.and_then(|generations| generations.found.get(index).map(String::as_str)),
      started,
      &mut budget,
    )?;
  }
  for missing in &witness.missing {
    if !fs::symlink_metadata(missing).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) {
      return Err(RailError::message("ELF linker missing input appeared"));
    }
  }
  Ok(budget.bytes_hashed)
}

fn revalidate_captured_namespace(captured: &NativeNamespaceCapture, role: &str) -> RailResult<()> {
  if captured.guard.entries.len() != captured.state.entries.len() {
    return Err(RailError::message(format!(
      "native {role} restore guard paths are invalid"
    )));
  }
  for expected in &captured.guard.entries {
    if captured
      .state
      .entries
      .binary_search_by(|entry| entry.path.as_str().cmp(&expected.path))
      .is_err()
    {
      return Err(RailError::message(format!(
        "native {role} restore guard path is invalid"
      )));
    }
    let path = captured.root.join(&expected.path);
    let metadata = fs::symlink_metadata(&path)?;
    if crate::utils::is_symlink_or_reparse(&metadata) || native_metadata_guard(&path, &metadata)? != expected.metadata {
      return Err(RailError::message(format!(
        "native {role} input changed before the restore commit"
      )));
    }
  }
  Ok(())
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
  required_file: Option<&Path>,
  excluded_roots: &[PathBuf],
  source_root: &Path,
  started: Instant,
  budget: &mut NativeCaptureBudget,
) -> RailResult<(NativeSourceState, NativeCaptureGuard)> {
  let root_metadata = fs::symlink_metadata(namespace)?;
  if !root_metadata.is_dir() || crate::utils::is_symlink_or_reparse(&root_metadata) {
    return Err(RailError::message("native source namespace is not a real directory"));
  }
  let root = ObservationPath::capture(namespace, source_root, source_root);
  if excluded_roots.iter().any(|excluded| excluded == namespace) {
    return Ok((
      NativeSourceState {
        version: 1,
        root,
        entries: Vec::new(),
      },
      NativeCaptureGuard { entries: Vec::new() },
    ));
  }
  let mut entries = Vec::new();
  let mut guards = Vec::new();
  let mut pending = vec![(PathBuf::new(), 0usize)];
  let mut found_required_file = required_file.is_none();
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
      fs::read_dir(&absolute_directory)?.filter_map(|entry| match entry {
        Ok(entry) => {
          let child = relative_directory.join(entry.file_name());
          let absolute = namespace.join(child);
          (!excluded_roots.iter().any(|excluded| absolute.starts_with(excluded))).then_some(Ok(entry.file_name()))
        }
        Err(error) => Some(Err(error)),
      }),
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
      if required_file.is_some_and(|required| absolute == required) {
        found_required_file = true;
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

  if !found_required_file {
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

fn capture_native_generated_namespace(
  observation: &RawCompilerInvocation,
  source_root: &Path,
  started: Instant,
  budget: &mut NativeCaptureBudget,
) -> RailResult<Option<NativeNamespaceCapture>> {
  let current_directory = std::env::current_dir()?;
  let excluded = compiler_output_directory(&observation.compiler_arguments, &current_directory, source_root)?
    .map(|directory| crate::utils::canonicalize_existing(&directory))
    .transpose()?
    .into_iter()
    .collect::<Vec<_>>();
  capture_native_generated_namespace_from(std::env::var_os("OUT_DIR"), &excluded, source_root, started, budget)
}

fn capture_native_generated_namespace_from(
  root: Option<OsString>,
  excluded_roots: &[PathBuf],
  source_root: &Path,
  started: Instant,
  budget: &mut NativeCaptureBudget,
) -> RailResult<Option<NativeNamespaceCapture>> {
  let Some(root) = root else {
    return Ok(None);
  };
  let root_spelling = PathBuf::from(root);
  if !root_spelling.is_absolute() || root_spelling.as_os_str().as_encoded_bytes().contains(&0) {
    return Err(RailError::message("native compiler OUT_DIR is not an absolute path"));
  }
  let metadata = match fs::symlink_metadata(&root_spelling) {
    Ok(metadata) => metadata,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error.into()),
  };
  if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
    return Err(RailError::message("native compiler OUT_DIR is not a real directory"));
  }
  let root = crate::utils::canonicalize_existing(&root_spelling)?;
  let (state, guard) = capture_native_source_namespace(&root, None, excluded_roots, source_root, started, budget)?;
  Ok(Some(NativeNamespaceCapture {
    root,
    root_spelling,
    state,
    guard,
  }))
}

fn capture_native_search_namespaces(
  arguments: &[String],
  generated: Option<&NativeNamespaceCapture>,
  source_root: &Path,
  started: Instant,
  budget: &mut NativeCaptureBudget,
) -> RailResult<Vec<NativeNamespaceCapture>> {
  let current_directory = std::env::current_dir()?;
  let mut captures = Vec::<NativeNamespaceCapture>::new();
  let paths = native_search_paths(arguments, &current_directory, source_root)?;
  if paths.is_empty()
    && arguments
      .iter()
      .any(|argument| argument == "-l" || argument.starts_with("-l"))
  {
    return Err(RailError::message(
      "native static library has no captured search namespace",
    ));
  }
  for spelling in paths {
    let metadata = fs::symlink_metadata(&spelling)?;
    if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
      return Err(RailError::message(
        "native compiler library search namespace is not a real directory",
      ));
    }
    let root = crate::utils::canonicalize_existing(&spelling)?;
    if generated.is_some_and(|generated| generated.root == root)
      || captures.iter().any(|captured| captured.root == root)
    {
      continue;
    }
    let (state, guard) = capture_native_source_namespace(&root, None, &[], source_root, started, budget)?;
    captures.push(NativeNamespaceCapture {
      root,
      root_spelling: spelling,
      state,
      guard,
    });
  }
  Ok(captures)
}

fn capture_pathless_extern_searches(
  arguments: &[String],
  source_root: &Path,
  started: Instant,
  budget: &mut NativeCaptureBudget,
) -> RailResult<Vec<NativePathlessExternSearchCapture>> {
  if pathless_extern_names(arguments).is_empty() {
    return Ok(Vec::new());
  }
  let current_directory = std::env::current_dir()?;
  let roots = pathless_extern_search_roots(arguments, &current_directory, source_root)?;
  let mut captures = Vec::with_capacity(roots.len());
  for (root, root_spelling) in roots {
    let names = pathless_extern_candidate_names(&root, started)?;
    let mut entries = Vec::with_capacity(names.len());
    let mut guards = Vec::with_capacity(names.len());
    for name in &names {
      budget.account_entry(name)?;
      let path = root.join(name);
      let metadata = fs::symlink_metadata(&path)?;
      if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::message(
          "pathless compiler extern search contains a non-regular candidate",
        ));
      }
      let mode = semantic_mode(&metadata);
      let (content_digest, guard, bytes) = capture_guarded_file(&path, started, budget)?;
      entries.push(NativeSourceEntry {
        path: name.clone(),
        kind: NativeSourceEntryKind::RegularFile {
          bytes,
          content_digest,
          mode,
        },
      });
      guards.push(NativeGuardEntry {
        path: name.clone(),
        metadata: guard,
      });
    }
    if pathless_extern_candidate_names(&root, started)? != names {
      return Err(RailError::message(
        "pathless compiler extern search changed during action capture",
      ));
    }
    captures.push(NativePathlessExternSearchCapture {
      root,
      root_spelling,
      entries,
      guard: NativeCaptureGuard { entries: guards },
    });
  }
  Ok(captures)
}

fn revalidate_pathless_extern_search(captured: &NativePathlessExternSearchCapture) -> RailResult<()> {
  let metadata = fs::symlink_metadata(&captured.root_spelling)?;
  if !metadata.is_dir()
    || crate::utils::is_symlink_or_reparse(&metadata)
    || crate::utils::canonicalize_existing(&captured.root_spelling)? != captured.root
  {
    return Err(RailError::message(
      "pathless compiler extern search namespace changed before the restore commit",
    ));
  }
  let names = pathless_extern_candidate_names(&captured.root, Instant::now())?;
  if names.len() != captured.entries.len()
    || !names
      .iter()
      .zip(&captured.entries)
      .all(|(name, expected)| name == &expected.path)
    || captured.guard.entries.len() != captured.entries.len()
  {
    return Err(RailError::message(
      "pathless compiler extern candidates changed before the restore commit",
    ));
  }
  for (entry, expected) in captured.entries.iter().zip(&captured.guard.entries) {
    if entry.path != expected.path {
      return Err(RailError::message(
        "pathless compiler extern restore guard path is invalid",
      ));
    }
    let metadata = fs::symlink_metadata(captured.root.join(&entry.path))?;
    if crate::utils::is_symlink_or_reparse(&metadata)
      || native_metadata_guard(&captured.root.join(&entry.path), &metadata)? != expected.metadata
    {
      return Err(RailError::message(
        "pathless compiler extern candidate changed before the restore commit",
      ));
    }
  }
  Ok(())
}

fn pathless_extern_search_roots(
  arguments: &[String],
  current_directory: &Path,
  source_root: &Path,
) -> RailResult<Vec<(PathBuf, PathBuf)>> {
  let mut roots = Vec::new();
  for value in compiler_library_search_values(arguments)? {
    let Some(path) = pathless_extern_search_path(value)? else {
      continue;
    };
    let spelling = resolve_portable_compiler_path(path, current_directory, source_root)?;
    let metadata = fs::symlink_metadata(&spelling)?;
    if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
      return Err(RailError::message(
        "pathless compiler extern search namespace is not a real directory",
      ));
    }
    let root = crate::utils::canonicalize_existing(&spelling)?;
    if roots.iter().any(|(captured, _)| captured == &root) {
      continue;
    }
    roots.push((root, spelling));
  }
  Ok(roots)
}

fn pathless_extern_candidate_names(root: &Path, started: Instant) -> RailResult<Vec<String>> {
  let mut scanned = 0usize;
  let mut path_bytes = 0usize;
  let mut names = Vec::new();
  for entry in fs::read_dir(root)? {
    if started.elapsed() > MAX_SOURCE_CAPTURE_TIME {
      return Err(RailError::message(
        "pathless compiler extern search exceeded its time bound",
      ));
    }
    let entry = entry?;
    scanned = scanned
      .checked_add(1)
      .ok_or_else(|| RailError::message("pathless compiler extern search entry count overflowed"))?;
    if scanned > MAX_SOURCE_ENTRIES {
      return Err(RailError::message(
        "pathless compiler extern search exceeded its entry bound",
      ));
    }
    let name = entry.file_name();
    path_bytes = path_bytes
      .checked_add(name.as_encoded_bytes().len())
      .ok_or_else(|| RailError::message("pathless compiler extern search path-byte count overflowed"))?;
    if path_bytes > MAX_SOURCE_PATH_BYTES {
      return Err(RailError::message(
        "pathless compiler extern search exceeded its path-byte bound",
      ));
    }
    let Some(name) = name.to_str() else {
      continue;
    };
    if pathless_proc_macro_candidate_name(name) {
      names.push(name.to_string());
    }
  }
  names.sort_unstable();
  names.dedup();
  Ok(names)
}

fn pathless_proc_macro_candidate_name(name: &str) -> bool {
  let candidate = name
    .strip_prefix("libproc_macro")
    .or_else(|| name.strip_prefix("proc_macro"));
  candidate.is_some_and(|suffix| suffix.starts_with('-') || suffix.starts_with('.'))
    && matches!(
      Path::new(name).extension().and_then(OsStr::to_str),
      Some("rlib" | "rmeta" | "dylib" | "so" | "dll" | "a" | "lib")
    )
}

fn native_search_paths(arguments: &[String], current_directory: &Path, source_root: &Path) -> RailResult<Vec<PathBuf>> {
  let mut paths = Vec::new();
  for value in compiler_library_search_values(arguments)? {
    if let Some(path) = native_search_path(value)? {
      paths.push(resolve_portable_compiler_path(path, current_directory, source_root)?);
    }
  }
  Ok(paths)
}

fn compiler_library_search_values(arguments: &[String]) -> RailResult<Vec<&str>> {
  let mut values = Vec::new();
  let mut index = 0usize;
  while index < arguments.len() {
    if arguments[index] == "-L" {
      index = index.saturating_add(1);
      values.push(
        arguments
          .get(index)
          .ok_or_else(|| RailError::message("native compiler library search path is missing"))?
          .as_str(),
      );
    } else if let Some(value) = arguments[index].strip_prefix("-L").filter(|value| !value.is_empty()) {
      values.push(value);
    }
    index = index.saturating_add(1);
  }
  Ok(values)
}

fn compiler_output_directory(
  arguments: &[String],
  current_directory: &Path,
  source_root: &Path,
) -> RailResult<Option<PathBuf>> {
  let mut selected = None;
  let mut index = 0usize;
  while index < arguments.len() {
    let value = if arguments[index] == "--out-dir" {
      index = index.saturating_add(1);
      Some(
        arguments
          .get(index)
          .ok_or_else(|| RailError::message("native compiler output directory is missing"))?
          .as_str(),
      )
    } else {
      arguments[index].strip_prefix("--out-dir=")
    };
    if let Some(value) = value {
      selected = Some(resolve_portable_compiler_path(value, current_directory, source_root)?);
    }
    index = index.saturating_add(1);
  }
  Ok(selected)
}

fn resolve_portable_compiler_path(path: &str, current_directory: &Path, source_root: &Path) -> RailResult<PathBuf> {
  if let Some(relative) = path.strip_prefix("repository:") {
    let relative = relative.trim_start_matches(['/', '\\']);
    crate::source::RepositoryPath::new(Path::new(relative))?;
    Ok(source_root.join(relative))
  } else {
    let path = PathBuf::from(path);
    Ok(if path.is_absolute() {
      path
    } else {
      current_directory.join(path)
    })
  }
}

fn native_search_path(value: &str) -> RailResult<Option<&str>> {
  if value.starts_with("dependency=") {
    return Ok(None);
  }
  let path = match value.split_once('=') {
    Some(("native" | "all", path)) => path,
    Some(_) => {
      return Err(RailError::message(
        "native compiler library search kind is not graduated",
      ));
    }
    None => value,
  };
  if path.is_empty() || path.as_bytes().contains(&0) {
    return Err(RailError::message("native compiler library search path is invalid"));
  }
  Ok(Some(path))
}

fn pathless_extern_search_path(value: &str) -> RailResult<Option<&str>> {
  let path = match value.split_once('=') {
    Some(("dependency" | "all", path)) => Some(path),
    Some(("native", _)) => None,
    Some(_) => {
      return Err(RailError::message(
        "pathless compiler extern search kind is not graduated",
      ));
    }
    None => Some(value),
  };
  match path {
    Some(path) if path.is_empty() || path.as_bytes().contains(&0) => {
      Err(RailError::message("pathless compiler extern search path is invalid"))
    }
    _ => Ok(path),
  }
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
  #[cfg(windows)]
  let mut file = crate::windows_fs::open_for_stable_byte_observation(path)?;
  #[cfg(not(windows))]
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

fn executable_mode_from_guard(_metadata: &NativeMetadataGuard) -> bool {
  #[cfg(unix)]
  {
    _metadata.mode & 0o111 != 0
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
      // `env!("OUT_DIR")` may embed the literal path in Rust metadata. Keep it
      // exact when the same path also owns the captured generated namespace.
      if name != "OUT_DIR" || capture.generated.is_none() {
        for (spellings, token) in &root_bindings {
          let (next, replaced) = replace_source_root_spellings(&value, spellings, token);
          value = next;
          root_mapped |= replaced;
        }
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

fn source_root_path_spellings(path: &Path) -> Vec<Vec<u8>> {
  source_root_path_forms(path)
    .into_iter()
    .map(|(_, spelling)| spelling)
    .collect()
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
        | BENCH_COVERAGE_DIRECTORY_ENV
        | LEGACY_STORE_ENV
        | crate::remote_cache::REMOTE_URL_ENV
        | crate::remote_cache::REMOTE_MODE_ENV
        | crate::remote_cache::REMOTE_ENVIRONMENT_ENV
        | crate::cache::cas::CACHE_BASE_ENV
        | crate::cache::cas::CACHE_MAX_BYTES_ENV
        | crate::cache::cas::CACHE_TRUST_DOMAIN_ENV
        | crate::compiler::invocation::CACHE_CONTROL_ENV
        | crate::compiler::invocation::CACHE_WRAPPER_MARKER
        | crate::compiler::invocation::WRAPPER_MARKER
        | crate::compiler::invocation::INNER_WRAPPER_ENV
        | crate::compiler::invocation::RUSTDOC_WRAPPER_MARKER
        | crate::compiler::invocation::INNER_RUSTDOC_ENV
        | DIRECT_LAUNCHER_ENV
        | crate::compiler::invocation::OBSERVATION_DIRECTORY_ENV
        | crate::compiler::invocation::OBSERVATION_SOURCE_ROOT_ENV
        | crate::compiler::invocation::OBSERVATION_ONLY_ENV
        | crate::compiler::invocation::FACT_DOCTEST_BUILDER_ENV
        | crate::compiler::invocation::FACT_DOCTEST_RUNNER_ENV
        | crate::compiler::facts::COMPILER_FACT_INVOCATION_ENV
        | crate::compiler::session::FACT_SESSION_ENV
        | APPLE_LINK_ADAPTER_ENV
        | APPLE_LINK_DRIVER_ENV
        | APPLE_LINK_CERTIFICATE_ENV
        | APPLE_LINK_DRIVER_INPUTS_ENV
        | ELF_LINK_ADAPTER_ENV
        | ELF_LINK_DRIVER_ENV
        | ELF_LINK_DEPENDENCIES_ENV
        | ELF_LINK_DRIVER_INPUTS_ENV
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

pub(crate) fn direct_worker_executable() -> RailResult<PathBuf> {
  let cargo_rail_executable = std::env::current_exe()?;
  cargo_rail_executable
    .parent()
    .map(|directory| directory.join(DIRECT_WORKER_NAME))
    .filter(|candidate| {
      fs::metadata(candidate).is_ok_and(|metadata| metadata.is_file() && executable_metadata(&metadata))
    })
    .ok_or_else(|| RailError::message("native compiler cache worker executable is unavailable"))
}

pub(crate) fn direct_distributed_worker_executable() -> RailResult<PathBuf> {
  let cargo_rail_executable = std::env::current_exe()?;
  cargo_rail_executable
    .parent()
    .map(|directory| directory.join(DISTRIBUTED_WORKER_NAME))
    .filter(|candidate| {
      fs::metadata(candidate).is_ok_and(|metadata| metadata.is_file() && executable_metadata(&metadata))
    })
    .ok_or_else(|| RailError::message("distributed compiler worker executable is unavailable"))
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

impl NativeCacheContext {
  pub(crate) fn activate(self) -> RailResult<()> {
    ACTIVE_CONTEXT
      .set(self)
      .map_err(|_| RailError::message("native compiler cache context was activated twice"))
  }

  pub(crate) fn from_environment() -> Option<Self> {
    let source_root = std::env::var_os(crate::compiler::invocation::OBSERVATION_SOURCE_ROOT_ENV).map(PathBuf::from)?;
    Some(Self {
      session: NativeCacheSession::Persisted(std::env::var_os(SESSION_ENV).map(PathBuf::from)?),
      source_root_spelling: source_root.clone(),
      source_root,
      observation_directory: std::env::var_os(crate::compiler::invocation::OBSERVATION_DIRECTORY_ENV)
        .map(PathBuf::from)?,
      local_cas: None,
      remote: crate::remote_cache::RemoteCacheSelection::from_environment()
        .ok()
        .flatten(),
      remote_store: OnceLock::new(),
      installation: None,
      _runtime: None,
    })
  }

  /// Detect the dedicated wrapper without loading its private context.
  pub(crate) fn is_direct_invocation() -> bool {
    std::env::args_os().next().is_some_and(|invoked| {
      let file_name = Path::new(&invoked).file_name();
      file_name == Some(OsStr::new(DIRECT_WRAPPER_NAME))
        || (file_name == Some(OsStr::new(DIRECT_WORKER_NAME)) && std::env::var_os(DIRECT_LAUNCHER_ENV).is_some())
    })
  }

  pub(crate) fn is_direct_wrapper_program(program: &OsStr) -> bool {
    matches!(
      Path::new(program).file_name(),
      Some(name) if name == OsStr::new(DIRECT_WRAPPER_NAME) || name == OsStr::new(DIRECT_WORKER_NAME)
    )
  }

  /// Load the direct wrapper context after acquisition-free controls are resolved.
  pub(crate) fn load_direct_invocation(
    rustc_program: &OsStr,
    rustc_arguments: &[OsString],
  ) -> Result<Self, &'static str> {
    let invoked = std::env::current_exe().map_err(|_| "native_cache_worker_unavailable");
    invoked.and_then(|invoked| Self::load_installed(&invoked, rustc_program, rustc_arguments))
  }

  fn load_installed(invoked: &Path, rustc_program: &OsStr, rustc_arguments: &[OsString]) -> Result<Self, &'static str> {
    let (source_root_spelling, source_root) =
      direct_compilation_root(rustc_arguments).map_err(|_| "compiler_output_root_unavailable")?;
    let receipt =
      crate::cache::installation::load_for_wrapper(invoked).map_err(|_| "native_cache_installation_unavailable")?;
    let local_cas = LocalCas::open_initialized_selected(receipt.cache()).map_err(|_| "local_cache_unavailable")?;
    let session = installed_native_session(&receipt, &source_root, rustc_program)
      .map_err(|_| "native_cache_session_unavailable")?;
    let runtime = private_command_directory().map_err(|_| "native_cache_runtime_unavailable")?;
    let remote = crate::remote_cache::RemoteCacheSelection::from_environment_or_installed(receipt.remote())
      .ok()
      .flatten();
    Ok(Self {
      session: NativeCacheSession::Prepared(session),
      source_root,
      source_root_spelling,
      observation_directory: runtime.path().to_path_buf(),
      local_cas: Some(local_cas),
      remote,
      remote_store: OnceLock::new(),
      installation: Some(receipt),
      _runtime: Some(runtime),
    })
  }
}

/// Derive Cargo's physical compilation root from the standard target layout.
fn direct_compilation_root(arguments: &[OsString]) -> RailResult<(PathBuf, PathBuf)> {
  let mut selected = None;
  let mut index = 0usize;
  while index < arguments.len() {
    let argument = &arguments[index];
    if argument == "--out-dir" {
      selected = arguments.get(index.saturating_add(1)).map(PathBuf::from);
      index = index.saturating_add(2);
      continue;
    }
    if let Some(argument) = argument.to_str()
      && let Some(value) = argument.strip_prefix("--out-dir=")
    {
      selected = Some(PathBuf::from(value));
    }
    index = index.saturating_add(1);
  }
  let output = selected.ok_or_else(|| RailError::message("transparent compiler output directory is unavailable"))?;
  let output = if output.is_absolute() {
    output
  } else {
    std::env::current_dir()?.join(output)
  };
  let target = output
    .ancestors()
    .find(|ancestor| ancestor.file_name() == Some(OsStr::new("target")))
    .ok_or_else(|| RailError::message("transparent compiler output does not use Cargo's standard target layout"))?;
  let spelling = target
    .parent()
    .ok_or_else(|| RailError::message("transparent compiler target directory has no compilation root"))?
    .to_path_buf();
  let canonical = crate::utils::canonicalize_existing(&spelling)?;
  let canonical_output = crate::utils::canonicalize_existing(&output)?;
  if !canonical_output.starts_with(canonical.join("target")) {
    return Err(RailError::message(
      "transparent compiler output escaped Cargo's standard target root",
    ));
  }
  let manifest = canonical.join("Cargo.toml");
  let metadata = fs::symlink_metadata(&manifest)
    .map_err(|_| RailError::message("transparent compiler target root has no real Cargo workspace manifest"))?;
  if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) {
    return Err(RailError::message(
      "transparent compiler target root has no real Cargo workspace manifest",
    ));
  }
  Ok((spelling, canonical))
}

fn installed_native_session(
  receipt: &crate::cache::installation::InstallationReceipt,
  source_root: &Path,
  rustc_program: &OsStr,
) -> RailResult<NativeCompilerSession> {
  fn load(
    receipt: &crate::cache::installation::InstallationReceipt,
    source_root: &Path,
    rustc_program: &OsStr,
  ) -> RailResult<Option<NativeCompilerSession>> {
    let Some(bytes) = crate::cache::installation::load_session_memo(receipt)? else {
      return Ok(None);
    };
    let memo = match crate::compiler::collector::TransparentNativeSessionMemo::decode(&bytes) {
      Ok(memo) => memo,
      Err(_) => return Ok(None),
    };
    crate::compiler::collector::reuse_transparent_native_session(&memo, source_root, rustc_program)
  }

  if let Some(session) = load(receipt, source_root, rustc_program)? {
    return Ok(session);
  }
  let _lock = crate::cache::installation::lock_session(receipt)?;
  if let Some(session) = load(receipt, source_root, rustc_program)? {
    return Ok(session);
  }
  let (session, _, memo) =
    crate::compiler::collector::capture_transparent_native_session(source_root, rustc_program, receipt.cache())?;
  crate::cache::installation::store_session_memo(receipt, &memo.encode()?)?;
  Ok(session)
}

fn active_context() -> Option<&'static NativeCacheContext> {
  ACTIVE_CONTEXT.get()
}

fn open_active_local_cas() -> RailResult<LocalCas> {
  active_context()
    .and_then(|context| context.local_cas.clone())
    .map_or_else(LocalCas::open_initialized, Ok)
}

fn active_remote_selection() -> Option<&'static crate::remote_cache::RemoteCacheSelection> {
  active_context().and_then(|context| context.remote.as_ref())
}

fn open_active_remote_store()
-> Result<Option<&'static crate::remote_cache::RemoteStore>, crate::remote_cache::RemoteStoreError> {
  let Some(context) = active_context() else {
    return Ok(None);
  };
  let Some(selection) = context
    .remote
    .as_ref()
    .filter(|selection| selection.direct_transport_supported())
  else {
    return Ok(None);
  };
  match context
    .remote_store
    .get_or_init(|| crate::remote_cache::RemoteStore::connect(selection, context.installation.as_ref()))
  {
    Ok(store) => Ok(Some(store)),
    Err(error) => Err(error.clone()),
  }
}

impl NativeCompilerSession {
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
      &source_root_identity,
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
      &self.source_root_identity,
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

  pub(crate) fn validate_for_source_root(&self, source_root: &Path) -> RailResult<()> {
    self.validate_object()?;
    if self.source_root_identity != path_identity(source_root)? {
      return Err(RailError::message("transparent compiler session source root changed"));
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
  file_name: String,
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
  #[serde(default, skip_serializing_if = "Option::is_none")]
  linker_generations: Option<LinkerGenerationWitness>,
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
    linker_generations: Option<LinkerGenerationWitness>,
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
      version: 16,
      action_key,
      result_key,
      session_identity: session.identity.clone(),
      session_authority: session.authority,
      class: session.class.clone(),
      witness,
      linker_generations,
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

  pub(crate) fn is_authoritative(&self) -> bool {
    self.session_authority == NativeSessionAuthority::Exact
  }

  pub(crate) fn action_key(&self) -> &str {
    &self.action_key
  }

  pub(crate) fn result_key(&self) -> &str {
    &self.result_key
  }

  pub(crate) fn remote_environment_is_approved(&self, approved_names: &[String]) -> bool {
    self
      .compiler_environment_names
      .iter()
      .all(|name| approved_names.binary_search(name).is_ok())
  }

  /// Verify that this exact action was derived from the supplied base action.
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
          root_mapped: false,
        })
        .collect(),
    };
    let pre_link_action = action_key_from_base(base_action_key, &approved_environment)?;
    let selected_action = if linked_observation(&self.observation) {
      witnessed_action_key(&pre_link_action, &self.witness)?
    } else {
      pre_link_action
    };
    if selected_action != self.action_key {
      return Err(RailError::message(
        "native remote publication base action does not bind its exact action",
      ));
    }
    Ok(&self.compiler_environment_names)
  }

  #[cfg(test)]
  pub(crate) fn with_action_key_for_test(&self, action_key: String) -> RailResult<Self> {
    validate_action_key(&action_key)?;
    let mut validation = self.clone();
    validation.action_key = action_key;
    validation.result_key = result_key(
      &validation.action_key,
      &validation.witness,
      &validation.outputs,
      &validation.stdout_digest,
      validation.stdout_bytes,
      &validation.stderr_digest,
      validation.stderr_bytes,
    )?;
    validation.validate_object()?;
    Ok(validation)
  }

  fn revalidate_publication(
    &self,
    session: &NativeCompilerSession,
    source_root: &Path,
    proof: &NativePublicationProof,
    installation_authority: Option<&str>,
  ) -> Result<u64, NativePublicationRevalidationFailure> {
    self
      .validate_object()
      .map_err(|error| NativePublicationRevalidationFailure::new("cold_protocol_changed_before_admission", error))?;
    session
      .validate_object()
      .map_err(|error| NativePublicationRevalidationFailure::new("cold_protocol_changed_before_admission", error))?;
    proof
      .validate_object()
      .map_err(|error| NativePublicationRevalidationFailure::new("cold_protocol_changed_before_admission", error))?;
    let current_source_root_identity = path_identity(source_root)
      .map_err(|error| NativePublicationRevalidationFailure::new("cold_session_changed_before_admission", error))?;
    if session.source_root_identity != current_source_root_identity
      || session.authority != NativeSessionAuthority::Exact
      || self.session_identity != session.identity
      || self.session_authority != session.authority
      || self.class != session.class
      || !self
        .compiler_environment_names
        .iter()
        .eq(proof.approved_environment.entries.iter().map(|entry| &entry.name))
    {
      return Err(NativePublicationRevalidationFailure::new(
        "cold_session_changed_before_admission",
        RailError::message("native publication proof does not match its compiler session"),
      ));
    }
    capture_test_pause("before_admission_revalidation", &self.observation).map_err(|error| {
      NativePublicationRevalidationFailure::new("cold_action_recapture_failed_before_admission", error)
    })?;
    let capture =
      NativeActionCapture::capture_with_publication_proof(&self.observation, source_root, proof).map_err(|error| {
        NativePublicationRevalidationFailure::new("cold_action_recapture_failed_before_admission", error)
      })?;
    let pre_link_action =
      action_key(&session.identity, &session.class, &self.observation, &capture).map_err(|error| {
        NativePublicationRevalidationFailure::new("cold_action_recapture_failed_before_admission", error)
      })?;
    let (selected_action, linker_bytes_hashed) = revalidate_selected_action(
      &self.observation,
      &self.witness,
      self.linker_generations.as_ref(),
      &pre_link_action,
      installation_authority,
    )
    .map_err(|error| NativePublicationRevalidationFailure::new("cold_linker_inputs_changed_before_admission", error))?;
    if selected_action != self.action_key {
      return Err(NativePublicationRevalidationFailure::new(
        "cold_action_changed_before_admission",
        RailError::message("native compiler action changed before publication authority"),
      ));
    }
    if !capture.validates_witness(&self.witness, &self.observation) {
      return Err(NativePublicationRevalidationFailure::new(
        "cold_witness_changed_before_admission",
        RailError::message("native compiler witness changed before publication authority"),
      ));
    }
    let guard_identity = capture
      .guard_identity()
      .map_err(|error| NativePublicationRevalidationFailure::new("cold_guard_changed_before_admission", error))?;
    if guard_identity != proof.guard_identity {
      return Err(NativePublicationRevalidationFailure::new(
        "cold_guard_changed_before_admission",
        RailError::message("native compiler generation guard changed before publication authority"),
      ));
    }
    Ok(
      capture
        .bytes_hashed
        .saturating_add(proof.environment_bytes_hashed)
        .saturating_add(linker_bytes_hashed),
    )
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
    if self.version != 16 {
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
      || self.observation.version != 6
      || !self.observation.success
      || self.observation.mode != CompilerMode::Rustc
      || self.observation.compiler_arguments.is_empty()
      || invocation_bypass_reason(&self.observation, true, &self.class.host_target).is_some()
      || !output_contract_matches(&self.outputs, &self.observation)
      || self.outputs.iter().any(|output| {
        validate_sha256(&output.content_digest).is_err()
          || !valid_native_output_mode(&output.role, output.mode)
          || output.file_name.is_empty()
          || output.file_name.as_bytes().contains(&0)
          || Path::new(&output.file_name).file_name() != Some(OsStr::new(&output.file_name))
      })
      || !complete_compiler_observation(&self.observation)
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
    if self.witness.version != 5
      || !self.witness.complete
      || !strictly_sorted_unique_strings(&self.witness.source_paths)
      || !strictly_sorted_unique_strings(&self.witness.generated_paths)
      || !strictly_sorted_unique_strings(&self.witness.dependency_names)
      || validate_environment_selector_names(self.witness.environment_names.iter().map(String::as_str)).is_err()
      || !self
        .witness
        .environment_names
        .iter()
        .map(String::as_str)
        .eq(observed_environment_names)
      || self.witness.source_paths.len() > MAX_SOURCE_ENTRIES
      || self.witness.generated_paths.len() > MAX_SOURCE_ENTRIES
      || self.witness.dependency_names.len() > MAX_SOURCE_ENTRIES
      || self
        .witness
        .source_paths
        .iter()
        .any(|path| !native_relative_path(Path::new(path)).is_ok_and(|canonical| canonical == *path))
      || self
        .witness
        .generated_paths
        .iter()
        .any(|path| !native_relative_path(Path::new(path)).is_ok_and(|canonical| canonical == *path))
      || self
        .witness
        .dependency_names
        .iter()
        .any(|name| name.is_empty() || name.as_bytes().contains(&0))
      || !platform_linker_witness_is_valid(&self.observation, &self.witness, self.linker_generations.as_ref())
    {
      return Err(RailError::message("native compiler witness is invalid"));
    }
    for output in &self.outputs {
      if output.bytes == 0 && output.role != "metadata" {
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
      if !path.is_empty() {
        crate::source::RepositoryPath::new(Path::new(path))?;
      }
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
  source_root_identity: &str,
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
      (b"version", &13_u32.to_le_bytes()),
      (
        b"toolchain-capability-contract",
        &NATIVE_CACHE_IDENTITY_CONTRACT_VERSION.to_le_bytes(),
      ),
      (b"class", &class),
      (b"source-root", source_root_identity.as_bytes()),
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

fn link_candidate_selector(pre_link_action: &str) -> RailResult<String> {
  validate_action_key(pre_link_action)?;
  Ok(sha256_identity(
    CANDIDATE_SELECTOR_PREFIX,
    b"cargo-rail-native-link-candidate\0",
    &[
      (b"version", &5_u32.to_le_bytes()),
      (b"pre-link-action", pre_link_action.as_bytes()),
    ],
  ))
}

fn witnessed_action_key(pre_link_action: &str, witness: &NativeCompilerWitness) -> RailResult<String> {
  validate_action_key(pre_link_action)?;
  let linker = witness
    .linker
    .as_ref()
    .ok_or_else(|| RailError::message("linked compiler action has no platform linker witness"))?;
  validate_linker_witness(linker)?;
  let linker = serde_json::to_vec(linker)?;
  Ok(sha256_identity(
    ACTION_KEY_PREFIX,
    b"cargo-rail-native-witnessed-compiler-action\0",
    &[
      (b"version", &17_u32.to_le_bytes()),
      (b"pre-link-action", pre_link_action.as_bytes()),
      (b"linker", &linker),
    ],
  ))
}

fn revalidate_selected_action(
  observation: &RawCompilerInvocation,
  witness: &NativeCompilerWitness,
  generations: Option<&LinkerGenerationWitness>,
  pre_link_action: &str,
  installation_authority: Option<&str>,
) -> RailResult<(String, u64)> {
  if !linked_observation(observation) {
    if witness.linker.is_some() || generations.is_some() {
      return Err(RailError::message(
        "non-linked compiler action contains a linker witness",
      ));
    }
    return Ok((pre_link_action.to_string(), 0));
  }
  #[cfg(target_os = "macos")]
  {
    let Some(LinkerWitness::Apple(linker)) = witness.linker.as_ref() else {
      return Err(RailError::message("linked compiler action has no Apple linker witness"));
    };
    let bytes_hashed = revalidate_apple_linker_witness(linker, generations, installation_authority)?;
    Ok((witnessed_action_key(pre_link_action, witness)?, bytes_hashed))
  }
  #[cfg(target_os = "linux")]
  {
    let Some(LinkerWitness::Elf(linker)) = witness.linker.as_ref() else {
      return Err(RailError::message("linked compiler action has no ELF linker witness"));
    };
    let bytes_hashed = revalidate_elf_linker_witness(linker, generations, installation_authority)?;
    Ok((witnessed_action_key(pre_link_action, witness)?, bytes_hashed))
  }
  #[cfg(not(any(target_os = "macos", target_os = "linux")))]
  {
    let _ = (witness, generations, installation_authority);
    Err(RailError::message(
      "linked compiler action is unavailable on this platform",
    ))
  }
}

fn action_key_from_base(base_action: &str, approved_environment: &ApprovedEnvState) -> RailResult<String> {
  validate_identity(base_action, BASE_ACTION_KEY_PREFIX)?;
  approved_environment.validate_object()?;
  let approved_environment = serde_json::to_vec(approved_environment)?;
  Ok(sha256_identity(
    ACTION_KEY_PREFIX,
    b"cargo-rail-native-compiler-action\0",
    &[
      (b"version", &14_u32.to_le_bytes()),
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
  let generated_state = serde_json::to_vec(&capture.generated.as_ref().map(|generated| &generated.state))?;
  let native_search_states = serde_json::to_vec(
    &capture
      .native_searches
      .iter()
      .map(|captured| &captured.state)
      .collect::<Vec<_>>(),
  )?;
  let pathless_extern_search_states = serde_json::to_vec(
    &capture
      .pathless_extern_searches
      .iter()
      .map(|captured| &captured.entries)
      .collect::<Vec<_>>(),
  )?;
  Ok(sha256_identity(
    BASE_ACTION_KEY_PREFIX,
    b"cargo-rail-native-compiler-base-action\0",
    &[
      (b"version", &9_u32.to_le_bytes()),
      (b"session", session_identity.as_bytes()),
      (b"class", &class),
      (b"compilation-root", capture.compilation_root_identity().as_bytes()),
      (b"pre-execution", &pre_execution),
      (b"source-state", &source_state),
      (b"generated-state", &generated_state),
      (b"native-search-states", &native_search_states),
      (b"pathless-extern-search-states", &pathless_extern_search_states),
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

/// Derive protocol v3 only when the captured native action is already the
/// worker's exact portable, non-linking Rust compilation class.
///
/// This is deliberately stricter than native-cache eligibility. Unknown or
/// merely harmless rustc options are not portable execution authority.
pub(crate) fn distributed_rust_library_candidate(
  observation: &RawCompilerInvocation,
  capture: &NativeActionCapture,
  output_paths: &NativeOutputPaths,
  workspace_root: &Path,
  workspace_root_spelling: &Path,
) -> Result<crate::compiler::distributed::RustLibraryCandidate, &'static str> {
  distributed_rust_library_candidate_with_remap(
    observation,
    capture,
    output_paths,
    workspace_root,
    workspace_root_spelling,
    true,
  )
}

pub(crate) fn distributed_rust_library_normalization_candidate(
  observation: &RawCompilerInvocation,
  capture: &NativeActionCapture,
  output_paths: &NativeOutputPaths,
  workspace_root: &Path,
  workspace_root_spelling: &Path,
) -> Result<crate::compiler::distributed::RustLibraryCandidate, &'static str> {
  distributed_rust_library_candidate_with_remap(
    observation,
    capture,
    output_paths,
    workspace_root,
    workspace_root_spelling,
    false,
  )
}

fn distributed_rust_library_input_candidate(
  observation: &RawCompilerInvocation,
  capture: &NativeActionCapture,
  output_paths: &NativeOutputPaths,
  workspace_root: &Path,
  workspace_root_spelling: &Path,
) -> Result<crate::compiler::distributed::RustLibraryCandidate, &'static str> {
  distributed_rust_library_candidate(
    observation,
    capture,
    output_paths,
    workspace_root,
    workspace_root_spelling,
  )
  .or_else(|reason| {
    if reason != "distributed_argument_authority_mismatch" {
      return Err(reason);
    }
    distributed_rust_library_normalization_candidate(
      observation,
      capture,
      output_paths,
      workspace_root,
      workspace_root_spelling,
    )
  })
}

fn distributed_rust_library_candidate_with_remap(
  observation: &RawCompilerInvocation,
  capture: &NativeActionCapture,
  output_paths: &NativeOutputPaths,
  workspace_root: &Path,
  workspace_root_spelling: &Path,
  workspace_remap_required: bool,
) -> Result<crate::compiler::distributed::RustLibraryCandidate, &'static str> {
  let authority = distributed_rust_library_authority_with_remap(
    observation,
    capture,
    output_paths,
    workspace_root,
    workspace_remap_required,
  )?;
  capture
    .revalidate_before_restore_commit(observation, workspace_root, workspace_root_spelling)
    .map_err(|_| "distributed_action_changed_before_execution")?;
  crate::compiler::distributed::RustLibraryCandidate::from_captured_inputs(
    crate::compiler::distributed::RustLibraryCandidateInput {
      crate_name: authority.crate_name,
      crate_type: authority.crate_type,
      dep_info_name: authority.dep_info_name,
      edition: authority.edition,
      emission: authority.emission,
      metadata: authority.metadata,
      metadata_name: authority.metadata_name,
      extra_filename: authority.extra_filename,
      output_relative_directory: authority.output_relative_directory,
      source_relative_path: authority.source_relative_path,
      test_mode: authority.test_mode,
      toolchain_proc_macro: authority.toolchain_proc_macro,
      rlib_name: authority.rlib_name,
      options: authority.execution_options,
    },
    authority.sources,
    authority.dependencies,
  )
  .map_err(|_| "distributed_source_capture_failed")
}

struct DistributedRustLibraryAuthority {
  crate_name: String,
  crate_type: String,
  dependencies: Vec<crate::compiler::distributed::RustLibraryDependencyInput>,
  dep_info_name: String,
  edition: String,
  emission: crate::compiler::distributed::RustLibraryEmission,
  execution_options: crate::compiler::distributed::RustLibraryExecutionOptions,
  extra_filename: String,
  metadata: String,
  metadata_name: String,
  output_relative_directory: String,
  rlib_name: Option<String>,
  sources: Vec<crate::compiler::distributed::RustLibrarySourceInput>,
  source_relative_path: String,
  test_mode: bool,
  toolchain_proc_macro: bool,
}

#[cfg(test)]
fn distributed_rust_library_authority(
  observation: &RawCompilerInvocation,
  capture: &NativeActionCapture,
  output_paths: &NativeOutputPaths,
  workspace_root: &Path,
) -> Result<DistributedRustLibraryAuthority, &'static str> {
  distributed_rust_library_authority_with_remap(observation, capture, output_paths, workspace_root, true)
}

fn distributed_rust_library_authority_with_remap(
  observation: &RawCompilerInvocation,
  capture: &NativeActionCapture,
  output_paths: &NativeOutputPaths,
  workspace_root: &Path,
  workspace_remap_required: bool,
) -> Result<DistributedRustLibraryAuthority, &'static str> {
  use crate::compiler::distributed::RustLibraryEmission;

  let metadata_emits = BTreeSet::from(["dep-info".to_string(), "metadata".to_string()]);
  let linked_emits = BTreeSet::from(["dep-info".to_string(), "link".to_string(), "metadata".to_string()]);
  let emission = if observation.emit_modes == metadata_emits {
    RustLibraryEmission::Metadata
  } else if observation.emit_modes == linked_emits {
    RustLibraryEmission::MetadataAndLink
  } else {
    return Err("distributed_output_contract_ineligible");
  };
  let mut crate_types = observation.crate_types.iter().map(String::as_str);
  let crate_type = match (crate_types.next(), observation.test_mode) {
    (Some(crate_type), _) => crate_type,
    (None, true) => "bin",
    (None, false) => return Err("distributed_crate_type_unavailable"),
  };
  if crate_types.next().is_some() {
    return Err("distributed_crate_type_unavailable");
  }
  let portable_crate_type = matches!(
    crate_type,
    "bin" | "cdylib" | "dylib" | "lib" | "proc-macro" | "rlib" | "staticlib"
  );
  if observation.mode != CompilerMode::Rustc
    || !portable_crate_type
    || emission == RustLibraryEmission::MetadataAndLink && !matches!(crate_type, "lib" | "rlib")
    || observation.target_argument.is_some()
    || !observation.environment_reads.is_empty()
    || !observation.bypasses.is_empty()
    || capture.generated.is_some()
    || !capture.native_searches.is_empty()
    || !capture.pathless_extern_searches.is_empty()
      && !(crate_type == "proc-macro" && pathless_extern_names(&observation.compiler_arguments) == ["proc_macro"])
    || !capture.approved_environment.entries.is_empty()
  {
    return Err("distributed_action_class_ineligible");
  }
  let output_roles = output_paths
    .artifacts
    .iter()
    .map(|artifact| artifact.role)
    .collect::<Vec<_>>();
  let expected_roles = match emission {
    RustLibraryEmission::Metadata => &[NativeOutputRole::Metadata][..],
    RustLibraryEmission::MetadataAndLink => &[NativeOutputRole::Metadata, NativeOutputRole::Rlib][..],
  };
  if output_roles != expected_roles {
    return Err("distributed_output_contract_ineligible");
  }
  let [declared] = observation.declared_inputs.as_slice() else {
    return Err("distributed_source_input_unavailable");
  };
  let ObservationPath::Repository(source_relative_path) = &declared.path else {
    return Err("distributed_source_input_unavailable");
  };
  let ObservationPath::Repository(namespace_relative) = &capture.source_state.root else {
    return Err("distributed_source_namespace_ineligible");
  };
  let sources = distributed_source_inputs(capture, namespace_relative, source_relative_path, declared)?;
  let dependencies = distributed_dependency_inputs(observation, workspace_root)?;

  let arguments = DistributedRustLibraryArguments::parse(&observation.compiler_arguments)?;
  let crate_name = observation
    .crate_name
    .as_deref()
    .ok_or("distributed_crate_name_unavailable")?;
  if arguments.crate_name.as_deref() != Some(crate_name)
    || arguments
      .crate_type
      .as_deref()
      .map_or(!observation.test_mode, |argument| argument != crate_type)
    || !arguments
      .source
      .as_deref()
      .is_some_and(|source| distributed_source_argument_matches(source, &declared.path))
    || arguments.emit_modes != observation.emit_modes
    || arguments.out_dir.is_none()
    || arguments.workspace_remap_seen != workspace_remap_required
    || arguments.test_mode != observation.test_mode
    || arguments.cfg.iter().cloned().collect::<BTreeSet<_>>() != observation.cfg
    || arguments.externs
      != observation
        .dependency_artifacts
        .iter()
        .map(|(name, artifact)| {
          observation_path_basename(&artifact.path)
            .map(|artifact| (name.clone(), artifact.to_string()))
            .ok_or("distributed_dependency_artifact_ineligible")
        })
        .collect::<Result<Vec<_>, _>>()?
  {
    return Err("distributed_argument_authority_mismatch");
  }
  let extra_filename = arguments
    .extra_filename
    .as_deref()
    .ok_or("distributed_extra_filename_unavailable")?;
  let dep_info_name = output_paths
    .dep_info
    .file_name()
    .and_then(OsStr::to_str)
    .filter(|name| Path::new(name).extension() == Some(OsStr::new("d")))
    .ok_or("distributed_output_contract_ineligible")?;
  let metadata_name = output_paths
    .artifacts
    .iter()
    .find(|artifact| artifact.role == NativeOutputRole::Metadata)
    .and_then(|artifact| artifact.path.file_name())
    .and_then(OsStr::to_str)
    .filter(|name| Path::new(name).extension() == Some(OsStr::new("rmeta")))
    .ok_or("distributed_output_contract_ineligible")?;
  let rlib_name = output_paths
    .artifacts
    .iter()
    .find(|artifact| artifact.role == NativeOutputRole::Rlib)
    .and_then(|artifact| artifact.path.file_name())
    .and_then(OsStr::to_str)
    .map(str::to_string);
  let output_parent = output_paths
    .dep_info
    .parent()
    .ok_or("distributed_output_contract_ineligible")?;
  let canonical_root =
    crate::utils::canonicalize_existing(workspace_root).map_err(|_| "distributed_output_contract_ineligible")?;
  let canonical_output =
    crate::utils::canonicalize_existing(output_parent).map_err(|_| "distributed_output_contract_ineligible")?;
  let output_relative_directory = canonical_output
    .strip_prefix(&canonical_root)
    .map(crate::utils::path_to_git_format)
    .map_err(|_| "distributed_output_contract_ineligible")?;
  if crate::source::RepositoryPath::new(Path::new(&output_relative_directory)).is_err() {
    return Err("distributed_output_contract_ineligible");
  }
  let execution_options = arguments.execution_options();
  Ok(DistributedRustLibraryAuthority {
    crate_name: crate_name.to_string(),
    crate_type: crate_type.to_string(),
    dep_info_name: dep_info_name.to_string(),
    edition: arguments.edition.ok_or("distributed_edition_unavailable")?,
    emission,
    execution_options,
    extra_filename: extra_filename.to_string(),
    metadata: arguments.metadata.ok_or("distributed_metadata_unavailable")?,
    metadata_name: metadata_name.to_string(),
    output_relative_directory,
    rlib_name,
    dependencies,
    sources,
    source_relative_path: source_relative_path.clone(),
    test_mode: observation.test_mode,
    toolchain_proc_macro: arguments.toolchain_proc_macro,
  })
}

fn distributed_source_inputs(
  capture: &NativeActionCapture,
  namespace_relative: &str,
  source_relative_path: &str,
  declared: &FileObservation,
) -> Result<Vec<crate::compiler::distributed::RustLibrarySourceInput>, &'static str> {
  let mut sources = Vec::new();
  for entry in &capture.source_state.entries {
    let NativeSourceEntryKind::RegularFile {
      bytes, content_digest, ..
    } = &entry.kind
    else {
      continue;
    };
    let repository_relative_path = if namespace_relative.is_empty() {
      entry.path.clone()
    } else if entry.path.is_empty() {
      namespace_relative.to_string()
    } else {
      format!("{namespace_relative}/{}", entry.path)
    };
    if crate::source::RepositoryPath::new(Path::new(&repository_relative_path)).is_err() {
      return Err("distributed_source_namespace_ineligible");
    }
    sources.push(crate::compiler::distributed::RustLibrarySourceInput {
      bytes: *bytes,
      content_digest: content_digest.clone(),
      path: capture.source_root.join(&entry.path),
      repository_relative_path,
    });
  }
  let root = sources
    .iter()
    .find(|source| source.repository_relative_path == source_relative_path)
    .ok_or("distributed_source_namespace_ineligible")?;
  if root.content_digest != declared.content_digest || declared.executable || declared.symlink_target.is_some() {
    return Err("distributed_source_namespace_ineligible");
  }
  Ok(sources)
}

fn distributed_dependency_inputs(
  observation: &RawCompilerInvocation,
  workspace_root: &Path,
) -> Result<Vec<crate::compiler::distributed::RustLibraryDependencyInput>, &'static str> {
  observation
    .dependency_artifacts
    .iter()
    .map(|(extern_name, artifact)| {
      let artifact_name = observation_path_basename(&artifact.path)
        .filter(|name| {
          matches!(
            Path::new(name).extension().and_then(OsStr::to_str),
            Some("rmeta" | "rlib")
          )
        })
        .ok_or("distributed_dependency_artifact_ineligible")?;
      if artifact.executable || artifact.symlink_target.is_some() {
        return Err("distributed_dependency_artifact_ineligible");
      }
      let path = artifact.path.resolve(workspace_root);
      let metadata = fs::symlink_metadata(&path).map_err(|_| "distributed_dependency_artifact_ineligible")?;
      if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) {
        return Err("distributed_dependency_artifact_ineligible");
      }
      Ok(crate::compiler::distributed::RustLibraryDependencyInput {
        artifact_name: artifact_name.to_string(),
        bytes: metadata.len(),
        content_digest: artifact.content_digest.clone(),
        extern_name: extern_name.clone(),
        path,
      })
    })
    .collect()
}

fn distributed_source_argument_matches(argument: &str, declared: &ObservationPath) -> bool {
  let ObservationPath::Repository(declared) = declared else {
    return false;
  };
  let argument = argument.replace('\\', "/");
  let declared = declared.replace('\\', "/");
  argument == declared
}

#[derive(Default)]
struct DistributedRustLibraryArguments {
  cap_lints: Option<String>,
  cargo_error_format_seen: bool,
  cargo_json_seen: bool,
  check_cfg: Vec<String>,
  codegen: crate::compiler::distributed::RustLibraryCodegen,
  color: Option<String>,
  cfg: Vec<String>,
  crate_name: Option<String>,
  crate_type: Option<String>,
  diagnostic_width: Option<u32>,
  edition: Option<String>,
  emit_modes: BTreeSet<String>,
  externs: Vec<(String, String)>,
  extra_filename: Option<String>,
  lints: Vec<crate::compiler::distributed::RustLibraryLint>,
  metadata: Option<String>,
  out_dir: Option<String>,
  output_dependency_search: Option<String>,
  source: Option<String>,
  test_mode: bool,
  toolchain_proc_macro: bool,
  workspace_remap_seen: bool,
}

impl DistributedRustLibraryArguments {
  fn parse(arguments: &[String]) -> Result<Self, &'static str> {
    let mut parsed = Self::default();
    let mut index = 0usize;
    while index < arguments.len() {
      let argument = arguments[index].as_str();
      let next = arguments.get(index + 1).map(String::as_str);
      let mut consumed = 1usize;
      match argument {
        "--crate-name" => {
          consumed = 2;
          set_distributed_argument(&mut parsed.crate_name, next)?;
        }
        "--crate-type" => {
          consumed = 2;
          set_distributed_argument(&mut parsed.crate_type, next)?;
        }
        "--edition" => {
          consumed = 2;
          set_distributed_argument(&mut parsed.edition, next)?;
        }
        "--emit" => {
          consumed = 2;
          parsed.capture_emit(next)?;
        }
        "--out-dir" => {
          consumed = 2;
          set_distributed_argument(&mut parsed.out_dir, next)?;
        }
        "--error-format" => {
          consumed = 2;
          parsed.capture_cargo_error_format(next)?;
        }
        "--extern" => {
          consumed = 2;
          parsed.capture_extern(next)?;
        }
        "--test" => {
          if parsed.test_mode {
            return Err("distributed_argument_shape_ineligible");
          }
          parsed.test_mode = true;
        }
        "--json" => {
          consumed = 2;
          parsed.capture_cargo_json(next)?;
        }
        "--cfg" => {
          consumed = 2;
          parsed.capture_cfg(next)?;
        }
        "--check-cfg" => {
          consumed = 2;
          parsed.capture_check_cfg(next)?;
        }
        "--cap-lints" => {
          consumed = 2;
          parsed.capture_cap_lints(next)?;
        }
        "--color" => {
          consumed = 2;
          parsed.capture_color(next)?;
        }
        "--diagnostic-width" => {
          consumed = 2;
          parsed.capture_diagnostic_width(next)?;
        }
        "--allow" | "--warn" | "--deny" | "--forbid" | "-A" | "-W" | "-D" | "-F" => {
          consumed = 2;
          parsed.capture_lint(argument, next)?;
        }
        "--remap-path-prefix" => {
          consumed = 2;
          parsed.capture_workspace_remap(next)?;
        }
        "-C" => {
          consumed = 2;
          parsed.capture_codegen(next)?;
        }
        "-L" => {
          consumed = 2;
          parsed.capture_library_search(next)?;
        }
        _ if argument.starts_with("--crate-name=") => {
          set_distributed_argument(&mut parsed.crate_name, argument.strip_prefix("--crate-name="))?;
        }
        _ if argument.starts_with("--crate-type=") => {
          set_distributed_argument(&mut parsed.crate_type, argument.strip_prefix("--crate-type="))?;
        }
        _ if argument.starts_with("--edition=") => {
          set_distributed_argument(&mut parsed.edition, argument.strip_prefix("--edition="))?;
        }
        _ if argument.starts_with("--emit=") => parsed.capture_emit(argument.strip_prefix("--emit="))?,
        _ if argument.starts_with("--out-dir=") => {
          set_distributed_argument(&mut parsed.out_dir, argument.strip_prefix("--out-dir="))?;
        }
        _ if argument.starts_with("--error-format=") => {
          parsed.capture_cargo_error_format(argument.strip_prefix("--error-format="))?;
        }
        _ if argument.starts_with("--extern=") => {
          parsed.capture_extern(argument.strip_prefix("--extern="))?;
        }
        _ if argument.starts_with("--json=") => parsed.capture_cargo_json(argument.strip_prefix("--json="))?,
        _ if argument.starts_with("--cfg=") => parsed.capture_cfg(argument.strip_prefix("--cfg="))?,
        _ if argument.starts_with("--check-cfg=") => {
          parsed.capture_check_cfg(argument.strip_prefix("--check-cfg="))?;
        }
        _ if argument.starts_with("--cap-lints=") => {
          parsed.capture_cap_lints(argument.strip_prefix("--cap-lints="))?;
        }
        _ if argument.starts_with("--color=") => parsed.capture_color(argument.strip_prefix("--color="))?,
        _ if argument.starts_with("--diagnostic-width=") => {
          parsed.capture_diagnostic_width(argument.strip_prefix("--diagnostic-width="))?;
        }
        _ if argument.starts_with("--allow=")
          || argument.starts_with("--warn=")
          || argument.starts_with("--deny=")
          || argument.starts_with("--forbid=") =>
        {
          let (option, value) = argument
            .split_once('=')
            .ok_or("distributed_argument_shape_ineligible")?;
          parsed.capture_lint(option, Some(value))?;
        }
        _ if matches!(argument.as_bytes().first(), Some(b'-'))
          && matches!(argument.as_bytes().get(1), Some(b'A' | b'W' | b'D' | b'F'))
          && argument.len() > 2 =>
        {
          parsed.capture_lint(&argument[..2], Some(&argument[2..]))?;
        }
        _ if argument.starts_with("--remap-path-prefix=") => {
          parsed.capture_workspace_remap(argument.strip_prefix("--remap-path-prefix="))?;
        }
        _ if argument.starts_with("-C") => parsed.capture_codegen(argument.strip_prefix("-C"))?,
        _ if argument.starts_with("-L") => parsed.capture_library_search(argument.strip_prefix("-L"))?,
        _ if !argument.starts_with('-') && argument.ends_with(".rs") => {
          set_distributed_argument(&mut parsed.source, Some(argument))?;
        }
        _ => return Err("distributed_argument_shape_ineligible"),
      }
      if consumed == 2 && next.is_none() {
        return Err("distributed_argument_shape_ineligible");
      }
      index = index.saturating_add(consumed);
    }
    if parsed.cargo_error_format_seen != parsed.cargo_json_seen
      || parsed
        .output_dependency_search
        .as_deref()
        .is_some_and(|search| parsed.out_dir.as_deref() != Some(search))
    {
      return Err("distributed_argument_shape_ineligible");
    }
    Ok(parsed)
  }

  fn capture_emit(&mut self, value: Option<&str>) -> Result<(), &'static str> {
    let value = value
      .filter(|value| !value.is_empty())
      .ok_or("distributed_argument_shape_ineligible")?;
    if !self.emit_modes.is_empty() {
      return Err("distributed_argument_shape_ineligible");
    }
    for mode in value.split(',') {
      let (name, path) = mode
        .split_once('=')
        .map_or((mode, None), |(name, path)| (name, Some(path)));
      if name.is_empty() || path.is_some_and(str::is_empty) || !self.emit_modes.insert(name.to_string()) {
        return Err("distributed_argument_shape_ineligible");
      }
    }
    Ok(())
  }

  fn capture_extern(&mut self, value: Option<&str>) -> Result<(), &'static str> {
    if value == Some("proc_macro") && !self.toolchain_proc_macro {
      self.toolchain_proc_macro = true;
      return Ok(());
    }
    let (name, path) = value
      .and_then(|value| value.split_once('='))
      .filter(|(name, path)| !name.is_empty() && !path.is_empty())
      .ok_or("distributed_argument_shape_ineligible")?;
    let artifact = portable_path_basename(path).ok_or("distributed_argument_shape_ineligible")?;
    self.externs.push((name.to_string(), artifact.to_string()));
    Ok(())
  }

  fn capture_codegen(&mut self, value: Option<&str>) -> Result<(), &'static str> {
    let value = value.ok_or("distributed_argument_shape_ineligible")?;
    let (name, value) = value.split_once('=').ok_or("distributed_argument_shape_ineligible")?;
    match name {
      "metadata" => set_distributed_argument(&mut self.metadata, Some(value)),
      "extra-filename" => set_distributed_argument(&mut self.extra_filename, Some(value)),
      "codegen-units" => set_distributed_u32(&mut self.codegen.codegen_units, value),
      "debuginfo" => set_distributed_argument(&mut self.codegen.debuginfo, Some(value)),
      "debug-assertions" => set_distributed_bool(&mut self.codegen.debug_assertions, value),
      "embed-bitcode" => set_distributed_bool(&mut self.codegen.embed_bitcode, value),
      "linker-plugin-lto" => set_distributed_bool(&mut self.codegen.linker_plugin_lto, value),
      "lto" => set_distributed_argument(&mut self.codegen.lto, Some(value)),
      "opt-level" => set_distributed_argument(&mut self.codegen.opt_level, Some(value)),
      "overflow-checks" => set_distributed_bool(&mut self.codegen.overflow_checks, value),
      "panic" => set_distributed_argument(&mut self.codegen.panic, Some(value)),
      "prefer-dynamic" => set_distributed_bool(&mut self.codegen.prefer_dynamic, value),
      "split-debuginfo" => set_distributed_argument(&mut self.codegen.split_debuginfo, Some(value)),
      "strip" => set_distributed_argument(&mut self.codegen.strip, Some(value)),
      _ => Err("distributed_argument_shape_ineligible"),
    }
  }

  fn capture_cargo_error_format(&mut self, value: Option<&str>) -> Result<(), &'static str> {
    if self.cargo_error_format_seen || value != Some("json") {
      return Err("distributed_argument_shape_ineligible");
    }
    self.cargo_error_format_seen = true;
    Ok(())
  }

  fn capture_cargo_json(&mut self, value: Option<&str>) -> Result<(), &'static str> {
    if self.cargo_json_seen || value != Some("diagnostic-rendered-ansi,artifacts,future-incompat") {
      return Err("distributed_argument_shape_ineligible");
    }
    self.cargo_json_seen = true;
    Ok(())
  }

  fn capture_check_cfg(&mut self, value: Option<&str>) -> Result<(), &'static str> {
    let value = value
      .filter(|value| !value.is_empty())
      .ok_or("distributed_argument_shape_ineligible")?;
    if self.check_cfg.iter().any(|existing| existing == value) {
      return Err("distributed_argument_shape_ineligible");
    }
    self.check_cfg.push(value.to_string());
    Ok(())
  }

  fn capture_cfg(&mut self, value: Option<&str>) -> Result<(), &'static str> {
    let value = value
      .filter(|value| !value.is_empty())
      .ok_or("distributed_argument_shape_ineligible")?;
    self.cfg.push(value.to_string());
    Ok(())
  }

  fn capture_cap_lints(&mut self, value: Option<&str>) -> Result<(), &'static str> {
    let value = value.filter(|value| matches!(*value, "allow" | "warn" | "deny" | "forbid"));
    set_distributed_argument(&mut self.cap_lints, value)
  }

  fn capture_color(&mut self, value: Option<&str>) -> Result<(), &'static str> {
    let value = value.filter(|value| matches!(*value, "auto" | "always" | "never"));
    set_distributed_argument(&mut self.color, value)
  }

  fn capture_diagnostic_width(&mut self, value: Option<&str>) -> Result<(), &'static str> {
    let value = value
      .and_then(|value| value.parse::<u32>().ok())
      .filter(|value| *value > 0 && *value <= 65_535)
      .ok_or("distributed_argument_shape_ineligible")?;
    if self.diagnostic_width.replace(value).is_some() {
      return Err("distributed_argument_shape_ineligible");
    }
    Ok(())
  }

  fn capture_lint(&mut self, option: &str, value: Option<&str>) -> Result<(), &'static str> {
    use crate::compiler::distributed::RustLibraryLintLevel;

    let level = match option {
      "--allow" | "-A" => RustLibraryLintLevel::Allow,
      "--warn" | "-W" => RustLibraryLintLevel::Warn,
      "--deny" | "-D" => RustLibraryLintLevel::Deny,
      "--forbid" | "-F" => RustLibraryLintLevel::Forbid,
      _ => return Err("distributed_argument_shape_ineligible"),
    };
    let name = value
      .filter(|value| !value.is_empty())
      .ok_or("distributed_argument_shape_ineligible")?;
    self.lints.push(crate::compiler::distributed::RustLibraryLint {
      level,
      name: name.to_string(),
    });
    Ok(())
  }

  fn capture_library_search(&mut self, value: Option<&str>) -> Result<(), &'static str> {
    let value = value
      .and_then(|value| value.strip_prefix("dependency="))
      .filter(|value| !value.is_empty())
      .ok_or("distributed_argument_shape_ineligible")?;
    set_distributed_argument(&mut self.output_dependency_search, Some(value))
  }

  fn execution_options(&self) -> crate::compiler::distributed::RustLibraryExecutionOptions {
    crate::compiler::distributed::RustLibraryExecutionOptions {
      cap_lints: self.cap_lints.clone(),
      cargo_json_diagnostics: self.cargo_error_format_seen,
      check_cfg: self.check_cfg.clone(),
      codegen: self.codegen.clone(),
      color: self.color.clone(),
      cfg: self.cfg.clone(),
      diagnostic_width: self.diagnostic_width,
      lints: self.lints.clone(),
      output_dependency_search: self.output_dependency_search.is_some(),
    }
  }

  fn capture_workspace_remap(&mut self, value: Option<&str>) -> Result<(), &'static str> {
    if self.workspace_remap_seen || value.is_none_or(|value| !distributed_workspace_remap(value)) {
      return Err("distributed_argument_shape_ineligible");
    }
    self.workspace_remap_seen = true;
    Ok(())
  }
}

fn set_distributed_argument(slot: &mut Option<String>, value: Option<&str>) -> Result<(), &'static str> {
  let value = value
    .filter(|value| !value.is_empty())
    .ok_or("distributed_argument_shape_ineligible")?;
  if slot.replace(value.to_string()).is_some() {
    return Err("distributed_argument_shape_ineligible");
  }
  Ok(())
}

fn set_distributed_bool(slot: &mut Option<bool>, value: &str) -> Result<(), &'static str> {
  let value = match value {
    "yes" => true,
    "no" => false,
    _ => return Err("distributed_argument_shape_ineligible"),
  };
  if slot.replace(value).is_some() {
    return Err("distributed_argument_shape_ineligible");
  }
  Ok(())
}

fn set_distributed_u32(slot: &mut Option<u32>, value: &str) -> Result<(), &'static str> {
  let value = value
    .parse::<u32>()
    .ok()
    .filter(|value| *value > 0)
    .ok_or("distributed_argument_shape_ineligible")?;
  if slot.replace(value).is_some() {
    return Err("distributed_argument_shape_ineligible");
  }
  Ok(())
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

fn complete_compiler_observation(observation: &RawCompilerInvocation) -> bool {
  let [declared] = observation.declared_inputs.as_slice() else {
    return false;
  };
  observation.observed_reads.iter().any(|observed| observed == declared)
    && observation
      .dependency_artifacts
      .iter()
      .all(|(_, artifact)| matches!(&artifact.path, ObservationPath::Repository(_)))
}

fn output_contract_matches(outputs: &[NativeCompilerOutput], observation: &RawCompilerInvocation) -> bool {
  let mut expected = vec![("dep_info", DEP_INFO_SLOT)];
  if observation.emit_modes.contains("metadata") {
    expected.push(("metadata", METADATA_SLOT));
    if NativeOutputRole::from_invocation(&observation.crate_types, observation.test_mode)
      == Some(NativeOutputRole::Rlib)
      && observation.emit_modes.contains("link")
    {
      expected.push(("rlib", RLIB_SLOT));
    }
  } else {
    expected.push(
      match NativeOutputRole::from_invocation(&observation.crate_types, observation.test_mode) {
        Some(NativeOutputRole::Executable) => ("executable", EXECUTABLE_SLOT),
        Some(NativeOutputRole::ProcMacro) => ("proc_macro", PROC_MACRO_SLOT),
        Some(NativeOutputRole::Dylib) => ("dylib", DYLIB_SLOT),
        Some(NativeOutputRole::Cdylib) => ("cdylib", CDYLIB_SLOT),
        Some(NativeOutputRole::Staticlib) => ("staticlib", STATICLIB_SLOT),
        Some(NativeOutputRole::Metadata | NativeOutputRole::Rlib) | None => return false,
      },
    );
  }
  outputs.len() == expected.len()
    && outputs
      .iter()
      .zip(&expected)
      .all(|(output, (role, slot))| output.role == *role && output.slot == *slot)
}

#[cfg(unix)]
fn valid_native_output_mode(role: &str, mode: u32) -> bool {
  let executable = matches!(role, "executable" | "proc_macro" | "dylib" | "cdylib");
  mode & !0o777 == 0 && mode & 0o400 != 0 && (mode & 0o111 != 0) == executable
}

#[cfg(not(unix))]
fn valid_native_output_mode(_role: &str, mode: u32) -> bool {
  matches!(mode, 0o444 | 0o644)
}

fn outputs_match_observation(outputs: &[NativeCompilerOutput], observed: &[FileObservation]) -> bool {
  if outputs.len() < 2 || observed.len() != outputs.len() {
    return false;
  }
  let mut matched = BTreeSet::new();
  for output in observed {
    if output.symlink_target.is_some() {
      return false;
    }
    let Some((index, _)) = outputs.iter().enumerate().find(|(index, expected)| {
      !matched.contains(index)
        && observation_path_basename(&output.path) == Some(expected.file_name.as_str())
        && output_role_path_matches(&expected.role, &output.path.resolve(Path::new("/")))
        && output.executable == matches!(expected.role.as_str(), "executable" | "proc_macro" | "dylib" | "cdylib")
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
    return Some(if observation.test_mode {
      "doctest_execution_result_authority_unavailable"
    } else {
      "rustdoc_output_tree_observation_unavailable"
    });
  }
  if observation
    .target_argument
    .as_deref()
    .is_some_and(|target| target != host_target)
  {
    return Some("cross_target_toolchain_evidence_unavailable");
  }
  let output_role = NativeOutputRole::from_invocation(&observation.crate_types, observation.test_mode);
  let library = output_role == Some(NativeOutputRole::Rlib);
  let metadata = BTreeSet::from(["dep-info".to_string(), "metadata".to_string()]);
  let metadata_and_rlib = BTreeSet::from(["dep-info".to_string(), "link".to_string(), "metadata".to_string()]);
  let linked_emit = BTreeSet::from(["dep-info".to_string(), "link".to_string()]);
  let linked = observation.emit_modes == linked_emit && output_role.is_some_and(NativeOutputRole::requires_linker);
  let compiler_only = output_role.is_some() && observation.emit_modes == metadata;
  let compiler_archive = output_role == Some(NativeOutputRole::Staticlib) && observation.emit_modes == linked_emit;
  if !library && !linked && !compiler_only && !compiler_archive {
    return Some("compiler_output_contract_unavailable");
  }
  if library && observation.emit_modes != metadata && observation.emit_modes != metadata_and_rlib {
    return Some("compiler_emit_contract_unavailable");
  }
  if compiler_long_option_value(&observation.compiler_arguments, "--error-format")
    .is_some_and(|format| !matches!(format, "json" | "human" | "short"))
  {
    return Some("compiler_diagnostic_replay_unavailable");
  }
  if observation.compiler_arguments.iter().any(|argument| argument == "-") {
    return Some("compiler_stdin_observation_unavailable");
  }
  if let Some(reason) = compiler_linker_configuration_bypass_reason(&observation.compiler_arguments) {
    return Some(reason);
  }
  if linked && let Some(reason) = platform_linker_bypass_reason(std::env::consts::OS) {
    return Some(reason);
  }
  if observation
    .compiler_arguments
    .iter()
    .any(|argument| argument.contains("incremental="))
  {
    return Some("incremental_work_product_observation_unavailable");
  }
  if !supported_pathless_toolchain_externs(&observation.compiler_arguments, &observation.crate_types) {
    return Some("dependency_artifact_path_unavailable");
  }
  if let Some(reason) = compiler_argument_bypass_reason(&observation.compiler_arguments, None) {
    return Some(reason);
  }
  if let Some(reason) = observation.dependency_artifacts.iter().find_map(|(_, artifact)| {
    dependency_artifact_bypass_reason(
      artifact
        .path
        .resolve(Path::new("/"))
        .extension()
        .and_then(OsStr::to_str),
    )
  }) {
    return Some(reason);
  }
  if observation
    .environment_reads
    .iter()
    .any(|environment| environment.secret_capability)
  {
    return Some("secret_compiler_environment");
  }
  if let Some(reason) = observation.bypasses.iter().next() {
    return Some(match reason.as_str() {
      "declared_input_bytes_unavailable" => "declared_input_bytes_unavailable",
      "declared_input_symlink_unavailable" => "declared_input_symlink_unavailable",
      "dep-info_output_path_unavailable" => "dep_info_output_path_unavailable",
      "dep_info_path_unavailable" => "dep_info_path_unavailable",
      "dep_info_unavailable" => "dep_info_observation_unavailable",
      "dep_info_output_bytes_unavailable" => "dep_info_output_bytes_unavailable",
      "dep_info_output_symlink_unavailable" => "dep_info_output_symlink_unavailable",
      "dependency_artifact_bytes_unavailable" => "dependency_artifact_bytes_unavailable",
      "dependency_artifact_path_unavailable" => "dependency_artifact_path_unavailable",
      "dependency_artifact_symlink_unavailable" => "dependency_artifact_symlink_unavailable",
      "emitted_output_bytes_unavailable" => "compiler_emitted_output_bytes_unavailable",
      "emitted_output_symlink_unavailable" => "compiler_emitted_output_symlink_unavailable",
      "link_output_path_unavailable" => "link_output_path_unavailable",
      "metadata_output_path_unavailable" => "metadata_output_path_unavailable",
      "non_utf8_compiler_argument" => "non_utf8_compiler_argument",
      "response_file_expansion_unavailable" => "response_file_expansion_unavailable",
      "rlib_output_path_unavailable" => "rlib_output_path_unavailable",
      "rustdoc_dep_info_unavailable" => "rustdoc_dep_info_unavailable",
      "rustdoc_external_tool_identity_unavailable" => "rustdoc_external_tool_identity_unavailable",
      "rustdoc_output_tree_unavailable" => "rustdoc_output_tree_observation_unavailable",
      _ => "compiler_observation_bypass_reason_unrecognized",
    });
  }
  if observation.declared_inputs.is_empty() {
    return Some("declared_compiler_inputs_unavailable");
  }
  let expected_outputs = if library && observation.emit_modes.contains("link") {
    3
  } else {
    2
  };
  if complete && observation.observed_reads.is_empty() {
    return Some("compiler_observed_read_set_unavailable");
  }
  if complete && observation.emitted_outputs.len() != expected_outputs {
    return Some("compiler_emitted_output_set_unavailable");
  }
  None
}

/// Return a sufficient acquisition-free bypass for an obviously unsupported rustc shape.
///
/// The complete classifier still runs after observation. This prefilter only
/// recognizes shapes that cannot enter the graduated class, so it can never
/// turn an eligible invocation into a cache hit or store under a second rule.
pub(crate) fn fast_bypass_reason(program: &OsStr, arguments: &[OsString]) -> Option<&'static str> {
  if std::env::var_os("CARGO_TARGET_DIR").is_some() {
    return Some("custom_target_directory_authority_unavailable");
  }
  if Path::new(program)
    .file_stem()
    .and_then(OsStr::to_str)
    .is_some_and(|name| name.eq_ignore_ascii_case("clippy-driver"))
  {
    return Some("clippy_diagnostic_result_authority_unavailable");
  }
  let mut crate_types = BTreeSet::new();
  let mut emit_seen = false;
  let mut emits_dep_info = false;
  let mut emits_metadata = false;
  let mut emits_link = false;
  let mut emit_supported = true;
  let mut diagnostic_format_supported = true;
  let mut output_directory = false;
  let mut pathless_externs = BTreeSet::new();
  let mut test_mode = false;
  let mut argument_text = Vec::with_capacity(arguments.len());

  for (index, argument) in arguments.iter().enumerate() {
    let Some(argument) = argument.to_str() else {
      return Some("non_utf8_compiler_argument");
    };
    argument_text.push(argument);
    let next = || arguments.get(index + 1).and_then(|argument| argument.to_str());
    if matches!(argument, "-h" | "--help" | "-V" | "--version" | "-vV" | "--print") || argument.starts_with("--print=")
    {
      return Some("compiler_information_request");
    }
    if argument.starts_with('@') {
      return Some("response_file_expansion_unavailable");
    }
    if argument == "--test" {
      test_mode = true;
    }
    if argument == "-" {
      return Some("compiler_stdin_observation_unavailable");
    }
    if argument.contains("incremental=") {
      return Some("incremental_work_product_observation_unavailable");
    }
    if let Some(option) = short_option_value(argument, next(), "-C")
      && let Some(reason) = linker_option_bypass_reason(option)
    {
      return Some(reason);
    }
    if let Some(value) = inline_or_next(argument, next(), "--crate-type") {
      crate_types.extend(value.split(',').map(str::to_string));
    }
    if let Some(value) = inline_or_next(argument, next(), "--emit") {
      emit_seen = true;
      for mode in value
        .split(',')
        .map(|mode| mode.split_once('=').map_or(mode, |(name, _)| name))
      {
        match mode {
          "dep-info" => emits_dep_info = true,
          "metadata" => emits_metadata = true,
          "link" => emits_link = true,
          _ => emit_supported = false,
        }
      }
    }
    if let Some(value) = inline_or_next(argument, next(), "--error-format") {
      diagnostic_format_supported &= matches!(value, "json" | "human" | "short");
    }
    if let Some(value) = inline_or_next(argument, next(), "--extern") {
      let Some((_, artifact)) = value.split_once('=') else {
        pathless_externs.insert(value.to_string());
        continue;
      };
      if let Some(reason) = dependency_artifact_bypass_reason(Path::new(artifact).extension().and_then(OsStr::to_str)) {
        return Some(reason);
      }
    }
    if inline_or_next(argument, next(), "--out-dir").is_some() {
      output_directory = true;
    }
  }

  let output_role = NativeOutputRole::from_invocation(&crate_types, test_mode);
  let library = output_role == Some(NativeOutputRole::Rlib);
  let linked = emits_link && !emits_metadata && output_role.is_some_and(NativeOutputRole::requires_linker);
  if linked && let Some(reason) = platform_linker_bypass_reason(std::env::consts::OS) {
    return Some(reason);
  }
  let compiler_only = output_role.is_some() && emits_metadata && !emits_link;
  let compiler_archive = output_role == Some(NativeOutputRole::Staticlib) && emits_link && !emits_metadata;
  if !pathless_externs.is_empty()
    && (crate_types != BTreeSet::from(["proc-macro".to_string()])
      || pathless_externs != BTreeSet::from(["proc_macro".to_string()]))
  {
    return Some("dependency_artifact_path_unavailable");
  }
  if !library && !linked && !compiler_only && !compiler_archive {
    return Some("compiler_output_contract_unavailable");
  }
  if !emit_seen || !emit_supported || !emits_dep_info || library && !emits_metadata {
    return Some("compiler_emit_contract_unavailable");
  }
  if !diagnostic_format_supported {
    return Some("compiler_diagnostic_replay_unavailable");
  }
  if !output_directory {
    return Some("compiler_output_paths_unavailable");
  }
  let current_directory = std::env::current_dir().ok();
  compiler_argument_bypass_reason(&argument_text, current_directory.as_deref())
}

fn inline_or_next<'a>(argument: &'a str, next: Option<&'a str>, option: &str) -> Option<&'a str> {
  if argument == option {
    next
  } else {
    argument.strip_prefix(option).and_then(|value| value.strip_prefix('='))
  }
}

fn short_option_value<'a>(argument: &'a str, next: Option<&'a str>, option: &str) -> Option<&'a str> {
  if argument == option {
    next
  } else {
    argument.strip_prefix(option).filter(|value| !value.is_empty())
  }
}

fn platform_linker_bypass_reason(os: &str) -> Option<&'static str> {
  match os {
    "macos" | "linux" => None,
    "windows" => Some("coff_linker_evidence_unavailable"),
    _ => Some("platform_linker_evidence_unavailable"),
  }
}

fn linker_option_bypass_reason(option: &str) -> Option<&'static str> {
  match option.split_once('=').map_or(option, |(name, _)| name) {
    "linker" => Some("explicit_linker_evidence_unavailable"),
    "link-arg" | "link-args" => Some("explicit_link_argument_evidence_unavailable"),
    "dlltool" | "link-self-contained" | "linker-features" | "linker-flavor" => {
      Some("linker_configuration_evidence_unavailable")
    }
    _ => None,
  }
}

fn compiler_linker_configuration_bypass_reason(arguments: &[String]) -> Option<&'static str> {
  let mut index = 0usize;
  while index < arguments.len() {
    let argument = &arguments[index];
    if let Some(option) = short_option_value(
      argument,
      arguments.get(index.saturating_add(1)).map(String::as_str),
      "-C",
    ) && let Some(reason) = linker_option_bypass_reason(option)
    {
      return Some(reason);
    }
    index = index.saturating_add(if argument == "-C" { 2 } else { 1 });
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

fn pathless_extern_names(arguments: &[String]) -> Vec<&str> {
  let mut names = Vec::new();
  let mut index = 0usize;
  while index < arguments.len() {
    if arguments[index] == "--extern" {
      if let Some(value) = arguments.get(index + 1)
        && !value.contains('=')
      {
        names.push(value.as_str());
      }
      index = index.saturating_add(2);
    } else {
      if let Some(value) = arguments[index].strip_prefix("--extern=")
        && !value.contains('=')
      {
        names.push(value);
      }
      index = index.saturating_add(1);
    }
  }
  names
}

fn supported_pathless_toolchain_externs(arguments: &[String], crate_types: &BTreeSet<String>) -> bool {
  let names = pathless_extern_names(arguments);
  names.is_empty()
    || crate_types == &BTreeSet::from(["proc-macro".to_string()]) && names.iter().all(|name| *name == "proc_macro")
}

fn compiler_argument_bypass_reason<T: AsRef<str>>(
  arguments: &[T],
  current_directory: Option<&Path>,
) -> Option<&'static str> {
  let mut index = 0usize;
  let mut source_inputs = 0usize;
  while index < arguments.len() {
    let argument = arguments[index].as_ref();
    let next = arguments.get(index + 1).map(AsRef::as_ref);
    if matches!(
      argument,
      "--crate-name"
        | "--crate-type"
        | "--emit"
        | "--out-dir"
        | "--target"
        | "--edition"
        | "--error-format"
        | "--json"
        | "--cfg"
        | "--check-cfg"
        | "--cap-lints"
        | "--color"
        | "--diagnostic-width"
        | "--allow"
        | "--warn"
        | "--deny"
        | "--forbid"
        | "--remap-path-prefix"
        | "--extern"
        | "-L"
        | "-l"
        | "-C"
        | "-Z"
        | "-A"
        | "-W"
        | "-D"
        | "-F"
    ) && next.is_none()
    {
      return Some("compiler_option_value_unavailable");
    }
    let consumes_next = match argument {
      "--crate-name" | "--crate-type" | "--emit" | "--out-dir" | "--target" | "--edition" | "--error-format"
      | "--json" | "--cfg" | "--check-cfg" | "--cap-lints" | "--color" | "--diagnostic-width" | "--allow"
      | "--warn" | "--deny" | "--forbid" => next.is_some(),
      "--remap-path-prefix" if next.is_some_and(|value| distributed_workspace_remap_at(value, current_directory)) => {
        true
      }
      "--remap-path-prefix" => return Some("remapped_path_observation_unavailable"),
      "--extern" => next.is_some_and(|value| value.contains('=') || value == "proc_macro"),
      "-L" => next.is_some_and(supported_library_search),
      "-l" => next.is_some_and(supported_native_library),
      "-C" => next.is_some_and(supported_codegen_option),
      "-Z" => next.is_some_and(supported_unstable_option),
      "-A" | "-W" | "-D" | "-F" => next.is_some(),
      "--test" => false,
      _ if argument.starts_with("--crate-name=")
        || argument.starts_with("--crate-type=")
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
        || argument.starts_with("-A") && argument.len() > 2
        || argument.starts_with("-W") && argument.len() > 2
        || argument.starts_with("-D") && argument.len() > 2
        || argument.starts_with("-F") && argument.len() > 2 =>
      {
        false
      }
      _ if argument
        .strip_prefix("--remap-path-prefix=")
        .is_some_and(|value| distributed_workspace_remap_at(value, current_directory)) =>
      {
        false
      }
      _ if argument.starts_with("-L") && argument.len() > 2 => {
        if !supported_library_search(argument.trim_start_matches("-L")) {
          return Some("library_search_input_evidence_unavailable");
        }
        false
      }
      _ if argument.starts_with("-l") && argument.len() > 2 => {
        if !supported_native_library(argument.trim_start_matches("-l")) {
          return Some(native_library_bypass_reason(argument.trim_start_matches("-l")));
        }
        false
      }
      _ if argument.starts_with("-C") && argument.len() > 2 => {
        if !supported_codegen_option(argument.trim_start_matches("-C")) {
          return Some("codegen_option_input_evidence_unavailable");
        }
        false
      }
      _ if argument.starts_with("-Z") && argument.len() > 2 => {
        let option = argument.trim_start_matches("-Z");
        if !supported_unstable_option(option) {
          return Some(unstable_option_bypass_reason(option));
        }
        false
      }
      _ if !argument.starts_with('-') && argument.ends_with(".rs") => {
        source_inputs += 1;
        false
      }
      _ if argument.starts_with("--remap-path-prefix") || argument.starts_with("--remap-path-scope") => {
        return Some("remapped_path_observation_unavailable");
      }
      _ => return Some("compiler_option_input_evidence_unavailable"),
    };
    if consumes_next && next.is_none() {
      return Some("compiler_option_value_unavailable");
    }
    if argument == "-L" && next.is_some_and(|value| !supported_library_search(value)) {
      return Some("library_search_input_evidence_unavailable");
    }
    if argument == "-l"
      && let Some(value) = next.filter(|value| !supported_native_library(value))
    {
      return Some(native_library_bypass_reason(value));
    }
    if argument == "-C" && next.is_some_and(|value| !supported_codegen_option(value)) {
      return Some("codegen_option_input_evidence_unavailable");
    }
    if argument == "-Z"
      && let Some(value) = next.filter(|value| !supported_unstable_option(value))
    {
      return Some(unstable_option_bypass_reason(value));
    }
    index += usize::from(consumes_next) + 1;
  }
  (source_inputs != 1).then_some("compiler_source_input_observation_unavailable")
}

fn distributed_workspace_remap(value: &str) -> bool {
  value.strip_prefix("repository:=") == Some(crate::compiler::distributed::VIRTUAL_WORKSPACE)
}

fn distributed_workspace_remap_at(value: &str, current_directory: Option<&Path>) -> bool {
  if distributed_workspace_remap(value) {
    return true;
  }
  let Some((source, destination)) = value.rsplit_once('=') else {
    return false;
  };
  destination == crate::compiler::distributed::VIRTUAL_WORKSPACE
    && current_directory.is_some_and(|current_directory| {
      let source = Path::new(source);
      source == current_directory
        || crate::utils::canonicalize_existing(source)
          .ok()
          .zip(crate::utils::canonicalize_existing(current_directory).ok())
          .is_some_and(|(source, current_directory)| source == current_directory)
    })
}

fn dependency_artifact_bypass_reason(extension: Option<&str>) -> Option<&'static str> {
  match extension {
    Some("rmeta" | "rlib") => None,
    Some("dll" | "dylib" | "so") => Some("dynamic_dependency_execution_observation_unavailable"),
    _ => Some("dependency_artifact_format_observation_unavailable"),
  }
}

fn native_library_bypass_reason(value: &str) -> &'static str {
  if value.starts_with("dylib=") || value.starts_with("framework=") {
    "dynamic_native_library_search_evidence_unavailable"
  } else {
    "native_library_input_evidence_unavailable"
  }
}

fn unstable_option_bypass_reason(value: &str) -> &'static str {
  if value.starts_with("codegen-backend=") {
    "external_codegen_backend_identity_unavailable"
  } else {
    "unstable_compiler_option_evidence_unavailable"
  }
}

fn supported_library_search(value: &str) -> bool {
  if let Some(path) = value.strip_prefix("dependency=") {
    return !path.is_empty() && !path.as_bytes().contains(&0);
  }
  native_search_path(value).is_ok_and(|path| path.is_some())
}

fn supported_native_library(value: &str) -> bool {
  let Some((kind, name)) = value.split_once('=') else {
    return false;
  };
  !name.is_empty()
    && !name.as_bytes().contains(&0)
    && (kind == "static"
      || kind
        .strip_prefix("static:")
        .is_some_and(|modifiers| !modifiers.is_empty() && !modifiers.as_bytes().contains(&0)))
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
      | "prefer-dynamic"
      | "codegen-units"
      | "lto"
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
    distributed_placement: Option<crate::compiler::distributed::PlacementObservation>,
  },
  /// A restore crossed its irreversible effect boundary and must not run rustc.
  OperationalFailure(RailError),
  /// Execute the original invocation unchanged.
  Execute,
}

/// Attempt native reuse and configure the cold child without changing Cargo's wrapper order.
///
/// `arguments` starts with the rustc executable because `program` is Cargo's
/// workspace-wrapper slot. A returned code means verified outputs and streams
/// were already restored; `Execute` preserves the ordinary child execution.
pub(crate) fn configure_outer(program: &OsStr, arguments: &[OsString], command: &mut Command) -> OuterCacheAction {
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
      "incremental_work_product_observation_unavailable",
      None,
      0,
      diagnostic_wrapper,
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
    );
    return OuterCacheAction::Execute;
  };
  let source_root = &context.source_root;
  let source_root_spelling = &context.source_root_spelling;
  let observation_directory = &context.observation_directory;
  let session = context.session.load(source_root);
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
      );
      return OuterCacheAction::Execute;
    }
  };
  if session.authority != NativeSessionAuthority::Exact {
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      "native_cache_session_authority_mismatch",
      None,
      0,
      diagnostic_wrapper,
    );
    return OuterCacheAction::Execute;
  }
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
      );
      return OuterCacheAction::Execute;
    }
  };
  let mut recorder = match crate::compiler::observation::begin_invocation_in(
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
      );
      return OuterCacheAction::Execute;
    }
  };
  let initial_input_bytes = estimated_input_bytes(recorder.observation(), source_root);
  let bypass_reason = invocation_bypass_reason(recorder.observation(), false, &session.class.host_target);
  if let Some(reason) = bypass_reason {
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      reason,
      None,
      initial_input_bytes,
      diagnostic_wrapper,
    );
    return OuterCacheAction::Execute;
  }
  let Some(mut output_paths) = recorder.native_output_paths() else {
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      "compiler_output_paths_unavailable",
      None,
      estimated_input_bytes(recorder.observation(), source_root),
      diagnostic_wrapper,
    );
    return OuterCacheAction::Execute;
  };
  if validated_output_parent(&output_paths, source_root).is_err() {
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      "compiler_output_root_authority_unavailable",
      None,
      estimated_input_bytes(recorder.observation(), source_root),
      diagnostic_wrapper,
    );
    return OuterCacheAction::Execute;
  }
  let cas = open_active_local_cas();
  let cas = match cas {
    Ok(cas) => cas,
    Err(error) => return OuterCacheAction::OperationalFailure(error),
  };
  if let Err(error) = recover_restore_commit_in(&cas, &output_paths, source_root, observation_directory) {
    return OuterCacheAction::OperationalFailure(error);
  }
  let capture = NativeActionCapture::capture(recorder.observation(), source_root);
  let capture_bytes = capture.as_ref().map_or(0, |capture| capture.bytes_hashed);
  let mut capture = match capture {
    Ok(capture) => capture,
    Err(_) => {
      configure_cold(
        command,
        CompilerCacheWrapperStatus::Bypassed,
        "complete_action_capture_unavailable",
        None,
        initial_input_bytes.saturating_add(capture_bytes),
        diagnostic_wrapper,
      );
      return OuterCacheAction::Execute;
    }
  };
  let distributed_worker = context
    .installation
    .as_ref()
    .and_then(crate::cache::installation::InstallationReceipt::local_distributed_worker)
    .map(Path::to_path_buf);
  let distributed_remote = context
    .installation
    .as_ref()
    .and_then(crate::cache::installation::InstallationReceipt::mutual_tls_distributed_worker);
  let mut normalized_compiler_arguments = None;
  let mut distributed_candidate = None;
  let mut normalization_bytes = 0_u64;
  if (distributed_worker.is_some() || distributed_remote.is_some()) && !diagnostic_wrapper {
    let normalized = (|| -> Result<_, &'static str> {
      let initial_candidate = distributed_rust_library_input_candidate(
        recorder.observation(),
        &capture,
        &output_paths,
        source_root,
        source_root_spelling,
      )?;
      let temporary = observation_directory.join("distributed-local-tmp");
      fs::create_dir_all(&temporary).map_err(|_| "distributed_local_temporary_unavailable")?;
      let temporary_metadata =
        fs::symlink_metadata(&temporary).map_err(|_| "distributed_local_temporary_unavailable")?;
      if !temporary_metadata.is_dir() || crate::utils::is_symlink_or_reparse(&temporary_metadata) {
        return Err("distributed_local_temporary_unavailable");
      }
      let normalized_command = initial_candidate
        .normalized_local_command(rustc, source_root, &temporary)
        .map_err(|_| "distributed_normalized_command_unavailable")?;
      let arguments = normalized_command
        .get_args()
        .map(OsStr::to_os_string)
        .collect::<Vec<_>>();
      let normalized_recorder = crate::compiler::observation::begin_invocation_in(
        observation_directory,
        source_root,
        source_root,
        rustc,
        &arguments,
      )
      .map_err(|_| "distributed_normalized_observation_unavailable")?;
      let normalized_output_paths = normalized_recorder
        .native_output_paths()
        .ok_or("distributed_normalized_output_unavailable")?;
      validated_output_parent(&normalized_output_paths, source_root)
        .map_err(|_| "distributed_normalized_output_unavailable")?;
      let normalized_capture = NativeActionCapture::capture(normalized_recorder.observation(), source_root)
        .map_err(|_| "distributed_normalized_capture_unavailable")?;
      let exact_candidate = distributed_rust_library_candidate(
        normalized_recorder.observation(),
        &normalized_capture,
        &normalized_output_paths,
        source_root,
        source_root_spelling,
      )?;
      if !initial_candidate.same_normalized_operation(&exact_candidate) {
        return Err("distributed_normalized_action_changed");
      }
      Ok((
        normalized_recorder,
        normalized_output_paths,
        normalized_capture,
        exact_candidate,
        arguments,
      ))
    })();
    match normalized {
      Ok((normalized_recorder, normalized_output_paths, normalized_capture, candidate, arguments)) => {
        normalization_bytes = initial_input_bytes.saturating_add(capture.bytes_hashed);
        recorder = normalized_recorder;
        output_paths = normalized_output_paths;
        capture = normalized_capture;
        distributed_candidate = Some(candidate);
        normalized_compiler_arguments = Some(arguments);
      }
      Err(reason) if BENCH_COVERAGE_DIRECTORY.get().is_some() => {
        eprintln!("cargo-rail native coverage: distributed normalization unavailable: {reason}");
      }
      Err(_) => {}
    }
  }
  let compiler_arguments = normalized_compiler_arguments.as_deref().unwrap_or(compiler_arguments);
  let observation = recorder.observation();
  let initial_input_bytes = estimated_input_bytes(observation, source_root);
  let base_action = base_action_key(&session.identity, &session.class, observation, &capture).ok();
  let provisional_action = action_key(&session.identity, &session.class, observation, &capture).ok();
  let (base_action, provisional_action) = match (base_action, provisional_action) {
    (Some(base_action), Some(provisional_action)) => (base_action, provisional_action),
    _ => {
      configure_cold(
        command,
        CompilerCacheWrapperStatus::Bypassed,
        "complete_action_capture_unavailable",
        None,
        normalization_bytes.saturating_add(initial_input_bytes.saturating_add(capture.bytes_hashed)),
        diagnostic_wrapper,
      );
      return OuterCacheAction::Execute;
    }
  };
  let distributed_placement = distributed_remote.as_ref().and_then(|(identity, _)| {
    distributed_candidate.as_ref().and_then(|candidate| {
      candidate
        .placement_observation(identity.worker_capability_id, identity.endpoint)
        .ok()
    })
  });
  if capture_test_pause("after_initial_capture", observation).is_err() {
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      "capture_test_pause_failed",
      None,
      initial_input_bytes.saturating_add(capture.bytes_hashed),
      diagnostic_wrapper,
    );
    return OuterCacheAction::Execute;
  }
  let mut metrics = NativeCacheMetrics {
    bytes_hashed: normalization_bytes.saturating_add(initial_input_bytes.saturating_add(capture.bytes_hashed)),
    ..NativeCacheMetrics::default()
  };
  let mut remote_entry = None;
  let mut selector_miss_reason = "environment_selector_not_found";
  let mut environment_names = match cas.native_environment_selector(&base_action) {
    Ok(Some(names)) => Some(names),
    Ok(None) => None,
    Err(_) => {
      configure_cold(
        command,
        CompilerCacheWrapperStatus::Bypassed,
        "local_cache_environment_selector_unavailable",
        Some(provisional_action),
        metrics.bytes_hashed,
        diagnostic_wrapper,
      );
      return OuterCacheAction::Execute;
    }
  };
  if environment_names.is_none()
    && let Some(selection) = active_remote_selection()
  {
    if !selection.direct_transport_supported() {
      selector_miss_reason = "remote_transport_not_qualified";
    } else {
      metrics.begin_remote();
      let started = Instant::now();
      let remote = open_active_remote_store();
      metrics.remote_timing.store_connect.record(started);
      let lookup = remote.and_then(|store| {
        let store =
          store.ok_or_else(|| crate::remote_cache::RemoteStoreError::unavailable("remote store is inactive"))?;
        let started = Instant::now();
        let lookup = store.lookup(&base_action);
        metrics.remote_timing.lookup.record(started);
        lookup
      });
      match lookup {
        Ok(crate::remote_cache::RemoteLookup::Unique {
          environment_names: names,
          action_key,
          result_key,
          body,
          bytes,
          compressed_bytes,
        }) if selection.approves_environment_names(&names) => {
          match cas.publish_native_environment_selector(&base_action, &names) {
            Ok(crate::cache::cas::NativeEnvironmentSelectorPublication::Created)
            | Ok(crate::cache::cas::NativeEnvironmentSelectorPublication::Converged) => {
              environment_names = Some(names.clone());
              remote_entry = Some(crate::remote_cache::RemoteLookup::Unique {
                environment_names: names,
                action_key,
                result_key,
                body,
                bytes,
                compressed_bytes,
              });
            }
            Ok(crate::cache::cas::NativeEnvironmentSelectorPublication::Diverged) | Err(_) => {
              selector_miss_reason = "remote_environment_selector_admission_failed";
            }
          }
        }
        Ok(crate::remote_cache::RemoteLookup::Unique { .. }) => {
          selector_miss_reason = "remote_environment_not_shareable";
        }
        Ok(crate::remote_cache::RemoteLookup::Conflict) => {
          selector_miss_reason = "remote_entry_conflicted";
        }
        Ok(crate::remote_cache::RemoteLookup::Miss) => {
          selector_miss_reason = "remote_entry_not_found";
        }
        Err(error) => selector_miss_reason = error.cold_reason(),
      }
    }
  }
  let environment_names = match environment_names {
    Some(names) => names,
    None => {
      if let Some(candidate) = distributed_candidate.as_ref()
        && prepare_distributed_local_fallback(command, rustc, candidate, source_root, observation_directory).is_err()
      {
        configure_cold(
          command,
          CompilerCacheWrapperStatus::Bypassed,
          "distributed_normalized_fallback_unavailable",
          Some(provisional_action),
          metrics.bytes_hashed,
          diagnostic_wrapper,
        );
        return OuterCacheAction::Execute;
      }
      let metadata = configure_cold(
        command,
        CompilerCacheWrapperStatus::Miss,
        selector_miss_reason,
        Some(provisional_action.clone()),
        metrics.bytes_hashed,
        diagnostic_wrapper,
      );
      let mut recorder = recorder;
      recorder.set_cache_wrapper(metadata);
      if distributed_candidate.is_none() {
        prepare_observed_cold_child(
          command,
          rustc,
          compiler_arguments,
          diagnostic_wrapper,
          recorder.observation(),
          observation_directory,
        );
      }
      return OuterCacheAction::Store {
        recorder,
        capture,
        base_action_key: base_action,
        cache_bytes_read: 0,
        distributed_placement,
      };
    }
  };
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
      );
      return OuterCacheAction::Execute;
    }
  }
  let pre_link_action = match action_key(&session.identity, &session.class, observation, &capture) {
    Ok(action) => action,
    Err(_) => {
      configure_cold(
        command,
        CompilerCacheWrapperStatus::Bypassed,
        "selected_action_identity_unavailable",
        Some(provisional_action),
        metrics.bytes_hashed,
        diagnostic_wrapper,
      );
      return OuterCacheAction::Execute;
    }
  };
  let linked = linked_observation(observation);
  let lookup_key = if linked {
    match link_candidate_selector(&pre_link_action) {
      Ok(selector) => selector,
      Err(_) => {
        configure_cold(
          command,
          CompilerCacheWrapperStatus::Bypassed,
          "link_candidate_selector_unavailable",
          Some(pre_link_action),
          metrics.bytes_hashed,
          diagnostic_wrapper,
        );
        return OuterCacheAction::Execute;
      }
    }
  } else {
    pre_link_action.clone()
  };
  let cached = lookup_native_action(
    &cas,
    &session,
    &lookup_key,
    &pre_link_action,
    &capture,
    observation,
    context
      .installation
      .as_ref()
      .map(crate::cache::installation::InstallationReceipt::authority),
  );
  if let Ok((_, bytes_hashed)) = &cached {
    metrics.bytes_hashed = metrics.bytes_hashed.saturating_add(*bytes_hashed);
  }
  let cached = match cached {
    Ok((cached, _)) => cached,
    Err(error) => {
      if BENCH_COVERAGE_DIRECTORY.get().is_some() {
        eprintln!("cargo-rail native coverage: local linked lookup unavailable: {error}");
      }
      configure_cold(
        command,
        CompilerCacheWrapperStatus::Bypassed,
        "local_cache_action_state_unavailable",
        Some(lookup_key),
        metrics.bytes_hashed,
        diagnostic_wrapper,
      );
      return OuterCacheAction::Execute;
    }
  };
  let (mut miss_reason, local_miss) = match cached {
    crate::cache::cas::NativeActionLookup::Hit(cached)
      if cached.validation.session_identity == session.identity
        && cached.validation.class == session.class
        && (!linked
          || platform_linker_witness_is_valid(
            observation,
            &cached.validation.witness,
            cached.validation.linker_generations.as_ref(),
          ))
        && (linked || cached.validation.action_key == pre_link_action)
        && capture.validates_witness(&cached.validation.witness, observation) =>
    {
      metrics.cache_bytes_read = metrics.cache_bytes_read.saturating_add(cached.bytes_read);
      match restore_and_publish(
        context,
        &cas,
        NativeRestoreSource::Materialized {
          cached: &cached,
          hit_source: NativeHitSource::Local,
        },
        &capture,
        observation,
        &output_paths,
        &mut metrics,
      ) {
        Ok(()) => return OuterCacheAction::Hit(0),
        Err(RestorePublishFailure::BeforeEffect(error)) => {
          drop(error);
          ("verified_result_materialization_failed".to_string(), false)
        }
        Err(RestorePublishFailure::AfterEffect(error) | RestorePublishFailure::Operational(error)) => {
          return OuterCacheAction::OperationalFailure(error);
        }
      }
    }
    crate::cache::cas::NativeActionLookup::Hit(cached) => {
      metrics.cache_bytes_read = metrics.cache_bytes_read.saturating_add(cached.bytes_read);
      ("action_descriptor_incompatible".to_string(), false)
    }
    crate::cache::cas::NativeActionLookup::Packed(cached) => {
      metrics.cache_bytes_read = metrics.cache_bytes_read.saturating_add(cached.bytes_read);
      match attempt_local_packed_reuse(
        &cas,
        &cached,
        &session,
        &capture,
        observation,
        &output_paths,
        &mut metrics,
      ) {
        PackedLocalReuse::Hit => return OuterCacheAction::Hit(0),
        PackedLocalReuse::Cold(reason) => (reason.to_string(), false),
        PackedLocalReuse::Reject(reason) => {
          let action_key = cached.action_key().to_string();
          drop(cached);
          let _ = cas.quarantine_packed_native_action(&action_key, reason);
          (reason.to_string(), false)
        }
        PackedLocalReuse::Operational(error) => return OuterCacheAction::OperationalFailure(error),
      }
    }
    crate::cache::cas::NativeActionLookup::Miss(miss) => {
      metrics.cache_bytes_read = metrics.cache_bytes_read.saturating_add(miss.bytes_read);
      (miss.reason, true)
    }
  };
  if local_miss
    && let Some(selection) = active_remote_selection()
    && selection.direct_transport_supported()
    && selection.approves_environment_names(&environment_names)
  {
    metrics.begin_remote();
    let started = Instant::now();
    let remote = open_active_remote_store();
    metrics.remote_timing.store_connect.record(started);
    match remote {
      Ok(Some(remote)) => match attempt_direct_remote_reuse(
        &cas,
        remote,
        selection,
        &session,
        &pre_link_action,
        &base_action,
        &environment_names,
        remote_entry,
        &capture,
        observation,
        &output_paths,
        &mut metrics,
      ) {
        DirectRemoteReuse::Hit => return OuterCacheAction::Hit(0),
        DirectRemoteReuse::Cold(reason) => miss_reason = reason.to_string(),
        DirectRemoteReuse::Operational(error) => return OuterCacheAction::OperationalFailure(error),
      },
      Ok(None) => miss_reason = "remote_cache_unavailable".to_string(),
      Err(error) => miss_reason = error.cold_reason().to_string(),
    }
  }
  if local_miss
    && environment_names.is_empty()
    && let Some(candidate) = distributed_candidate.as_ref()
  {
    let attempt = if let Some((identity, policy)) = distributed_remote.as_ref() {
      let decision = match policy {
        crate::cache::installation::DistributedPlacementPolicy::Qualification => {
          crate::compiler::distributed::PlacementDecision::Delegate
        }
        crate::cache::installation::DistributedPlacementPolicy::Automatic => distributed_placement
          .as_ref()
          .zip(context.installation.as_ref())
          .map_or(
            crate::compiler::distributed::PlacementDecision::Local("distributed_cost_history_unavailable"),
            |(placement, receipt)| crate::compiler::distributed::automatic_placement(receipt, placement),
          ),
      };
      match decision {
        crate::compiler::distributed::PlacementDecision::Delegate => {
          let started = Instant::now();
          let attempt = crate::compiler::distributed::execute_and_admit_mutual_tls_worker(
            identity,
            rustc,
            context,
            &cas,
            &session,
            &capture,
            &base_action,
            observation,
            &output_paths,
            candidate,
            metrics.cache_bytes_read,
            *policy == crate::cache::installation::DistributedPlacementPolicy::Qualification,
          );
          if let (Some(receipt), Some(placement)) = (context.installation.as_ref(), distributed_placement.as_ref()) {
            match &attempt {
              crate::compiler::distributed::LocalAttemptDecision::Completed(_) => {
                crate::compiler::distributed::record_remote_placement(receipt, placement, started.elapsed(), true);
              }
              crate::compiler::distributed::LocalAttemptDecision::Fallback(_) => {
                crate::compiler::distributed::record_remote_placement(receipt, placement, started.elapsed(), false);
              }
              crate::compiler::distributed::LocalAttemptDecision::CompilerFailed { .. }
              | crate::compiler::distributed::LocalAttemptDecision::OperationalFailure(_) => {}
            }
          }
          attempt
        }
        crate::compiler::distributed::PlacementDecision::Local(reason) => {
          crate::compiler::distributed::LocalAttemptDecision::Fallback(reason)
        }
      }
    } else if let Some(worker) = distributed_worker.as_deref() {
      crate::compiler::distributed::execute_and_admit_local_worker(
        worker,
        rustc,
        context,
        &cas,
        &session,
        &capture,
        &base_action,
        observation,
        &output_paths,
        candidate,
        metrics.cache_bytes_read,
      )
    } else {
      crate::compiler::distributed::LocalAttemptDecision::Fallback("distributed_authority_unavailable")
    };
    match attempt {
      crate::compiler::distributed::LocalAttemptDecision::Completed(exit_code) => {
        return OuterCacheAction::Hit(exit_code);
      }
      crate::compiler::distributed::LocalAttemptDecision::CompilerFailed { termination, result } => {
        if !result.binds_candidate(candidate) {
          miss_reason = "distributed_compiler_failure_action_mismatch".to_string();
        } else {
          let exit_code = match replay_distributed_compiler_failure(*result, termination, source_root) {
            Ok(exit_code) => exit_code,
            Err(error) => return OuterCacheAction::OperationalFailure(error),
          };
          let mut raw = match recorder.complete(false) {
            Ok(raw) => raw,
            Err(_) => return OuterCacheAction::Hit(exit_code),
          };
          let _ = publish_and_record_cold_observation(
            &mut raw,
            "distributed_compiler_execution_failed",
            Some(pre_link_action),
            None,
            metrics.bytes_hashed,
            metrics.cache_bytes_read,
          );
          return OuterCacheAction::Hit(exit_code);
        }
      }
      crate::compiler::distributed::LocalAttemptDecision::Fallback(reason) => {
        miss_reason = reason.to_string();
      }
      crate::compiler::distributed::LocalAttemptDecision::OperationalFailure(error) => {
        return OuterCacheAction::OperationalFailure(error);
      }
    }
  }
  if let Some(candidate) = distributed_candidate.as_ref()
    && prepare_distributed_local_fallback(command, rustc, candidate, source_root, observation_directory).is_err()
  {
    configure_cold(
      command,
      CompilerCacheWrapperStatus::Bypassed,
      "distributed_normalized_fallback_unavailable",
      Some(lookup_key),
      metrics.bytes_hashed,
      diagnostic_wrapper,
    );
    return OuterCacheAction::Execute;
  }
  let metadata = configure_cold(
    command,
    CompilerCacheWrapperStatus::Miss,
    &miss_reason,
    Some(lookup_key.clone()),
    metrics.bytes_hashed,
    diagnostic_wrapper,
  );
  recorder.set_cache_wrapper(metadata);
  if distributed_candidate.is_none() {
    prepare_observed_cold_child(
      command,
      rustc,
      compiler_arguments,
      diagnostic_wrapper,
      recorder.observation(),
      observation_directory,
    );
  }
  OuterCacheAction::Store {
    recorder,
    capture,
    base_action_key: base_action,
    cache_bytes_read: metrics.cache_bytes_read,
    distributed_placement,
  }
}

fn prepare_distributed_local_fallback(
  command: &mut Command,
  rustc: &OsStr,
  candidate: &crate::compiler::distributed::RustLibraryCandidate,
  source_root: &Path,
  observation_directory: &Path,
) -> RailResult<()> {
  let temporary = observation_directory.join("distributed-local-tmp");
  *command = candidate.normalized_local_command(rustc, source_root, &temporary)?;
  suppress_nested_observation(command);
  Ok(())
}

fn replay_distributed_compiler_failure(
  result: crate::compiler::distributed::StagedExecutionResult,
  termination: crate::compiler::distributed::CompilerTermination,
  source_root: &Path,
) -> RailResult<i32> {
  use crate::compiler::distributed::DistributedResultSlot;

  let localize = |bytes: Vec<u8>| -> RailResult<Vec<u8>> {
    let (localized, _) = replace_bytes(
      &bytes,
      crate::compiler::distributed::VIRTUAL_WORKSPACE.as_bytes(),
      &source_root_display_bytes(source_root),
    );
    if localized
      .windows(crate::compiler::distributed::VIRTUAL_ROOT.len())
      .any(|window| window == crate::compiler::distributed::VIRTUAL_ROOT.as_bytes())
    {
      return Err(RailError::message(
        "distributed compiler failure retained an unbound virtual path",
      ));
    }
    Ok(localized)
  };
  let stdout = localize(result.read_verified_frame(DistributedResultSlot::Stdout)?)?;
  let stderr = localize(result.read_verified_frame(DistributedResultSlot::Stderr)?)?;
  let mut stdout_writer = std::io::stdout().lock();
  stdout_writer.write_all(&stdout)?;
  stdout_writer.flush()?;
  let mut stderr_writer = std::io::stderr().lock();
  stderr_writer.write_all(&stderr)?;
  stderr_writer.flush()?;
  Ok(match termination {
    crate::compiler::distributed::CompilerTermination::Exit { code } => code,
    crate::compiler::distributed::CompilerTermination::Signal { .. }
    | crate::compiler::distributed::CompilerTermination::Unknown => 1,
  })
}

fn prepare_original_child(command: &mut Command, diagnostic_wrapper: bool) {
  crate::remote_cache::scrub_child_environment(command);
  if !diagnostic_wrapper {
    suppress_nested_observation(command);
  }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn lookup_native_action<'a>(
  cas: &'a LocalCas,
  _session: &NativeCompilerSession,
  lookup_key: &str,
  _pre_link_action: &str,
  _capture: &NativeActionCapture,
  _observation: &RawCompilerInvocation,
  _installation_authority: Option<&str>,
) -> RailResult<(crate::cache::cas::NativeActionLookup<'a>, u64)> {
  cas.native_action(lookup_key).map(|lookup| (lookup, 0))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn lookup_native_action<'a>(
  cas: &'a LocalCas,
  session: &NativeCompilerSession,
  lookup_key: &str,
  pre_link_action: &str,
  capture: &NativeActionCapture,
  observation: &RawCompilerInvocation,
  installation_authority: Option<&str>,
) -> RailResult<(crate::cache::cas::NativeActionLookup<'a>, u64)> {
  if !linked_observation(observation) {
    return cas.native_action(lookup_key).map(|lookup| (lookup, 0));
  }
  let candidates = cas.native_link_candidates(lookup_key)?;
  let mut bytes_hashed = 0u64;
  for action_key in candidates {
    let hit = match cas.native_action(&action_key)? {
      crate::cache::cas::NativeActionLookup::Hit(hit) => hit,
      crate::cache::cas::NativeActionLookup::Packed(hit) => {
        return Ok((crate::cache::cas::NativeActionLookup::Packed(hit), bytes_hashed));
      }
      crate::cache::cas::NativeActionLookup::Miss(_) => continue,
    };
    if hit.validation.session_identity != session.identity
      || hit.validation.class != session.class
      || !capture.validates_witness(&hit.validation.witness, observation)
      || witnessed_action_key(pre_link_action, &hit.validation.witness)? != action_key
    {
      continue;
    }
    let (selected_action, hashed) = match revalidate_selected_action(
      observation,
      &hit.validation.witness,
      hit.validation.linker_generations.as_ref(),
      pre_link_action,
      installation_authority,
    ) {
      Ok(selected) => selected,
      Err(_) => continue,
    };
    if selected_action != action_key {
      continue;
    }
    bytes_hashed = bytes_hashed.saturating_add(hashed);
    return Ok((crate::cache::cas::NativeActionLookup::Hit(hit), bytes_hashed));
  }
  Ok((
    crate::cache::cas::NativeActionLookup::Miss(crate::cache::cas::NativeCacheMiss {
      reason: "linked_action_not_found".to_string(),
      bytes_read: 0,
    }),
    bytes_hashed,
  ))
}

enum DirectRemoteReuse {
  Hit,
  Cold(&'static str),
  Operational(RailError),
}

enum PackedLocalReuse {
  Hit,
  Cold(&'static str),
  Reject(&'static str),
  Operational(RailError),
}

fn attempt_local_packed_reuse(
  cas: &LocalCas,
  authority: &crate::cache::cas::PackedNativeActionHit<'_>,
  session: &NativeCompilerSession,
  capture: &NativeActionCapture,
  observation: &RawCompilerInvocation,
  output_paths: &NativeOutputPaths,
  metrics: &mut NativeCacheMetrics,
) -> PackedLocalReuse {
  let context = match active_context() {
    Some(context) => context,
    None => return PackedLocalReuse::Cold("packed_context_unavailable"),
  };
  let output_parent = match validated_output_parent(output_paths, &context.source_root) {
    Ok(parent) => parent,
    Err(_) => return PackedLocalReuse::Cold("packed_output_root_unavailable"),
  };
  let staging = match pack::NativeResultStaging::temporary_in(&output_parent) {
    Ok(staging) => staging,
    Err(_) => return PackedLocalReuse::Cold("packed_output_staging_unavailable"),
  };
  let compressed = match authority.compressed_reader() {
    Ok(reader) => reader,
    Err(_) => return PackedLocalReuse::Reject("packed_entry_unavailable"),
  };
  let (decoded, association) = match pack::decode_zstd_for_action(
    compressed,
    authority.compressed_bytes(),
    authority.action_key(),
    authority.pack_bytes(),
    staging,
  ) {
    Ok(decoded) => decoded,
    Err(_) => return PackedLocalReuse::Reject("packed_entry_rejected"),
  };
  if association.result_key() != authority.result_key() {
    return PackedLocalReuse::Reject("packed_result_identity_mismatch");
  }
  let (validation, handoff, pack_bytes) = match prepare_authenticated_native_handoff(
    decoded,
    session,
    capture,
    observation,
    output_paths,
    &context.source_root,
  ) {
    Ok(prepared) => prepared,
    Err(_) => return PackedLocalReuse::Cold("packed_capability_mismatch"),
  };
  if validation.action_key() != authority.action_key()
    || validation.result_key() != authority.result_key()
    || validation.session_identity != session.identity
    || validation.class != session.class
    || !capture.validates_witness(&validation.witness, observation)
  {
    return PackedLocalReuse::Cold("packed_action_descriptor_incompatible");
  }
  metrics.cache_bytes_read = metrics.cache_bytes_read.saturating_add(pack_bytes);
  match restore_and_publish(
    context,
    cas,
    NativeRestoreSource::Packed {
      authority,
      validation: &validation,
      handoff,
      hit_source: NativeHitSource::Local,
    },
    capture,
    observation,
    output_paths,
    metrics,
  ) {
    Ok(()) => PackedLocalReuse::Hit,
    Err(RestorePublishFailure::BeforeEffect(_)) => PackedLocalReuse::Cold("packed_result_materialization_failed"),
    Err(RestorePublishFailure::AfterEffect(error) | RestorePublishFailure::Operational(error)) => {
      PackedLocalReuse::Operational(error)
    }
  }
}

#[allow(clippy::too_many_arguments)]
fn attempt_direct_remote_reuse(
  cas: &LocalCas,
  remote: &crate::remote_cache::RemoteStore,
  selection: &crate::remote_cache::RemoteCacheSelection,
  session: &NativeCompilerSession,
  pre_link_action: &str,
  base_action_key: &str,
  environment_names: &[String],
  remote_entry: Option<crate::remote_cache::RemoteLookup>,
  capture: &NativeActionCapture,
  observation: &RawCompilerInvocation,
  output_paths: &NativeOutputPaths,
  metrics: &mut NativeCacheMetrics,
) -> DirectRemoteReuse {
  let lookup = match remote_entry {
    Some(entry) => Ok(entry),
    None => {
      let started = Instant::now();
      let lookup = remote.lookup(base_action_key);
      metrics.remote_timing.lookup.record(started);
      lookup
    }
  };
  let linked = linked_observation(observation);
  let (selected_action, result_key, mut body, bytes, compressed_bytes) = match lookup {
    Ok(crate::remote_cache::RemoteLookup::Miss) => return DirectRemoteReuse::Cold("remote_entry_not_found"),
    Ok(crate::remote_cache::RemoteLookup::Conflict) => return DirectRemoteReuse::Cold("remote_entry_conflicted"),
    Ok(crate::remote_cache::RemoteLookup::Unique {
      environment_names: remote_environment_names,
      action_key: remote_action_key,
      result_key,
      body,
      bytes,
      compressed_bytes,
    }) => {
      if remote_environment_names != environment_names
        || (!linked && remote_action_key != pre_link_action)
        || validate_action_key(&remote_action_key).is_err()
      {
        return DirectRemoteReuse::Cold("remote_entry_identity_mismatch");
      }
      (remote_action_key, result_key, body, bytes, compressed_bytes)
    }
    Err(error) => return DirectRemoteReuse::Cold(error.cold_reason()),
  };
  let context = match active_context() {
    Some(context) => context,
    None => return DirectRemoteReuse::Cold("remote_context_unavailable"),
  };
  let output_parent = match validated_output_parent(output_paths, &context.source_root) {
    Ok(parent) => parent,
    Err(_) => return DirectRemoteReuse::Cold("remote_output_root_unavailable"),
  };
  let mut packed = match cas.packed_native_action_staging(
    base_action_key,
    environment_names,
    &selected_action,
    &result_key,
    selection.authority(),
    bytes,
    compressed_bytes,
  ) {
    Ok(staging) => staging,
    Err(_) => return DirectRemoteReuse::Cold("remote_pack_staging_unavailable"),
  };
  let started = Instant::now();
  let copied = match body.copy_compressed_to(packed.writer()) {
    Ok(copied) => copied,
    Err(_) => return DirectRemoteReuse::Cold("remote_compressed_entry_stream_failed"),
  };
  if copied != compressed_bytes || packed.finish_payload().is_err() {
    return DirectRemoteReuse::Cold("remote_compressed_entry_length_mismatch");
  }
  let output_staging = match pack::NativeResultStaging::temporary_in(&output_parent) {
    Ok(staging) => staging,
    Err(_) => return DirectRemoteReuse::Cold("remote_output_staging_unavailable"),
  };
  let compressed = match packed.compressed_reader() {
    Ok(reader) => reader,
    Err(_) => return DirectRemoteReuse::Cold("remote_compressed_entry_staging_failed"),
  };
  let (decoded, association) =
    match pack::decode_zstd_for_action(compressed, compressed_bytes, &selected_action, bytes, output_staging) {
      Ok(decoded) => decoded,
      Err(_) => return DirectRemoteReuse::Cold("remote_entry_rejected"),
    };
  metrics.remote_timing.decode.record(started);
  if association.result_key() != result_key {
    return DirectRemoteReuse::Cold("remote_action_result_mismatch");
  }
  let started = Instant::now();
  let (validation, handoff, pack_bytes) = match prepare_authenticated_native_handoff(
    decoded,
    session,
    capture,
    observation,
    output_paths,
    &context.source_root,
  ) {
    Ok(prepared) => prepared,
    Err(_) => return DirectRemoteReuse::Cold("remote_pack_capability_mismatch"),
  };
  metrics.remote_timing.validation.record(started);
  metrics.cache_bytes_read = metrics.cache_bytes_read.saturating_add(pack_bytes);
  let started = Instant::now();
  let mut recapture_bytes = 0_u64;
  if cas
    .commit_packed_native_action_revalidated(packed, &validation, |validation| {
      if validation.action_key() != selected_action
        || !validation.remote_environment_is_approved(selection.approved_environment_names())
      {
        return Err(RailError::message("remote result changed before local admission"));
      }
      recapture_bytes =
        capture.revalidate_before_restore_commit(observation, &context.source_root, &context.source_root_spelling)?;
      Ok(())
    })
    .is_err()
  {
    return DirectRemoteReuse::Cold("remote_pack_admission_failed");
  }
  metrics.remote_timing.l1_admission.record(started);
  metrics.bytes_hashed = metrics.bytes_hashed.saturating_add(recapture_bytes);
  if linked {
    let candidate = match link_candidate_selector(pre_link_action) {
      Ok(candidate) => candidate,
      Err(_) => return DirectRemoteReuse::Cold("remote_link_candidate_unavailable"),
    };
    if cas.publish_native_link_candidate(&candidate, &selected_action).is_err() {
      return DirectRemoteReuse::Cold("remote_link_candidate_admission_failed");
    }
  }
  let restored = match cas.native_action(&selected_action) {
    Ok(crate::cache::cas::NativeActionLookup::Packed(authority)) => {
      metrics.cache_bytes_read = metrics.cache_bytes_read.saturating_add(authority.bytes_read);
      restore_and_publish(
        context,
        cas,
        NativeRestoreSource::Packed {
          authority: &authority,
          validation: &validation,
          handoff,
          hit_source: NativeHitSource::Remote { base_action_key },
        },
        capture,
        observation,
        output_paths,
        metrics,
      )
    }
    Ok(crate::cache::cas::NativeActionLookup::Hit(cached)) if cached.validation == validation => restore_and_publish(
      context,
      cas,
      NativeRestoreSource::Materialized {
        cached: &cached,
        hit_source: NativeHitSource::Remote { base_action_key },
      },
      capture,
      observation,
      output_paths,
      metrics,
    ),
    Ok(crate::cache::cas::NativeActionLookup::Hit(_) | crate::cache::cas::NativeActionLookup::Miss(_)) | Err(_) => {
      return DirectRemoteReuse::Cold("remote_pack_authority_changed");
    }
  };
  match restored {
    Ok(()) => DirectRemoteReuse::Hit,
    Err(RestorePublishFailure::BeforeEffect(_)) => DirectRemoteReuse::Cold("remote_result_materialization_failed"),
    Err(RestorePublishFailure::AfterEffect(error) | RestorePublishFailure::Operational(error)) => {
      DirectRemoteReuse::Operational(error)
    }
  }
}

/// Bind one privately staged worker result to the current native action, admit
/// it through L1, and publish it through the existing restore transaction.
///
/// Any failure before restore publication returns a cold decision; restore
/// failures after its effect boundary fail closed. A configured L2 receives
/// the result only after the local restore transaction commits successfully.
#[allow(clippy::too_many_arguments)]
pub(crate) fn admit_distributed_rust_library_result(
  context: &NativeCacheContext,
  cas: &LocalCas,
  session: &NativeCompilerSession,
  initial_capture: &NativeActionCapture,
  expected_base_action: &str,
  current_observation: &RawCompilerInvocation,
  output_paths: &NativeOutputPaths,
  candidate: &crate::compiler::distributed::RustLibraryCandidate,
  result: crate::compiler::distributed::StagedExecutionResult,
  cache_bytes_read: u64,
  mut timing: crate::compiler::distributed::DistributedTiming,
) -> crate::compiler::distributed::LocalAdmission {
  use crate::compiler::distributed::LocalAdmission;

  let admission_started = Instant::now();
  let live_candidate = match distributed_rust_library_candidate(
    current_observation,
    initial_capture,
    output_paths,
    &context.source_root,
    &context.source_root_spelling,
  ) {
    Ok(candidate) => candidate,
    Err(_) => return LocalAdmission::RejectedBeforeEffect("distributed_action_changed_before_admission"),
  };
  if !result.binds_candidate(candidate) || !result.binds_candidate(&live_candidate) {
    return LocalAdmission::RejectedBeforeEffect("distributed_result_action_mismatch");
  }
  let (prepared, proof) = match prepare_distributed_result(
    session,
    initial_capture,
    expected_base_action,
    current_observation,
    output_paths,
    result,
    &context.source_root,
    &context.source_root_spelling,
  ) {
    Ok(prepared) => prepared,
    Err(reason) => return LocalAdmission::RejectedBeforeEffect(reason),
  };
  let environment_names = Vec::new();
  let mut recapture_bytes = 0_u64;
  let (validation, _) = match cas.store_native_revalidated(prepared, |validation| {
    recapture_bytes = validation
      .revalidate_publication(
        session,
        &context.source_root,
        &proof,
        context
          .installation
          .as_ref()
          .map(crate::cache::installation::InstallationReceipt::authority),
      )
      .map_err(|failure| failure.error)?;
    match cas.publish_native_environment_selector(expected_base_action, &environment_names)? {
      crate::cache::cas::NativeEnvironmentSelectorPublication::Created
      | crate::cache::cas::NativeEnvironmentSelectorPublication::Converged => Ok(()),
      crate::cache::cas::NativeEnvironmentSelectorPublication::Diverged => Err(RailError::message(
        "distributed native environment selector diverged before admission",
      )),
    }
  }) {
    Ok(stored) => stored,
    Err(_) => return LocalAdmission::RejectedBeforeEffect("distributed_local_admission_failed"),
  };
  let cached = match cas.native_action(validation.action_key()) {
    Ok(crate::cache::cas::NativeActionLookup::Hit(cached)) if cached.validation == validation => cached,
    Ok(
      crate::cache::cas::NativeActionLookup::Hit(_)
      | crate::cache::cas::NativeActionLookup::Packed(_)
      | crate::cache::cas::NativeActionLookup::Miss(_),
    )
    | Err(_) => return LocalAdmission::RejectedBeforeEffect("distributed_local_authority_changed"),
  };
  timing.record_admission(admission_started);
  let mut metrics = NativeCacheMetrics {
    bytes_hashed: recapture_bytes,
    cache_bytes_read,
    distributed_timing: Some(timing),
    ..NativeCacheMetrics::default()
  };
  match restore_and_publish(
    context,
    cas,
    NativeRestoreSource::Materialized {
      cached: &cached,
      hit_source: NativeHitSource::Distributed {
        base_action_key: expected_base_action,
      },
    },
    initial_capture,
    current_observation,
    output_paths,
    &mut metrics,
  ) {
    Ok(()) => LocalAdmission::Committed(0),
    Err(RestorePublishFailure::BeforeEffect(_)) => {
      LocalAdmission::RejectedBeforeEffect("distributed_result_materialization_failed")
    }
    Err(RestorePublishFailure::AfterEffect(error) | RestorePublishFailure::Operational(error)) => {
      LocalAdmission::FailedAfterEffect(error)
    }
  }
}

fn suppress_nested_observation(command: &mut Command) {
  remove_private_environment(command);
}

fn prepare_observed_cold_child(
  command: &mut Command,
  rustc: &OsStr,
  compiler_arguments: &[OsString],
  diagnostic_wrapper: bool,
  observation: &RawCompilerInvocation,
  observation_directory: &Path,
) {
  #[cfg(not(any(target_os = "macos", target_os = "linux")))]
  let _ = (observation, observation_directory);
  *command = Command::new(rustc);
  command.args(compiler_arguments);
  if diagnostic_wrapper {
    command.arg("--warn=unused-crate-dependencies");
  }
  suppress_nested_observation(command);
  #[cfg(target_os = "macos")]
  if apple_linked_observation(observation) {
    let adapter = std::env::current_exe().ok();
    let driver = PathBuf::from("/usr/bin/cc");
    let certificate = observation_directory.join(APPLE_LINK_CERTIFICATE_FILE);
    let driver_inputs = observation_directory.join(APPLE_LINK_DRIVER_INPUTS_FILE);
    if adapter.as_ref().is_some_and(|adapter| adapter.is_absolute())
      && fs::metadata(&driver).is_ok_and(|metadata| metadata.is_file() && executable_metadata(&metadata))
      && fs::symlink_metadata(&certificate).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
      && fs::symlink_metadata(&driver_inputs).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
      command
        .arg(format!(
          "-Clinker={}",
          adapter
            .as_ref()
            .map_or_else(String::new, |path| path.display().to_string())
        ))
        .env(APPLE_LINK_ADAPTER_ENV, "1")
        .env(APPLE_LINK_DRIVER_ENV, driver)
        .env(APPLE_LINK_CERTIFICATE_ENV, certificate)
        .env(APPLE_LINK_DRIVER_INPUTS_ENV, driver_inputs);
    }
  }
  #[cfg(target_os = "linux")]
  if elf_linked_observation(observation) {
    let adapter = std::env::current_exe().ok();
    let current_directory = std::env::current_dir().ok();
    let driver = current_directory
      .as_deref()
      .and_then(|current| crate::executable::resolve_executable_path(OsStr::new("cc"), current).ok());
    let linker = driver.as_deref().and_then(|driver| {
      current_directory
        .as_deref()
        .and_then(|current| resolve_selected_elf_linker(driver, current).ok())
    });
    let supported = linker
      .as_deref()
      .is_some_and(|linker| elf_linker_supports_dependency_file(linker).unwrap_or(false));
    let dependencies = observation_directory.join(ELF_LINK_DEPENDENCIES_FILE);
    let driver_inputs = observation_directory.join(ELF_LINK_DRIVER_INPUTS_FILE);
    if let Some(driver) = driver
      && adapter.as_ref().is_some_and(|adapter| adapter.is_absolute())
      && supported
      && fs::symlink_metadata(&dependencies).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
      && fs::symlink_metadata(&driver_inputs).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
      command
        .arg(format!(
          "-Clinker={}",
          adapter
            .as_ref()
            .map_or_else(String::new, |path| path.display().to_string())
        ))
        .env(ELF_LINK_ADAPTER_ENV, "1")
        .env(ELF_LINK_DRIVER_ENV, driver)
        .env(ELF_LINK_DEPENDENCIES_ENV, dependencies)
        .env(ELF_LINK_DRIVER_INPUTS_ENV, driver_inputs);
    }
  }
}

fn apple_linked_observation(observation: &RawCompilerInvocation) -> bool {
  cfg!(target_os = "macos") && native_linked_output_role(observation).is_some()
}

fn elf_linked_observation(observation: &RawCompilerInvocation) -> bool {
  cfg!(target_os = "linux") && native_linked_output_role(observation).is_some()
}

fn native_linked_output_role(observation: &RawCompilerInvocation) -> Option<NativeOutputRole> {
  (observation.emit_modes == BTreeSet::from(["dep-info".to_string(), "link".to_string()]))
    .then(|| NativeOutputRole::from_invocation(&observation.crate_types, observation.test_mode))
    .flatten()
    .filter(|role| role.requires_linker())
}

fn linked_observation(observation: &RawCompilerInvocation) -> bool {
  apple_linked_observation(observation) || elf_linked_observation(observation)
}

fn configure_cold(
  command: &mut Command,
  status: CompilerCacheWrapperStatus,
  reason: &str,
  action_key: Option<String>,
  bytes_hashed: u64,
  propagate_metadata: bool,
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
    );
  }
  metadata
}

fn is_diagnostic_workspace_wrapper(program: &OsStr) -> bool {
  if std::env::var_os(crate::compiler::invocation::WRAPPER_MARKER).is_none() {
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
  cached: &crate::cache::cas::NativeActionHit<'_>,
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

#[derive(Clone, Copy)]
enum NativeHitSource<'a> {
  Local,
  Remote { base_action_key: &'a str },
  Distributed { base_action_key: &'a str },
}

enum NativeRestoreSource<'a> {
  Materialized {
    cached: &'a crate::cache::cas::NativeActionHit<'a>,
    hit_source: NativeHitSource<'a>,
  },
  Packed {
    authority: &'a crate::cache::cas::PackedNativeActionHit<'a>,
    validation: &'a NativeCompilerValidation,
    handoff: pack::NativePackHandoff,
    hit_source: NativeHitSource<'a>,
  },
}

impl<'a> NativeRestoreSource<'a> {
  fn validation(&self) -> &NativeCompilerValidation {
    match self {
      Self::Materialized { cached, .. } => &cached.validation,
      Self::Packed { validation, .. } => validation,
    }
  }

  const fn hit_source(&self) -> NativeHitSource<'a> {
    match self {
      Self::Materialized { hit_source, .. } | Self::Packed { hit_source, .. } => *hit_source,
    }
  }
}

impl NativeHitSource<'_> {
  const fn reason(&self) -> &'static str {
    match self {
      Self::Local => "verified_local_result",
      Self::Remote { .. } => "verified_remote_result",
      Self::Distributed { .. } => "verified_distributed_execution",
    }
  }

  const fn remote_action_key(&self) -> Option<&str> {
    match self {
      Self::Local => None,
      Self::Remote { base_action_key } | Self::Distributed { base_action_key } => Some(base_action_key),
    }
  }

  const fn distributed_base_action_key(&self) -> Option<&str> {
    match self {
      Self::Distributed { base_action_key } => Some(base_action_key),
      Self::Local | Self::Remote { .. } => None,
    }
  }
}

fn restore_and_publish(
  context: &NativeCacheContext,
  cas: &LocalCas,
  source: NativeRestoreSource<'_>,
  initial_capture: &NativeActionCapture,
  current_observation: &RawCompilerInvocation,
  output_paths: &NativeOutputPaths,
  metrics: &mut NativeCacheMetrics,
) -> Result<(), RestorePublishFailure> {
  let restore_started = Instant::now();
  let durability = native_durability_phase(NativeDurabilityPhase::RestoreTransaction);
  match &source {
    NativeRestoreSource::Materialized { cached, .. } => {
      validate_restore_environment_authority(cached, initial_capture, current_observation)?;
    }
    NativeRestoreSource::Packed {
      authority, validation, ..
    } => {
      let base_action = base_action_key(
        &validation.session_identity,
        &validation.class,
        current_observation,
        initial_capture,
      )
      .map_err(RestorePublishFailure::Operational)?;
      if authority.base_action_key() != base_action
        || authority.action_key() != validation.action_key()
        || authority.result_key() != validation.result_key()
        || authority.environment_names() != validation.witness.environment_names
      {
        return Err(RestorePublishFailure::Operational(RailError::message(
          "packed native authority does not match its live validation",
        )));
      }
      authority
        .validate_environment_selector()
        .map_err(RestorePublishFailure::Operational)?;
    }
  }
  let before = RestorePublishFailure::BeforeEffect;
  let validation = source.validation().clone();
  let hit_source = source.hit_source();
  let source_root = &context.source_root;
  let source_root_spelling = &context.source_root_spelling;
  let observation_directory = &context.observation_directory;
  validate_current_output_binding(&validation, output_paths, source_root).map_err(before)?;
  let mut transaction = begin_restore_transaction_in(
    cas,
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
    source,
    &transaction,
    initial_capture,
    current_observation,
    output_paths,
    metrics,
    source_root,
    source_root_spelling,
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
  } = prepared;
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
    crate::compiler::observation::publish_prepared_raw(observation)?;
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
  drop(durability);
  metrics.remote_timing.output_restore.record(restore_started);
  metrics.finish_remote();
  let reason = hit_source
    .distributed_base_action_key()
    .and_then(|base_action_key| publish_direct_remote_result(cas, &validation, base_action_key))
    .map_or_else(
      || hit_source.reason().to_string(),
      |remote| format!("{};{remote}", hit_source.reason()),
    );
  write_cache_event(
    CompilerCacheWrapperStatus::Hit,
    &reason,
    Some(&validation.action_key),
    Some(&validation.result_key),
    hit_source.remote_action_key(),
    NativeCacheMetrics { ..*metrics },
  );
  Ok(())
}

#[allow(clippy::too_many_arguments)]
fn prepare_registered_restore(
  source: NativeRestoreSource<'_>,
  transaction: &NativeRestoreTransaction,
  initial_capture: &NativeActionCapture,
  current_observation: &RawCompilerInvocation,
  output_paths: &NativeOutputPaths,
  metrics: &mut NativeCacheMetrics,
  source_root: &Path,
  source_root_spelling: &Path,
  observation_directory: &Path,
) -> RailResult<PreparedNativeRestore> {
  let validation = source.validation().clone();
  let restored = transaction.paths.transaction_directory.join(RESTORE_VERIFIED_DIRECTORY);
  let staging = transaction
    .paths
    .transaction_directory
    .join(RESTORE_MATERIALIZING_DIRECTORY);
  let hit = match source {
    NativeRestoreSource::Materialized { cached, .. } => match cached.restore_registered(&restored, &staging) {
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
    },
    NativeRestoreSource::Packed { authority, handoff, .. } => {
      let bytes_restored = handoff
        .materialize(&validation, &restored)?
        .ok_or_else(|| RailError::message("authenticated native pack handoff changed before restore"))?;
      authority.refresh_access_if_stale();
      crate::instrumentation::record_cas_restore(bytes_restored);
      crate::cache::cas::NativeCacheHit {
        bytes_read: 0,
        bytes_restored,
      }
    }
  };

  let stdout = read_bounded(&restored.join(STDOUT_SLOT), MAX_STREAM_BYTES)?;
  let stderr = read_bounded(&restored.join(STDERR_SLOT), MAX_STREAM_BYTES)?;
  if digest(&stdout) != validation.stdout_digest || digest(&stderr) != validation.stderr_digest {
    return Err(RailError::message(
      "native compiler cache stream binding changed after restore",
    ));
  }
  let stdout = translate_output_binding_bytes(&stdout, &validation, output_paths, source_root, false)?;
  let stderr = translate_output_binding_bytes(&stderr, &validation, output_paths, source_root, false)?;
  let bindings = native_output_bindings(output_paths);
  let mut prepared_outputs = Vec::with_capacity(bindings.len());
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
        translate_dep_info_output_bindings(&bytes, &validation, output_paths, source_root, initial_capture)?;
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
    prepared_outputs.push(prepared);
  }
  capture_test_pause("before_restore_revalidation", current_observation)?;
  let final_capture =
    initial_capture.revalidate_before_restore_commit(current_observation, source_root, source_root_spelling);
  let final_capture_bytes = final_capture?;
  metrics.bytes_hashed = metrics.bytes_hashed.saturating_add(final_capture_bytes);
  if !initial_capture.validates_witness(&validation.witness, current_observation) {
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
  #[cfg(windows)]
  let opened = crate::windows_fs::open_for_stable_byte_observation(source)?;
  #[cfg(not(windows))]
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
  #[cfg(windows)]
  opened.sync_all()?;
  #[cfg(windows)]
  let opened = {
    drop(opened);
    crate::windows_fs::open_for_stable_byte_observation(&source)?
  };
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
    identity,
    output_parent,
    marker,
    transaction_directory,
    output_sources,
  })
}

fn begin_restore_transaction_in(
  cas: &LocalCas,
  outputs: &NativeOutputPaths,
  source_root: &Path,
  observation_directory: &Path,
  action_key: &str,
) -> RailResult<NativeRestoreTransaction> {
  validate_action_key(action_key)?;
  let paths = restore_commit_paths(outputs, source_root)?;
  let lock = cas.native_restore_lock(&paths.identity)?;
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
    // The record itself is durable. Its source-directory entry is transient:
    // the post-rename source and destination barriers below establish the only
    // name that can authorize visible output replacement.
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

fn recover_restore_commit_in(
  cas: &LocalCas,
  outputs: &NativeOutputPaths,
  source_root: &Path,
  observation_directory: &Path,
) -> RailResult<()> {
  let paths = restore_commit_paths(outputs, source_root)?;
  let _lock = cas.native_restore_lock(&paths.identity)?;
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
  // The private directory is removed immediately. Syncing its intermediate
  // deletions cannot strengthen recovery once the final parent barrier makes
  // the directory removal durable.
  remove_restore_file_if_present(&registration)?;
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
  sync_native_before_commit(&file)?;
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
  let _durability = native_durability_phase(NativeDurabilityPhase::OutputDirectorySync);
  sync_native_before_commit_unmeasured(&File::open(path)?)
}

#[cfg(not(unix))]
fn sync_native_directory(_path: &Path) -> RailResult<()> {
  Ok(())
}

/// Establish native-cache write ordering without draining Apple's entire
/// device queue for every compiler output and transaction record.
///
/// The local CAS uses the same boundary: POSIX `fsync` is sufficient before
/// an atomic rename, while `File::sync_all` maps to the much stronger and far
/// more expensive `F_FULLFSYNC` on Apple platforms.
#[cfg(target_os = "macos")]
fn sync_native_before_commit(file: &File) -> RailResult<()> {
  let _durability = native_durability_phase(NativeDurabilityPhase::OutputFileSync);
  sync_native_before_commit_unmeasured(file)
}

#[cfg(target_os = "macos")]
fn sync_native_before_commit_unmeasured(file: &File) -> RailResult<()> {
  rustix::fs::fsync(file).map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
  Ok(())
}

#[cfg(not(target_os = "macos"))]
fn sync_native_before_commit(file: &File) -> RailResult<()> {
  let _durability = native_durability_phase(NativeDurabilityPhase::OutputFileSync);
  sync_native_before_commit_unmeasured(file)
}

#[cfg(not(target_os = "macos"))]
fn sync_native_before_commit_unmeasured(file: &File) -> RailResult<()> {
  file.sync_all()?;
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
  let output_parent = bindings
    .first()
    .ok_or_else(|| RailError::message("native compiler output inventory is empty"))?
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
  if !canonical_parent.starts_with(canonical_root.join("target"))
    || bindings
      .iter()
      .any(|(role, _, output)| !output_role_path_matches(role, output))
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
  let mut bindings = vec![("dep_info", DEP_INFO_SLOT, outputs.dep_info.as_path())];
  bindings.extend(outputs.artifacts.iter().map(|artifact| {
    let slot = match artifact.role {
      NativeOutputRole::Metadata => METADATA_SLOT,
      NativeOutputRole::Rlib => RLIB_SLOT,
      NativeOutputRole::Executable => EXECUTABLE_SLOT,
      NativeOutputRole::ProcMacro => PROC_MACRO_SLOT,
      NativeOutputRole::Dylib => DYLIB_SLOT,
      NativeOutputRole::Cdylib => CDYLIB_SLOT,
      NativeOutputRole::Staticlib => STATICLIB_SLOT,
    };
    (artifact.role.name(), slot, artifact.path.as_path())
  }));
  bindings
}

fn output_role_path_matches(role: &str, output: &Path) -> bool {
  match role {
    "dep_info" => output.extension() == Some(OsStr::new("d")),
    "metadata" => output.extension() == Some(OsStr::new("rmeta")),
    "rlib" => output.extension() == Some(OsStr::new("rlib")),
    "executable" if cfg!(windows) => output.extension() == Some(OsStr::new("exe")),
    "executable" => output.file_name().is_some() && output.extension().is_none(),
    "proc_macro" | "dylib" | "cdylib" => {
      let extension = if cfg!(windows) {
        "dll"
      } else if cfg!(target_os = "macos") {
        "dylib"
      } else {
        "so"
      };
      output.extension() == Some(OsStr::new(extension))
    }
    "staticlib" => output.extension() == Some(OsStr::new(if cfg!(windows) { "lib" } else { "a" })),
    _ => false,
  }
}

fn validate_current_output_binding(
  validation: &NativeCompilerValidation,
  outputs: &NativeOutputPaths,
  source_root: &Path,
) -> RailResult<()> {
  let stored = validation
    .outputs
    .iter()
    .map(|output| (output.role.as_str(), Some(output.file_name.as_str())))
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
    replacements.extend(dep_info_source_root_replacements(root, token, true)?);
  }
  replacements.sort_unstable_by(|left, right| right.0.len().cmp(&left.0.len()).then_with(|| left.cmp(right)));
  if replacements
    .windows(2)
    .any(|pair| pair[0].0 == pair[1].0 && pair[0].1 != pair[1].1)
  {
    return Err(RailError::message(
      "native compiler dep-info source-root spelling is ambiguous",
    ));
  }
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
  let mut bindings = dep_info_source_root_replacements(source_root, PORTABLE_SOURCE_ROOT, false)?;
  if let Some(package) = &capture.package_binding {
    bindings.extend(dep_info_source_root_replacements(
      &package.spelling,
      PORTABLE_PACKAGE_ROOT,
      false,
    )?);
  }
  // rustc may emit the remapped root itself. Physical dep-info roots use the
  // representation-bearing bindings above; bare compiler tokens retain the
  // platform display spelling used by diagnostics and stream rebinding.
  bindings.push((
    PORTABLE_SOURCE_ROOT.as_bytes().to_vec(),
    escape_dep_info_path(&source_root_display_bytes(source_root)),
  ));
  if let Some(package) = &capture.package_binding {
    bindings.push((
      PORTABLE_PACKAGE_ROOT.as_bytes().to_vec(),
      escape_dep_info_path(&source_root_display_bytes(&package.spelling)),
    ));
  }
  rebind_portable_source_roots(bytes, &bindings)
}

fn dep_info_source_root_replacements(
  root: &Path,
  token: &str,
  to_portable: bool,
) -> RailResult<Vec<(Vec<u8>, Vec<u8>)>> {
  source_root_spellings(root)?;
  let canonical = crate::utils::canonicalize_existing(root)?;
  let mut seen = BTreeSet::new();
  let mut replacements = Vec::new();
  for (scope, path) in [("selected", root), ("canonical", canonical.as_path())] {
    for (form, spelling) in source_root_path_forms(path) {
      // Encoding one physical spelling under two tokens would be ambiguous.
      // Decoding is the inverse: distinct tokens may legitimately converge on
      // the same spelling when the selected root is already canonical.
      if to_portable && !seen.insert(spelling.clone()) {
        continue;
      }
      let escaped = escape_dep_info_path(&spelling);
      let mut representations = vec![("literal", spelling)];
      if escaped != representations[0].1 {
        representations.push(("escaped", escaped));
      }
      for (representation, rendered) in representations {
        let portable = format!("{token}/dep-info/{scope}/{form}/{representation}").into_bytes();
        replacements.push(if to_portable {
          (rendered, portable)
        } else {
          (portable, rendered)
        });
      }
    }
  }
  Ok(replacements)
}

#[cfg(unix)]
fn source_root_path_forms(path: &Path) -> Vec<(&'static str, Vec<u8>)> {
  vec![("native", path.as_os_str().as_encoded_bytes().to_vec())]
}

#[cfg(windows)]
fn source_root_path_forms(path: &Path) -> Vec<(&'static str, Vec<u8>)> {
  let native = path.as_os_str().as_encoded_bytes().to_vec();
  let forward = crate::utils::path_to_git_format(path).into_bytes();
  let backward = forward
    .iter()
    .map(|byte| if *byte == b'/' { b'\\' } else { *byte })
    .collect();
  vec![("native", native), ("forward", forward), ("backward", backward)]
}

#[cfg(not(any(unix, windows)))]
fn source_root_path_forms(path: &Path) -> Vec<(&'static str, Vec<u8>)> {
  vec![("native", path.as_os_str().as_encoded_bytes().to_vec())]
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
#[cfg(any(target_os = "macos", target_os = "linux", debug_assertions, all(test, unix)))]
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

#[cfg(target_os = "macos")]
fn overwrite_private_command_file(path: &Path, bytes: &[u8]) -> RailResult<()> {
  let metadata = fs::symlink_metadata(path)?;
  if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) || !single_link(&metadata) {
    return Err(RailError::message("private command file is not one regular file"));
  }
  let mut file = OpenOptions::new().write(true).open(path)?;
  if !crate::utils::private_file_matches_path(&file, path, metadata.len())? {
    return Err(RailError::message("private command file changed before update"));
  }
  file.set_len(0)?;
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

/// Remove cache-only authority while preserving an explicitly selected inner observation role.
pub(crate) fn remove_cache_environment(command: &mut Command) {
  crate::remote_cache::scrub_child_environment(command);
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
    .env_remove(BENCH_COVERAGE_DIRECTORY_ENV)
    .env_remove(LEGACY_STORE_ENV)
    .env_remove(crate::cache::cas::CACHE_BASE_ENV)
    .env_remove(crate::cache::cas::CACHE_MAX_BYTES_ENV)
    .env_remove(crate::cache::cas::CACHE_TRUST_DOMAIN_ENV)
    .env_remove(crate::compiler::invocation::CACHE_CONTROL_ENV)
    .env_remove(crate::compiler::invocation::CACHE_WRAPPER_MARKER);
}

/// Remove every Cargo-Rail compiler capability before transparent execution.
pub(crate) fn remove_private_environment(command: &mut Command) {
  remove_cache_environment(command);
  command
    .env_remove(crate::compiler::invocation::WRAPPER_MARKER)
    .env_remove(crate::compiler::invocation::INNER_WRAPPER_ENV)
    .env_remove(crate::compiler::invocation::RUSTDOC_WRAPPER_MARKER)
    .env_remove(crate::compiler::invocation::INNER_RUSTDOC_ENV)
    .env_remove(crate::compiler::invocation::OBSERVATION_DIRECTORY_ENV)
    .env_remove(crate::compiler::invocation::OBSERVATION_SOURCE_ROOT_ENV)
    .env_remove(crate::compiler::invocation::OBSERVATION_ONLY_ENV)
    .env_remove(crate::compiler::invocation::FACT_DOCTEST_BUILDER_ENV)
    .env_remove(crate::compiler::invocation::FACT_DOCTEST_RUNNER_ENV)
    .env_remove(crate::compiler::facts::COMPILER_FACT_INVOCATION_ENV)
    .env_remove(crate::compiler::session::FACT_SESSION_ENV)
    .env_remove(APPLE_LINK_ADAPTER_ENV)
    .env_remove(APPLE_LINK_DRIVER_ENV)
    .env_remove(APPLE_LINK_CERTIFICATE_ENV)
    .env_remove(APPLE_LINK_DRIVER_INPUTS_ENV)
    .env_remove(ELF_LINK_ADAPTER_ENV)
    .env_remove(ELF_LINK_DRIVER_ENV)
    .env_remove(ELF_LINK_DEPENDENCIES_ENV)
    .env_remove(ELF_LINK_DRIVER_INPUTS_ENV);
}

/// Add Apple linker evidence arguments without changing the selected driver.
///
/// Unsupported child argv returns `false` so the caller can execute the exact
/// original driver and let the outer wrapper decline publication.
pub(crate) fn configure_apple_link_adapter(command: &mut Command, arguments: &[OsString]) -> bool {
  #[cfg(not(target_os = "macos"))]
  {
    let _ = (command, arguments);
    false
  }
  #[cfg(target_os = "macos")]
  {
    let Some(certificate) = std::env::var_os(APPLE_LINK_CERTIFICATE_ENV).map(PathBuf::from) else {
      return false;
    };
    let Some(driver_inputs) = std::env::var_os(APPLE_LINK_DRIVER_INPUTS_ENV).map(PathBuf::from) else {
      return false;
    };
    if !certificate.is_absolute()
      || certificate.as_os_str().as_encoded_bytes().contains(&0)
      || certificate.as_os_str().as_encoded_bytes().contains(&b',')
      || !driver_inputs.is_absolute()
      || driver_inputs.parent() != certificate.parent()
      || driver_inputs.as_os_str().as_encoded_bytes().contains(&0)
      || arguments.iter().any(|argument| {
        let bytes = argument.as_encoded_bytes();
        bytes
          .windows(b"-dependency_info".len())
          .any(|window| window == b"-dependency_info")
          || bytes
            .windows(b"-oso_prefix".len())
            .any(|window| window == b"-oso_prefix")
      })
    {
      return false;
    }
    let mut temporary_prefix = None::<PathBuf>;
    let mut temporary_directories = BTreeSet::new();
    let mut certified_driver_inputs = BTreeSet::new();
    for argument in arguments {
      let path = Path::new(argument);
      let Some(file_name) = path.file_name().and_then(OsStr::to_str) else {
        continue;
      };
      let extension = path.extension().and_then(OsStr::to_str);
      if !path.is_absolute() || !matches!(extension, Some("o" | "rlib" | "a")) {
        continue;
      }
      let Some(parent) = path.parent() else {
        continue;
      };
      let rustc_temporary = parent.file_name().and_then(OsStr::to_str).is_some_and(|name| {
        name.len() == 11 && name.starts_with("rustc") && name[5..].bytes().all(|byte| byte.is_ascii_alphanumeric())
      });
      let direct_object = extension == Some("o");
      let temporary_archive = rustc_temporary && matches!(extension, Some("rlib" | "a"));
      if file_name.is_empty() || (!direct_object && !temporary_archive) {
        continue;
      }
      let Some(path) = path.to_str() else {
        return false;
      };
      certified_driver_inputs.insert(path.to_string());
      if rustc_temporary {
        temporary_directories.insert(parent.to_path_buf());
      }
      if temporary_archive {
        if temporary_prefix.as_ref().is_some_and(|selected| selected != parent) {
          return false;
        }
        temporary_prefix = Some(parent.to_path_buf());
      }
    }
    let mut preexisting_paths = BTreeSet::new();
    for directory in &temporary_directories {
      let Ok(metadata) = fs::symlink_metadata(directory) else {
        return false;
      };
      #[cfg(unix)]
      {
        use std::os::unix::fs::PermissionsExt as _;

        if !metadata.is_dir()
          || crate::utils::is_symlink_or_reparse(&metadata)
          || metadata.permissions().mode() & 0o022 != 0
        {
          return false;
        }
      }
      let Ok(entries) = fs::read_dir(directory) else {
        return false;
      };
      for entry in entries {
        let Ok(entry) = entry else {
          return false;
        };
        let path = entry.path();
        let Some(path) = path.to_str() else {
          return false;
        };
        preexisting_paths.insert(path.to_string());
        if preexisting_paths.len() > MAX_LINK_INPUTS {
          return false;
        }
      }
    }
    let mut certified_temporary_directories = Vec::with_capacity(temporary_directories.len());
    for path in temporary_directories {
      let Ok(path) = path.into_os_string().into_string() else {
        return false;
      };
      certified_temporary_directories.push(path);
    }
    let evidence = AppleLinkDriverEvidence {
      version: APPLE_LINK_DRIVER_EVIDENCE_VERSION,
      direct_inputs: certified_driver_inputs.into_iter().collect(),
      temporary_directories: certified_temporary_directories,
      preexisting_paths: preexisting_paths.into_iter().collect(),
      generated_inputs: Vec::new(),
    };
    let Ok(evidence) = serde_json::to_vec(&evidence) else {
      return false;
    };
    if write_private_command_file(&driver_inputs, &evidence).is_err() {
      return false;
    }
    command
      .args(arguments)
      .arg(format!("-Wl,-dependency_info,{}", certificate.display()));
    if let Some(prefix) = temporary_prefix {
      command.arg(format!("-Wl,-oso_prefix,{}/", prefix.display()));
    }
    true
  }
}

/// Add a GNU-compatible linker dependency-file argument and persist the exact
/// driver resolution namespace used by this link. Unsupported drivers execute
/// unchanged and the outer wrapper declines publication.
pub(crate) fn configure_elf_link_adapter(command: &mut Command, arguments: &[OsString]) -> bool {
  #[cfg(not(target_os = "linux"))]
  {
    let _ = (command, arguments);
    false
  }
  #[cfg(target_os = "linux")]
  {
    let Some(dependencies) = std::env::var_os(ELF_LINK_DEPENDENCIES_ENV).map(PathBuf::from) else {
      return false;
    };
    let Some(driver_inputs) = std::env::var_os(ELF_LINK_DRIVER_INPUTS_ENV).map(PathBuf::from) else {
      return false;
    };
    let Some(driver) = std::env::var_os(ELF_LINK_DRIVER_ENV).map(PathBuf::from) else {
      return false;
    };
    if !dependencies.is_absolute()
      || dependencies.as_os_str().as_encoded_bytes().contains(&0)
      || dependencies.as_os_str().as_encoded_bytes().contains(&b',')
      || !driver_inputs.is_absolute()
      || driver_inputs.parent() != dependencies.parent()
      || arguments
        .iter()
        .any(|argument| argument.as_encoded_bytes().starts_with(b"@"))
      || !fs::symlink_metadata(&dependencies).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
      || !fs::symlink_metadata(&driver_inputs).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
      return false;
    }
    let Ok(evidence) = capture_elf_link_driver_evidence(&driver, arguments) else {
      return false;
    };
    let Ok(bytes) = serde_json::to_vec(&evidence) else {
      return false;
    };
    if write_private_command_file(&driver_inputs, &bytes).is_err() {
      return false;
    }
    command
      .args(arguments)
      .arg(format!("-Wl,--dependency-file={}", dependencies.display()));
    true
  }
}

#[cfg(target_os = "linux")]
fn capture_elf_link_driver_evidence(driver: &Path, arguments: &[OsString]) -> RailResult<ElfLinkDriverEvidence> {
  let current_directory = crate::utils::canonicalize_existing(&std::env::current_dir()?)?;
  let driver = crate::utils::canonicalize_existing(driver)?;
  let linker = resolve_selected_elf_linker(&driver, &current_directory)?;
  if !elf_linker_supports_dependency_file(&linker)? {
    return Err(RailError::message(
      "selected ELF linker does not support dependency-file evidence",
    ));
  }

  let mut direct_inputs = BTreeSet::new();
  let mut search_directories = BTreeSet::new();
  let mut index = 0usize;
  while index < arguments.len() {
    let argument = &arguments[index];
    let path = Path::new(argument);
    let absolute = if path.is_absolute() {
      path.to_path_buf()
    } else {
      current_directory.join(path)
    };
    if fs::metadata(&absolute).is_ok_and(|metadata| metadata.is_file()) {
      direct_inputs.insert(crate::utils::canonicalize_existing(&absolute)?);
    }
    let mut library = None::<&OsStr>;
    if argument == "-L" {
      library = arguments.get(index + 1).map(OsString::as_os_str);
      index = index.saturating_add(1);
    } else if let Some(value) = argument.to_str().and_then(|value| value.strip_prefix("-L"))
      && !value.is_empty()
    {
      library = Some(OsStr::new(value));
    } else if let Some(value) = argument.to_str().and_then(|value| value.strip_prefix("-Wl,-L,")) {
      library = Some(OsStr::new(value));
    } else if let Some(value) = argument.to_str().and_then(|value| value.strip_prefix("-Wl,-L")) {
      library = Some(OsStr::new(value));
    }
    if let Some(library) = library {
      let library = Path::new(library);
      let absolute = if library.is_absolute() {
        library.to_path_buf()
      } else {
        current_directory.join(library)
      };
      if fs::metadata(&absolute).is_ok_and(|metadata| metadata.is_dir()) {
        search_directories.insert(crate::utils::canonicalize_existing(&absolute)?);
      }
    }
    index = index.saturating_add(1);
  }

  let sysroot = elf_driver_stdout(&driver, &["-print-sysroot"], &current_directory)?;
  let sysroot = PathBuf::from(sysroot.trim());
  let driver_search = elf_driver_stdout(&driver, &["-print-search-dirs"], &current_directory)?;
  if let Some(libraries) = driver_search.lines().find_map(|line| {
    line
      .strip_prefix("libraries: =")
      .or_else(|| line.strip_prefix("libraries: "))
  }) {
    for directory in std::env::split_paths(OsStr::new(libraries)) {
      let directory = resolve_elf_search_directory(&directory, &sysroot, &current_directory);
      if fs::metadata(&directory).is_ok_and(|metadata| metadata.is_dir()) {
        search_directories.insert(crate::utils::canonicalize_existing(&directory)?);
      }
    }
  }
  let linker_verbose = elf_driver_stdout(&linker, &["--verbose"], &current_directory)?;
  for directory in elf_linker_script_search_directories(&linker_verbose) {
    let directory = resolve_elf_search_directory(Path::new(&directory), &sysroot, &current_directory);
    if fs::metadata(&directory).is_ok_and(|metadata| metadata.is_dir()) {
      search_directories.insert(crate::utils::canonicalize_existing(&directory)?);
    }
  }

  let mut tool_inputs = BTreeSet::new();
  for tool in ["collect2", "lto-wrapper"] {
    let selected = elf_driver_stdout(&driver, &[&format!("-print-prog-name={tool}")], &current_directory)?;
    let selected = selected.trim();
    if selected.is_empty() || selected == tool {
      continue;
    }
    let selected = crate::executable::resolve_executable_path(OsStr::new(selected), &current_directory)?;
    if selected != driver && selected != linker {
      tool_inputs.insert(selected);
    }
  }
  let to_strings = |paths: BTreeSet<PathBuf>| -> RailResult<Vec<String>> {
    paths
      .into_iter()
      .map(|path| {
        path
          .into_os_string()
          .into_string()
          .map_err(|_| RailError::message("ELF linker evidence path is not valid UTF-8"))
      })
      .collect()
  };
  let evidence = ElfLinkDriverEvidence {
    version: 1,
    current_directory: current_directory
      .into_os_string()
      .into_string()
      .map_err(|_| RailError::message("ELF linker current directory is not valid UTF-8"))?,
    driver: driver
      .into_os_string()
      .into_string()
      .map_err(|_| RailError::message("ELF linker driver path is not valid UTF-8"))?,
    linker: linker
      .into_os_string()
      .into_string()
      .map_err(|_| RailError::message("ELF linker path is not valid UTF-8"))?,
    tool_inputs: to_strings(tool_inputs)?,
    search_directories: to_strings(search_directories)?,
    direct_inputs: to_strings(direct_inputs)?,
  };
  if evidence.direct_inputs.is_empty() {
    return Err(RailError::message("ELF linker driver exposed no direct file inputs"));
  }
  Ok(evidence)
}

#[cfg(target_os = "linux")]
fn resolve_selected_elf_linker(driver: &Path, current_directory: &Path) -> RailResult<PathBuf> {
  let selected = elf_driver_stdout(driver, &["-print-prog-name=ld"], current_directory)?;
  let selected = selected.trim();
  if selected.is_empty() {
    return Err(RailError::message("ELF linker driver returned no selected linker"));
  }
  crate::executable::resolve_executable_path(OsStr::new(selected), current_directory)
}

#[cfg(target_os = "linux")]
fn elf_linker_supports_dependency_file(linker: &Path) -> RailResult<bool> {
  Ok(elf_driver_stdout(linker, &["--help"], &std::env::current_dir()?)?.contains("--dependency-file"))
}

#[cfg(target_os = "linux")]
fn elf_driver_stdout(program: &Path, arguments: &[&str], current_directory: &Path) -> RailResult<String> {
  let output = Command::new(program)
    .args(arguments)
    .current_dir(current_directory)
    .output()?;
  if !output.status.success() || output.stdout.len() > MAX_ELF_LINK_DEPENDENCY_BYTES as usize {
    return Err(RailError::message("ELF linker capability probe failed"));
  }
  String::from_utf8(output.stdout).map_err(|_| RailError::message("ELF linker capability probe was not UTF-8"))
}

#[cfg(target_os = "linux")]
fn resolve_elf_search_directory(path: &Path, sysroot: &Path, current_directory: &Path) -> PathBuf {
  if let Some(path) = path.to_str().and_then(|path| path.strip_prefix('=')) {
    sysroot.join(path)
  } else if path.is_absolute() {
    path.to_path_buf()
  } else {
    current_directory.join(path)
  }
}

#[cfg(target_os = "linux")]
fn elf_linker_script_search_directories(verbose: &str) -> Vec<String> {
  verbose
    .split("SEARCH_DIR(")
    .skip(1)
    .filter_map(|tail| tail.split_once(')').map(|(value, _)| value))
    .map(|value| value.trim().trim_matches('"').to_string())
    .filter(|value| !value.is_empty() && !value.as_bytes().contains(&0))
    .collect()
}

/// Certify linker-generated LTO objects while the selected driver still owns
/// its private temporary namespace. Failure leaves the initial evidence file
/// intact, causing the outer wrapper to bypass publication.
pub(crate) fn finalize_apple_link_adapter() -> bool {
  #[cfg(not(target_os = "macos"))]
  {
    false
  }
  #[cfg(target_os = "macos")]
  {
    let Some(certificate) = std::env::var_os(APPLE_LINK_CERTIFICATE_ENV).map(PathBuf::from) else {
      return false;
    };
    let Some(driver_inputs) = std::env::var_os(APPLE_LINK_DRIVER_INPUTS_ENV).map(PathBuf::from) else {
      return false;
    };
    let Ok(mut evidence) = read_apple_link_driver_evidence(&driver_inputs) else {
      return false;
    };
    let Ok(bytes) = read_bounded(&certificate, MAX_APPLE_LINK_CERTIFICATE_BYTES as usize) else {
      return false;
    };
    let Ok((_, entries)) = parse_apple_link_certificate(&bytes) else {
      return false;
    };
    let direct = evidence
      .direct_inputs
      .iter()
      .map(PathBuf::from)
      .collect::<BTreeSet<_>>();
    let temporary_directories = evidence
      .temporary_directories
      .iter()
      .map(PathBuf::from)
      .collect::<BTreeSet<_>>();
    let preexisting = evidence
      .preexisting_paths
      .iter()
      .map(PathBuf::from)
      .collect::<BTreeSet<_>>();
    let mut generated = BTreeSet::new();
    for (opcode, value) in entries {
      if opcode != 0x10 {
        continue;
      }
      let path = PathBuf::from(value);
      if path.extension() != Some(OsStr::new("o")) || direct.contains(&path) {
        continue;
      }
      if path
        .parent()
        .is_none_or(|parent| !temporary_directories.contains(parent))
        || preexisting.contains(&path)
      {
        continue;
      }
      let Ok(metadata) = fs::symlink_metadata(&path) else {
        return false;
      };
      if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) || !single_link(&metadata) {
        return false;
      }
      let Some(path) = path.into_os_string().into_string().ok() else {
        return false;
      };
      generated.insert(path);
      if generated.len() > MAX_LINK_INPUTS {
        return false;
      }
    }
    evidence.generated_inputs = generated.into_iter().collect();
    let Ok(bytes) = serde_json::to_vec(&evidence) else {
      return false;
    };
    overwrite_private_command_file(&driver_inputs, &bytes).is_ok()
  }
}

/// Execute one eligible cold invocation, replay its exact streams, and publish
/// only a complete successful observation.
fn publish_direct_remote_result(
  cas: &LocalCas,
  validation: &NativeCompilerValidation,
  base_action_key: &str,
) -> Option<&'static str> {
  let selection = active_remote_selection()?;
  if !selection.direct_transport_supported() {
    return Some("remote_transport_not_qualified");
  }
  if selection.mode() != crate::remote_cache::RemoteCacheMode::ReadWrite {
    return Some("remote_read_only");
  }
  let remote = match open_active_remote_store() {
    Ok(Some(remote)) => remote,
    Ok(None) | Err(_) => return Some("remote_publication_unavailable"),
  };
  let cached = match cas.native_action(validation.action_key()) {
    Ok(crate::cache::cas::NativeActionLookup::Hit(cached)) => cached,
    Ok(crate::cache::cas::NativeActionLookup::Packed(_) | crate::cache::cas::NativeActionLookup::Miss(_)) | Err(_) => {
      return Some("remote_publication_local_result_unavailable");
    }
  };
  let environment_names = match cached.validate_remote_publication(base_action_key) {
    Ok(names) if selection.approves_environment_names(names) => names.to_vec(),
    Ok(_) | Err(_) => return Some("remote_environment_not_shareable"),
  };
  let association = match cached.association() {
    Ok(association) => association,
    Err(_) => return Some("remote_publication_association_failed"),
  };
  let mut pack = match tempfile::tempfile() {
    Ok(pack) => pack,
    Err(_) => return Some("remote_publication_staging_failed"),
  };
  match cached.export_pack(&mut pack) {
    Ok(exported)
      if exported.content_length == association.pack_length()
        && exported.bytes_written == association.pack_length() => {}
    Ok(_) | Err(_) => return Some("remote_publication_export_failed"),
  }
  drop(cached);
  if pack.metadata().is_err() || pack.rewind().is_err() {
    return Some("remote_publication_staging_failed");
  }
  match remote.publish(&association, base_action_key, &environment_names, pack) {
    Ok(crate::remote_cache::RemotePublication::Unique) => Some("remote_published"),
    Ok(crate::remote_cache::RemotePublication::Conflict) => Some("remote_entry_conflicted"),
    Err(_) => Some("remote_publication_failed"),
  }
}

pub(crate) fn run_and_store(
  command: Command,
  recorder: InvocationRecorder,
  mut capture: NativeActionCapture,
  base_action_key: String,
  cache_bytes_read: u64,
  distributed_placement: Option<crate::compiler::distributed::PlacementObservation>,
  context: &str,
) -> i32 {
  let Some(cache_context) = active_context() else {
    eprintln!("{context}: native compiler cache context disappeared before execution");
    return 2;
  };
  let source_root = &cache_context.source_root;
  let source_root_spelling = &cache_context.source_root_spelling;
  let output_paths = recorder.native_output_paths();
  let compiler_started = Instant::now();
  let output = match run_compiler_with_live_streams(command) {
    Ok(output) => output,
    Err(error) => {
      eprintln!("{context}: failed to execute compiler: {error}");
      return 1;
    }
  };
  let compiler_elapsed = compiler_started.elapsed();
  let CapturedCompilerOutput { status, stdout, stderr } = output;

  if status.success()
    && let (Some(receipt), Some(placement)) = (cache_context.installation.as_ref(), distributed_placement.as_ref())
  {
    crate::compiler::distributed::record_local_placement(receipt, placement, compiler_elapsed);
  }

  let capture_pause_failed =
    status.success() && capture_test_pause("after_compiler_execution", recorder.observation()).is_err();
  let mut raw = match recorder.complete(status.success()) {
    Ok(raw) => raw,
    Err(_) => return status.code().unwrap_or(1),
  };
  if !status.success() {
    let _ = publish_and_record_cold_observation(&mut raw, "compiler_execution_failed", None, None, 0, cache_bytes_read);
    return status.code().unwrap_or(1);
  }
  if capture_pause_failed {
    let _ = publish_and_record_cold_observation(&mut raw, "capture_test_pause_failed", None, None, 0, cache_bytes_read);
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
    );
    return status.code().unwrap_or(1);
  }
  let session = cache_context.session.load(source_root);
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
      );
      return status.code().unwrap_or(1);
    }
  };
  if let Some(reason) = invocation_bypass_reason(&raw, true, &session.class.host_target) {
    let bytes_hashed = cold_input_bytes(&raw, source_root, 0);
    let _ = publish_and_record_cold_observation(&mut raw, reason, None, None, bytes_hashed, cache_bytes_read);
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
      );
      return status.code().unwrap_or(1);
    }
  };
  capture.approved_environment = approved_environment;
  let pre_link_action = match action_key(&session.identity, &session.class, &raw, &capture) {
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
      );
      return status.code().unwrap_or(1);
    }
  };
  let mut witness = match capture.witness(&raw, source_root) {
    Ok(witness) => witness,
    Err(_) => {
      let bytes_hashed = cold_input_bytes(&raw, source_root, selected_environment_bytes);
      let _ = publish_and_record_cold_observation(
        &mut raw,
        "compiler_observation_outside_captured_action",
        Some(pre_link_action.clone()),
        None,
        bytes_hashed,
        cache_bytes_read,
      );
      return status.code().unwrap_or(1);
    }
  };
  let certificate = cache_context.observation_directory.join(if cfg!(target_os = "linux") {
    ELF_LINK_DEPENDENCIES_FILE
  } else {
    APPLE_LINK_CERTIFICATE_FILE
  });
  let driver_inputs = cache_context.observation_directory.join(if cfg!(target_os = "linux") {
    ELF_LINK_DRIVER_INPUTS_FILE
  } else {
    APPLE_LINK_DRIVER_INPUTS_FILE
  });
  let (link_candidate, linker_generations, link_witness_bytes) = match complete_linked_witness(
    &raw,
    &output_paths,
    &certificate,
    &driver_inputs,
    &pre_link_action,
    &mut witness,
    cache_context
      .installation
      .as_ref()
      .map(crate::cache::installation::InstallationReceipt::authority),
  ) {
    Ok(completed) => completed,
    Err(error) => {
      if BENCH_COVERAGE_DIRECTORY.get().is_some() {
        eprintln!("cargo-rail native coverage: linker witness unavailable: {error}");
      }
      let bytes_hashed = cold_input_bytes(&raw, source_root, selected_environment_bytes);
      let _ = publish_and_record_cold_observation(
        &mut raw,
        if cfg!(target_os = "linux") {
          "elf_linker_witness_unavailable"
        } else {
          "apple_linker_witness_unavailable"
        },
        Some(pre_link_action),
        None,
        bytes_hashed,
        cache_bytes_read,
      );
      return status.code().unwrap_or(1);
    }
  };
  let selected_action = match link_candidate.as_ref() {
    Some(_) => witnessed_action_key(&pre_link_action, &witness),
    None => Ok(pre_link_action),
  };
  let selected_action = match selected_action {
    Ok(action) => action,
    Err(_) => {
      let bytes_hashed = cold_input_bytes(
        &raw,
        source_root,
        selected_environment_bytes.saturating_add(link_witness_bytes),
      );
      let _ = publish_and_record_cold_observation(
        &mut raw,
        "compiler_selected_action_unavailable",
        link_candidate,
        None,
        bytes_hashed,
        cache_bytes_read,
      );
      return status.code().unwrap_or(1);
    }
  };
  let stdout = match stdout.into_bytes() {
    Some(bytes) => bytes,
    None => {
      let _ =
        publish_and_record_cold_observation(&mut raw, "compiler_stdout_unavailable", None, None, 0, cache_bytes_read);
      return status.code().unwrap_or(1);
    }
  };
  let stderr = match stderr.into_bytes() {
    Some(bytes) => bytes,
    None => {
      let _ =
        publish_and_record_cold_observation(&mut raw, "compiler_stderr_unavailable", None, None, 0, cache_bytes_read);
      return status.code().unwrap_or(1);
    }
  };
  let cas = open_active_local_cas();
  let prepared = match &cas {
    Ok(cas) => {
      let staging = cas.native_result_staging().ok();
      staging.ok_or("local_cache_staging_failed").and_then(|staging| {
        prepare_cold_result(
          &session,
          &capture,
          &base_action_key,
          SelectedNativeAction {
            action_key: selected_action,
            witness,
            linker_generations,
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
      })
    }
    Err(_) => Err("local_cache_open_failed"),
  };
  let initial = raw.cache_wrapper.clone().or_else(metadata_from_environment);
  let base_reason = initial
    .as_ref()
    .map(CompilerCacheWrapperMetadata::reason)
    .unwrap_or("exact_action_not_found")
    .to_string();
  let publication = prepared.and_then(|(prepared, proof)| {
    let mut final_capture_bytes = 0;
    let mut admission_failure = "local_cache_store_failed";
    (|| {
      let cas = cas.as_ref().map_err(|_| "local_cache_open_failed")?;
      let (validation, _stats) = cas
        .store_native_revalidated(prepared, |validation| {
          final_capture_bytes = validation
            .revalidate_publication(
              &session,
              source_root,
              &proof,
              cache_context
                .installation
                .as_ref()
                .map(crate::cache::installation::InstallationReceipt::authority),
            )
            .map_err(|failure| {
              admission_failure = failure.reason;
              failure.error
            })?;
          match cas.publish_native_environment_selector(&base_action_key, &environment_names) {
            Ok(crate::cache::cas::NativeEnvironmentSelectorPublication::Created)
            | Ok(crate::cache::cas::NativeEnvironmentSelectorPublication::Converged) => Ok(()),
            Ok(crate::cache::cas::NativeEnvironmentSelectorPublication::Diverged) => {
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
      if let Some(candidate) = link_candidate.as_deref() {
        admission_failure = "link_candidate_publication_failed";
        cas
          .publish_native_link_candidate(candidate, validation.action_key())
          .map_err(|_| admission_failure)?;
      }
      Ok((validation, final_capture_bytes))
    })()
  });
  match publication {
    Ok((validation, final_capture_bytes)) => {
      let remote_reason = cas
        .as_ref()
        .ok()
        .and_then(|cas| publish_direct_remote_result(cas, &validation, &base_action_key));
      let stored_reason = remote_reason.map_or_else(
        || format!("{base_reason};stored_verified_result"),
        |remote| format!("{base_reason};stored_verified_result;{remote}"),
      );
      let bytes_hashed = cold_input_bytes(
        &raw,
        source_root,
        selected_environment_bytes
          .saturating_add(link_witness_bytes)
          .saturating_add(final_capture_bytes),
      );
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
        None,
        NativeCacheMetrics {
          bytes_hashed,
          cache_bytes_read,
          ..NativeCacheMetrics::default()
        },
      );
    }
    Err(failure_reason) => {
      let reason = initial.as_ref().map(CompilerCacheWrapperMetadata::reason).map_or_else(
        || failure_reason.to_string(),
        |reason| format!("{reason};{failure_reason}"),
      );
      let bytes_hashed = cold_input_bytes(
        &raw,
        source_root,
        selected_environment_bytes.saturating_add(link_witness_bytes),
      );
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
fn run_compiler_with_live_streams(mut command: Command) -> std::io::Result<CapturedCompilerOutput> {
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
  let stdout_worker = match std::thread::Builder::new()
    .name("cargo-rail-rustc-stdout".to_string())
    .spawn(move || capture_compiler_stream(stdout, std::io::stdout(), MAX_STREAM_BYTES))
  {
    Ok(worker) => worker,
    Err(error) => {
      let _ = child.kill();
      let _ = child.wait();
      return Err(error);
    }
  };
  let stderr = capture_compiler_stream(stderr, std::io::stderr(), MAX_STREAM_BYTES);
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
      ..NativeCacheMetrics::default()
    },
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
  linker_generations: Option<LinkerGenerationWitness>,
}

struct CapturedCompilerStreams<'a> {
  stdout: &'a [u8],
  stderr: &'a [u8],
}

#[allow(clippy::too_many_arguments)]
fn prepare_distributed_result(
  session: &NativeCompilerSession,
  initial_capture: &NativeActionCapture,
  expected_base_action: &str,
  current_observation: &RawCompilerInvocation,
  output_paths: &NativeOutputPaths,
  mut result: crate::compiler::distributed::StagedExecutionResult,
  source_root: &Path,
  source_root_spelling: &Path,
) -> Result<(PreparedNativeResult, NativePublicationProof), &'static str> {
  use crate::compiler::distributed::DistributedResultSlot;

  let durable_handoff = result.requires_durable_handoff();
  let prepared: RailResult<_> = (|| {
    validated_output_parent(output_paths, source_root)?;
    let bindings = native_output_bindings(output_paths);
    let roles = bindings.iter().map(|(role, _, _)| *role).collect::<Vec<_>>();
    if roles != ["dep_info", "metadata"] && roles != ["dep_info", "metadata", "rlib"] {
      return Err(RailError::message(
        "distributed result does not match the native output contract",
      ));
    }

    let distributed_dep_info = result.read_verified_frame(DistributedResultSlot::DepInfo)?;
    let localized_dep_info = localize_distributed_dep_info(&distributed_dep_info, source_root)?;
    let observed_reads = distributed_dep_info_observed_reads(&localized_dep_info, result.staging_path(), source_root)?;
    let dep_info = portable_dep_info_output_bindings(&localized_dep_info, output_paths, source_root, initial_capture)?;
    let stdout = portable_stream_output_bindings(
      &result.read_verified_frame(DistributedResultSlot::Stdout)?,
      output_paths,
      source_root,
    )?;
    let stderr = portable_stream_output_bindings(
      &result.read_verified_frame(DistributedResultSlot::Stderr)?,
      output_paths,
      source_root,
    )?;
    for slot in [DistributedResultSlot::Stdout, DistributedResultSlot::Stderr] {
      if result.verified_frame(slot)?.3 != 0 {
        return Err(RailError::message(
          "distributed result stream has an invalid output mode",
        ));
      }
    }

    let mut frame_descriptors = BTreeMap::from([("dep_info", (digest(&dep_info), dep_info.len() as u64, 0o644))]);
    for (role, _, _) in bindings.iter().skip(1) {
      let slot = match *role {
        "metadata" => DistributedResultSlot::Metadata,
        "rlib" => DistributedResultSlot::Rlib,
        _ => return Err(RailError::message("distributed result output role is unavailable")),
      };
      let (_, content_digest, bytes, mode) = result.verified_frame(slot)?;
      frame_descriptors.insert(*role, (content_digest.to_string(), bytes, mode));
    }
    let outputs = bindings
      .iter()
      .map(|(role, slot, path)| {
        let (content_digest, bytes, mode) = frame_descriptors
          .get(role)
          .ok_or_else(|| RailError::message("distributed result output role is unavailable"))?;
        if !valid_native_output_mode(role, *mode) {
          return Err(RailError::message("distributed result output mode is invalid"));
        }
        Ok(NativeCompilerOutput {
          role: (*role).to_string(),
          slot: (*slot).to_string(),
          file_name: path
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| RailError::message("distributed result output has no UTF-8 file name"))?
            .to_string(),
          content_digest: content_digest.clone(),
          bytes: *bytes,
          mode: *mode,
        })
      })
      .collect::<RailResult<Vec<_>>>()?;

    let mut cache_observation = current_observation.clone();
    cache_observation.observed_reads = observed_reads;
    cache_observation.emitted_outputs = bindings
      .iter()
      .zip(&outputs)
      .map(|((_, _, path), output)| FileObservation {
        path: ObservationPath::capture(path, source_root, source_root),
        content_digest: output.content_digest.clone(),
        executable: source_mode_executable(output.mode),
        symlink_target: None,
      })
      .collect();
    cache_observation.emitted_outputs.sort();
    cache_observation.environment_reads.clear();
    cache_observation.success = true;
    cache_observation.cache_wrapper = None;
    if invocation_bypass_reason(&cache_observation, true, &session.class.host_target).is_some() {
      return Err(RailError::message(
        "distributed result does not complete an eligible native observation",
      ));
    }
    let witness = initial_capture.witness(&cache_observation, source_root)?;
    let selected_action = action_key(&session.identity, &session.class, &cache_observation, initial_capture)?;
    if base_action_key(&session.identity, &session.class, &cache_observation, initial_capture)? != expected_base_action
    {
      return Err(RailError::message(
        "distributed result changed the selected native base action",
      ));
    }
    let validation = NativeCompilerValidation::new(
      session,
      cache_observation,
      &initial_capture.approved_environment,
      None,
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

    let staged_paths = bindings
      .iter()
      .map(|(_, slot, _)| result.staging_path().join(slot))
      .chain([
        result.staging_path().join(STDOUT_SLOT),
        result.staging_path().join(STDERR_SLOT),
      ])
      .collect::<Vec<_>>();
    for directory in staged_paths.iter().filter_map(|path| path.parent()) {
      create_native_staging_parent(result.staging_path(), directory)?;
    }
    write_new_file(&staged_paths[0], &dep_info, 0o644, durable_handoff)?;
    for (((role, native_slot, _), destination), expected) in bindings
      .iter()
      .skip(1)
      .zip(staged_paths.iter().skip(1))
      .zip(validation.outputs.iter().skip(1))
    {
      let slot = match *role {
        "metadata" => DistributedResultSlot::Metadata,
        "rlib" => DistributedResultSlot::Rlib,
        _ => return Err(RailError::message("distributed result output role is unavailable")),
      };
      if destination != &result.staging_path().join(native_slot) {
        return Err(RailError::message("distributed result native staging slot changed"));
      }
      let (content_digest, bytes, mode) = result.move_verified_frame_to(slot, destination)?;
      if content_digest != expected.content_digest || bytes != expected.bytes || mode != expected.mode {
        return Err(RailError::message(
          "distributed result changed while entering native staging",
        ));
      }
      set_native_output_mode(destination, mode)?;
      if durable_handoff {
        let staged = OpenOptions::new().read(true).write(true).open(destination)?;
        sync_native_before_commit(&staged)?;
      }
    }
    write_new_file(
      &result.staging_path().join(STDOUT_SLOT),
      &stdout,
      0o644,
      durable_handoff,
    )?;
    write_new_file(
      &result.staging_path().join(STDERR_SLOT),
      &stderr,
      0o644,
      durable_handoff,
    )?;
    let slots = validation
      .cas_output_bindings()
      .chain(validation.cas_stream_bindings())
      .collect::<Vec<_>>();
    let manifest = crate::cache::result::manifest_from_verified_native_slots(&slots)?;
    let staging = result.into_native_staging()?;
    Ok((staging, manifest, validation))
  })();
  let (staging, manifest, validation) = prepared.map_err(|_| "distributed_result_preparation_failed")?;
  let proof = native_publication_proof(initial_capture, source_root, source_root_spelling)?;
  Ok((
    PreparedNativeResult::from_verified_local_cas_staging(staging, manifest, validation),
    proof,
  ))
}

fn distributed_dep_info_observed_reads(
  bytes: &[u8],
  staging: &Path,
  source_root: &Path,
) -> RailResult<Vec<FileObservation>> {
  let mut dep_info = tempfile::Builder::new()
    .prefix("distributed-dep-info-")
    .tempfile_in(staging)?;
  dep_info.write_all(bytes)?;
  dep_info.flush()?;
  let (_, dependencies) = crate::compiler::observation::makefile_dependency_paths(dep_info.path(), source_root)?;
  let mut observed = dependencies
    .iter()
    .map(|dependency| FileObservation::capture(dependency, source_root, source_root))
    .collect::<RailResult<Vec<_>>>()?;
  observed.sort();
  observed.dedup();
  if observed.is_empty() {
    return Err(RailError::message(
      "distributed compiler dep-info contains no observed source",
    ));
  }
  Ok(observed)
}

fn localize_distributed_dep_info(bytes: &[u8], source_root: &Path) -> RailResult<Vec<u8>> {
  let replacement = escape_dep_info_path(&source_root_display_bytes(source_root));
  let (localized, replacements) = replace_bytes(
    bytes,
    crate::compiler::distributed::VIRTUAL_WORKSPACE.as_bytes(),
    &replacement,
  );
  if replacements == 0
    || localized
      .windows(crate::compiler::distributed::VIRTUAL_ROOT.len())
      .any(|window| window == crate::compiler::distributed::VIRTUAL_ROOT.as_bytes())
  {
    return Err(RailError::message(
      "distributed dep-info contains an unmodeled virtual path",
    ));
  }
  Ok(localized)
}

fn native_publication_proof(
  initial_capture: &NativeActionCapture,
  source_root: &Path,
  source_root_spelling: &Path,
) -> Result<NativePublicationProof, &'static str> {
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
  Ok(NativePublicationProof {
    version: 4,
    source_state: initial_capture.source_state.clone(),
    package_binding: initial_capture.package_binding.clone(),
    approved_environment,
    guard_identity: initial_capture
      .guard_identity()
      .map_err(|_| "cold_final_capture_failed")?,
    environment_bytes_hashed,
  })
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
    linker_generations,
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
          file_name: path
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| RailError::message("native compiler output has no UTF-8 file name"))?
            .to_string(),
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
      linker_generations,
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
      create_native_staging_parent(staging.path(), directory)?;
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
    let manifest = crate::cache::result::manifest_from_verified_native_slots(&slots)?;
    Ok((staging, manifest, validation))
  })();
  let (staging, manifest, validation) = prepared.map_err(|_| "cold_result_preparation_failed")?;
  let proof = native_publication_proof(initial_capture, source_root, source_root_spelling)?;
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
  let (output, copied) = if let Some(output) = crate::utils::try_clone_regular_file(&input, destination) {
    let copied = output.metadata()?.len();
    (output, copied)
  } else {
    let mut output = OpenOptions::new().write(true).create_new(true).open(destination)?;
    let copied = std::io::copy(&mut input.take(expected_bytes.saturating_add(1)), &mut output)?;
    (output, copied)
  };
  set_native_output_mode(destination, expected_mode)?;
  if durable_handoff {
    sync_native_before_commit(&output)?;
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
    sync_native_before_commit(&file)?;
  }
  Ok(())
}

fn create_native_staging_parent(root: &Path, parent: &Path) -> RailResult<()> {
  if !parent.starts_with(root) {
    return Err(RailError::message(
      "native compiler cache slot escaped its staging root",
    ));
  }
  fs::create_dir_all(parent)?;
  #[cfg(unix)]
  {
    let mut current = parent;
    while current != root {
      set_native_output_mode(current, 0o755)?;
      current = current
        .parent()
        .ok_or_else(|| RailError::message("native compiler cache slot escaped its staging root"))?;
    }
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

  if mode & !0o777 != 0 || mode & 0o400 == 0 {
    return Err(RailError::message("native compiler output mode is unsupported"));
  }
  fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
  Ok(())
}

#[cfg(not(unix))]
fn set_native_output_mode(path: &Path, mode: u32) -> RailResult<()> {
  if !matches!(mode, 0o444 | 0o644) {
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

fn write_cache_event(
  status: CompilerCacheWrapperStatus,
  reason: &str,
  action_key: Option<&str>,
  result_key: Option<&str>,
  remote_base_action_key: Option<&str>,
  metrics: NativeCacheMetrics,
) {
  let Some(receipt) = active_context().and_then(|context| context.installation.as_ref()) else {
    return;
  };
  let outcome = match status {
    CompilerCacheWrapperStatus::Hit => b'H',
    CompilerCacheWrapperStatus::Miss => b'M',
    CompilerCacheWrapperStatus::Bypassed | CompilerCacheWrapperStatus::Disabled => b'B',
  };
  crate::cache::installation::record_usage(receipt, outcome);
  let _ = write_benchmark_coverage_event(status, reason, action_key, result_key, remote_base_action_key, metrics);
}

#[derive(Serialize)]
struct NativeBenchmarkCoverageEvent<'a> {
  schema_version: u32,
  lane: &'static str,
  status: CompilerCacheWrapperStatus,
  reason: &'a str,
  action: crate::compiler::operation::CompilerOperation,
  action_id: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  action_key: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  result_key: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  remote_base_action_key: Option<&'a str>,
  compiler: String,
  arguments: Vec<String>,
  current_directory: String,
  bytes_hashed: u64,
  cache_bytes_read: u64,
  remote_request_attempts: u64,
  remote_coordinator_requests: u64,
  remote_payload_bytes_read: u64,
  remote_payload_bytes_written: u64,
  remote_service_elapsed_ns: u64,
  timing: NativeRemoteTimingSnapshot,
  #[serde(skip_serializing_if = "Option::is_none")]
  distributed_timing: Option<crate::compiler::distributed::DistributedTiming>,
  durability: NativeDurabilitySnapshot,
  #[serde(skip_serializing_if = "Option::is_none")]
  remote_error: Option<String>,
}

/// Retain benchmark-only per-invocation evidence after ordinary cache outcome recording.
///
/// The private environment variable is intentionally consulted only after an authenticated
/// context already crossed the usage-recording boundary. Fast bypass and cache-off execution
/// therefore never acquire census context or perform an extra environment lookup.
fn write_benchmark_coverage_event(
  status: CompilerCacheWrapperStatus,
  reason: &str,
  action_key: Option<&str>,
  result_key: Option<&str>,
  remote_base_action_key: Option<&str>,
  metrics: NativeCacheMetrics,
) -> RailResult<()> {
  let Some(directory) = BENCH_COVERAGE_DIRECTORY.get() else {
    return Ok(());
  };
  let mut process_arguments = std::env::args_os().skip(1);
  let compiler = process_arguments
    .next()
    .ok_or_else(|| RailError::message("benchmark compiler coverage has no compiler argument"))?;
  let arguments = process_arguments.collect::<Vec<_>>();
  write_benchmark_coverage_invocation(
    directory,
    status,
    reason,
    action_key,
    result_key,
    remote_base_action_key,
    metrics,
    &compiler,
    &arguments,
  )
}

#[allow(clippy::too_many_arguments)]
fn write_benchmark_coverage_invocation(
  directory: &Path,
  status: CompilerCacheWrapperStatus,
  reason: &str,
  action_key: Option<&str>,
  result_key: Option<&str>,
  remote_base_action_key: Option<&str>,
  metrics: NativeCacheMetrics,
  compiler: &OsStr,
  arguments: &[OsString],
) -> RailResult<()> {
  let compiler = compiler
    .to_str()
    .ok_or_else(|| RailError::message("benchmark compiler coverage has a non-UTF-8 compiler argument"))?
    .to_string();
  let arguments = arguments
    .iter()
    .map(|argument| {
      argument
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| RailError::message("benchmark compiler coverage has a non-UTF-8 argument"))
    })
    .collect::<RailResult<Vec<_>>>()?;
  let current_directory = std::env::current_dir()?
    .into_os_string()
    .into_string()
    .map_err(|_| RailError::message("benchmark compiler coverage has a non-UTF-8 working directory"))?;
  let action = crate::compiler::operation::CompilerOperation::capture(&compiler, &arguments)?;
  let action_id = action.identity()?;
  let remote_state = active_context().and_then(|context| context.remote_store.get());
  let remote = remote_state
    .and_then(|store| store.as_ref().ok())
    .map_or_else(crate::remote_cache::RemoteTransferMetrics::default, |store| {
      store.metrics()
    });
  let remote_error = remote_state
    .and_then(|store| store.as_ref().err())
    .map(ToString::to_string)
    .or_else(|| {
      remote_state
        .and_then(|store| store.as_ref().ok())
        .and_then(crate::remote_cache::RemoteStore::coordinator_connect_error)
        .map(ToString::to_string)
    });
  let encoded = serde_json::to_vec(&NativeBenchmarkCoverageEvent {
    schema_version: 9,
    lane: "cargo-rail",
    status,
    reason,
    action,
    action_id,
    action_key,
    result_key,
    remote_base_action_key,
    compiler,
    arguments,
    current_directory,
    bytes_hashed: metrics.bytes_hashed,
    cache_bytes_read: metrics.cache_bytes_read,
    remote_request_attempts: remote.request_attempts,
    remote_coordinator_requests: remote.coordinator_requests,
    remote_payload_bytes_read: remote.payload_bytes_read,
    remote_payload_bytes_written: remote.payload_bytes_written,
    remote_service_elapsed_ns: remote.service_elapsed_ns,
    timing: metrics.remote_timing,
    distributed_timing: metrics.distributed_timing,
    durability: native_durability_snapshot(),
    remote_error,
  })?;
  if encoded.len() > MAX_BENCH_COVERAGE_EVENT_BYTES {
    return Err(RailError::message(
      "benchmark compiler coverage event exceeds its size bound",
    ));
  }

  let mut temporary = tempfile::Builder::new()
    .prefix(".cargo-rail-native-coverage-")
    .suffix(".tmp")
    .tempfile_in(directory)?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;

    temporary.as_file().set_permissions(fs::Permissions::from_mode(0o600))?;
  }
  temporary.write_all(&encoded)?;
  temporary.write_all(b"\n")?;
  let temporary_identity = ContentDigest::sha256(temporary.path().as_os_str().as_encoded_bytes());
  let destination = directory.join(format!("event-{temporary_identity}.json"));
  temporary
    .persist_noclobber(destination)
    .map_err(|error| RailError::from(error.error))?;
  Ok(())
}

/// Activate benchmark-only evidence after the existing cache-control read selected it.
///
/// Invalid or hostile evidence paths disable evidence without changing compiler behavior.
pub(crate) fn activate_benchmark_coverage() {
  let Some(directory) = std::env::var_os(BENCH_COVERAGE_DIRECTORY_ENV).map(PathBuf::from) else {
    return;
  };
  if validate_benchmark_coverage_directory(&directory).is_ok() {
    let _ = BENCH_COVERAGE_DIRECTORY.set(directory);
    let _ = BENCH_DURABILITY_COUNTERS.set(NativeDurabilityCounters::new());
  }
}

/// Record one acquisition-free direct-wrapper bypass for the explicit benchmark census.
pub(crate) fn record_benchmark_coverage_bypass(program: &OsStr, arguments: &[OsString], reason: &str) {
  let Some(directory) = BENCH_COVERAGE_DIRECTORY.get() else {
    return;
  };
  let _ = write_benchmark_coverage_invocation(
    directory,
    CompilerCacheWrapperStatus::Bypassed,
    reason,
    None,
    None,
    None,
    NativeCacheMetrics::default(),
    program,
    arguments,
  );
}

fn validate_benchmark_coverage_directory(directory: &Path) -> RailResult<()> {
  if !directory.is_absolute() {
    return Err(RailError::message(
      "benchmark compiler coverage directory is not absolute",
    ));
  }
  let metadata = fs::symlink_metadata(directory)?;
  if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) || fs::canonicalize(directory)? != directory {
    return Err(RailError::message(
      "benchmark compiler coverage directory is not one real canonical directory",
    ));
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.permissions().mode() & 0o077 != 0 {
      return Err(RailError::message(
        "benchmark compiler coverage directory is not private to the current user",
      ));
    }
  }
  Ok(())
}

/// Record an operational wrapper failure after an installed context was authenticated.
pub(crate) fn record_active_failure() {
  if let Some(receipt) = active_context().and_then(|context| context.installation.as_ref()) {
    crate::cache::installation::record_usage(receipt, b'F');
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

  fn metadata_output_paths(dep_info: PathBuf, metadata: PathBuf) -> NativeOutputPaths {
    NativeOutputPaths {
      dep_info,
      artifacts: vec![crate::compiler::observation::NativeOutputArtifact {
        role: NativeOutputRole::Metadata,
        path: metadata,
      }],
    }
  }

  #[cfg(target_os = "macos")]
  fn write_apple_link_certificate(path: &Path, entries: &[(u8, &Path)]) {
    let mut bytes = vec![0];
    bytes.extend_from_slice(b"@(#)PROGRAM:ld PROJECT:ld-1234.5\0");
    for (opcode, entry) in entries {
      bytes.push(*opcode);
      bytes.extend_from_slice(entry.as_os_str().as_encoded_bytes());
      bytes.push(0);
    }
    fs::write(path, bytes).expect("Apple linker certificate");
  }

  #[cfg(target_os = "macos")]
  fn write_apple_link_driver_evidence(
    path: &Path,
    direct_inputs: &[&Path],
    temporary_directories: &[&Path],
    preexisting_paths: &[&Path],
    generated_inputs: &[&Path],
  ) {
    let paths = |values: &[&Path]| {
      let mut values = values
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
      values.sort();
      values.dedup();
      values
    };
    let evidence = AppleLinkDriverEvidence {
      version: APPLE_LINK_DRIVER_EVIDENCE_VERSION,
      direct_inputs: paths(direct_inputs),
      temporary_directories: paths(temporary_directories),
      preexisting_paths: paths(preexisting_paths),
      generated_inputs: paths(generated_inputs),
    };
    fs::write(path, serde_json::to_vec(&evidence).expect("driver-input encoding")).expect("Apple linker driver inputs");
  }

  #[cfg(target_os = "macos")]
  fn write_apple_link_driver_inputs(path: &Path, inputs: &[&Path]) {
    write_apple_link_driver_evidence(path, inputs, &[], &[], &[]);
  }

  #[cfg(target_os = "macos")]
  #[test]
  fn apple_link_witness_binds_found_missing_toolchain_and_endogenous_inputs() {
    let state = tempfile::tempdir().expect("Apple witness state");
    let root = fs::canonicalize(state.path()).expect("canonical state");
    let linked = root.join("fixture");
    let endogenous = root.join("fixture.0.o");
    let found = root.join("stable-link-input.tbd");
    let missing = root.join("absent-link-input.tbd");
    let certificate = root.join("linker-dependencies.bin");
    let driver_inputs = root.join("linker-driver-inputs.json");
    fs::write(&linked, b"linked-output").expect("linked output");
    fs::write(&endogenous, b"object").expect("endogenous object");
    fs::write(&found, b"stable-one").expect("stable input");
    write_apple_link_certificate(
      &certificate,
      &[(0x10, &endogenous), (0x10, &found), (0x11, &missing), (0x40, &linked)],
    );
    write_apple_link_driver_inputs(&driver_inputs, &[&endogenous]);
    let outputs = NativeOutputPaths {
      dep_info: root.join("fixture.d"),
      artifacts: vec![crate::compiler::observation::NativeOutputArtifact {
        role: NativeOutputRole::Executable,
        path: linked,
      }],
    };
    let mut observation = graduated_observation();
    observation.crate_types = BTreeSet::from(["bin".to_string()]);
    observation.emit_modes = BTreeSet::from(["dep-info".to_string(), "link".to_string()]);

    let authority = "a".repeat(64);
    let (witness, generations, bytes_hashed) =
      capture_apple_linker_witness(&observation, &outputs, &certificate, &driver_inputs, Some(&authority))
        .expect("closed Apple witness");
    let generations = generations.expect("local generation proof");
    assert!(bytes_hashed > 0);
    assert_eq!(witness.endogenous_objects, 1);
    assert_eq!(witness.missing, vec![missing.to_string_lossy()]);
    assert_eq!(witness.found.len(), 1);
    assert_eq!(witness.found[0].path, found.to_string_lossy());
    assert_eq!(
      revalidate_apple_linker_witness(&witness, Some(&generations), Some(&authority)).expect("same installation"),
      0
    );
    fs::write(&found, b"stable-one").expect("same-content generation change");
    assert!(
      revalidate_apple_linker_witness(&witness, Some(&generations), Some(&authority)).expect("same-content fallback")
        > 0
    );
    assert!(
      revalidate_apple_linker_witness(&witness, Some(&generations), None).expect("foreign installation fallback") > 0
    );

    fs::write(&found, b"stable-two").expect("mutated stable input");
    assert!(revalidate_apple_linker_witness(&witness, Some(&generations), Some(&authority)).is_err());
  }

  #[cfg(target_os = "macos")]
  #[test]
  fn apple_link_witness_recognizes_dynamic_rustc_object_names() {
    let state = tempfile::tempdir().expect("Apple witness state");
    let root = fs::canonicalize(state.path()).expect("canonical state");
    let certificate = root.join("linker-dependencies.bin");
    let driver_inputs = root.join("linker-driver-inputs.json");

    for (role, crate_type, linked_name, object_name) in [
      (
        NativeOutputRole::ProcMacro,
        "proc-macro",
        "libfixture_macros.dylib",
        "fixture_macros.0.rcgu.o",
      ),
      (
        NativeOutputRole::Dylib,
        "dylib",
        "libfixture_dylib.dylib",
        "fixture_dylib.0.rcgu.o",
      ),
      (
        NativeOutputRole::Cdylib,
        "cdylib",
        "libfixture_cdylib.dylib",
        "fixture_cdylib.0.rcgu.o",
      ),
    ] {
      let linked = root.join(linked_name);
      let removed_object = root.join(object_name);
      fs::write(&linked, b"linked-output").expect("linked output");
      write_apple_link_certificate(&certificate, &[(0x10, &removed_object), (0x40, &linked)]);
      write_apple_link_driver_inputs(&driver_inputs, &[&removed_object]);
      let outputs = NativeOutputPaths {
        dep_info: root.join(format!("{crate_type}.d")),
        artifacts: vec![crate::compiler::observation::NativeOutputArtifact { role, path: linked }],
      };
      let mut observation = graduated_observation();
      observation.crate_types = BTreeSet::from([crate_type.to_string()]);
      observation.emit_modes = BTreeSet::from(["dep-info".to_string(), "link".to_string()]);

      let (witness, _, _) = capture_apple_linker_witness(&observation, &outputs, &certificate, &driver_inputs, None)
        .expect("dynamic Apple witness");
      assert_eq!(witness.endogenous_objects, 1, "{crate_type}");
      assert!(witness.found.is_empty(), "{crate_type}");
    }
  }

  #[cfg(target_os = "macos")]
  #[test]
  fn apple_link_witness_rejects_uncertified_or_user_selected_objects() {
    let state = tempfile::tempdir().expect("Apple witness state");
    let root = fs::canonicalize(state.path()).expect("canonical state");
    let linked = root.join("fixture");
    let object = root.join("fixture.0.o");
    let certificate = root.join("linker-dependencies.bin");
    let driver_inputs = root.join("linker-driver-inputs.json");
    fs::write(&linked, b"linked-output").expect("linked output");
    fs::write(&object, b"mutable-object").expect("object input");
    write_apple_link_certificate(&certificate, &[(0x10, &object), (0x40, &linked)]);
    let outputs = NativeOutputPaths {
      dep_info: root.join("fixture.d"),
      artifacts: vec![crate::compiler::observation::NativeOutputArtifact {
        role: NativeOutputRole::Executable,
        path: linked,
      }],
    };
    let mut observation = graduated_observation();
    observation.crate_types = BTreeSet::from(["bin".to_string()]);
    observation.emit_modes = BTreeSet::from(["dep-info".to_string(), "link".to_string()]);

    write_apple_link_driver_inputs(&driver_inputs, &[]);
    assert!(capture_apple_linker_witness(&observation, &outputs, &certificate, &driver_inputs, None).is_err());

    fs::remove_file(&driver_inputs).expect("replace driver-input certificate");
    write_apple_link_driver_inputs(&driver_inputs, &[&object]);
    observation
      .compiler_arguments
      .push(format!("-Clink-arg={}", object.display()));
    assert!(capture_apple_linker_witness(&observation, &outputs, &certificate, &driver_inputs, None).is_err());
  }

  #[cfg(target_os = "macos")]
  #[test]
  fn apple_link_witness_accepts_only_adapter_certified_lto_objects() {
    let state = tempfile::tempdir().expect("Apple witness state");
    let root = fs::canonicalize(state.path()).expect("canonical state");
    let temporary = root.join("rustcABC123");
    fs::create_dir(&temporary).expect("rustc temporary directory");
    let linked = root.join("fixture");
    let generated = temporary.join("fixture.lto.o");
    let aggregate = temporary.join("lib-lto.rlib");
    let certificate = root.join("linker-dependencies.bin");
    let driver_inputs = root.join("linker-driver-inputs.json");
    fs::write(&linked, b"linked-output").expect("linked output");
    fs::write(&generated, b"generated object").expect("generated object");
    fs::write(&aggregate, b"rustc aggregate archive").expect("aggregate archive");
    write_apple_link_certificate(&certificate, &[(0x10, &generated), (0x10, &aggregate), (0x40, &linked)]);
    write_apple_link_driver_evidence(
      &driver_inputs,
      &[&aggregate],
      &[&temporary],
      &[&aggregate],
      &[&generated],
    );
    let outputs = NativeOutputPaths {
      dep_info: root.join("fixture.d"),
      artifacts: vec![crate::compiler::observation::NativeOutputArtifact {
        role: NativeOutputRole::Executable,
        path: linked,
      }],
    };
    let mut observation = graduated_observation();
    observation.crate_types = BTreeSet::from(["bin".to_string()]);
    observation.emit_modes = BTreeSet::from(["dep-info".to_string(), "link".to_string()]);

    let (witness, _, _) = capture_apple_linker_witness(&observation, &outputs, &certificate, &driver_inputs, None)
      .expect("adapter-certified LTO inputs");
    assert_eq!(witness.endogenous_archives, 1);

    write_apple_link_driver_evidence(
      &driver_inputs,
      &[&aggregate],
      &[&temporary],
      &[&aggregate, &generated],
      &[],
    );
    assert!(
      capture_apple_linker_witness(&observation, &outputs, &certificate, &driver_inputs, None).is_err(),
      "a preexisting lookalike is not linker-generated authority"
    );
  }

  #[cfg(target_os = "macos")]
  #[test]
  fn apple_link_witness_binds_reported_symlink_resolution() {
    use std::os::unix::fs::symlink;

    let state = tempfile::tempdir().expect("Apple witness state");
    let root = fs::canonicalize(state.path()).expect("canonical state");
    let linked = root.join("fixture");
    let endogenous = root.join("fixture.0.o");
    let first = root.join("first.tbd");
    let second = root.join("second.tbd");
    let reported = root.join("versioned-sdk-input.tbd");
    let certificate = root.join("linker-dependencies.bin");
    let driver_inputs = root.join("linker-driver-inputs.json");
    fs::write(&linked, b"linked-output").expect("linked output");
    fs::write(&endogenous, b"object").expect("endogenous object");
    fs::write(&first, b"same-content").expect("first SDK input");
    fs::write(&second, b"same-content").expect("second SDK input");
    symlink(&first, &reported).expect("reported SDK symlink");
    write_apple_link_certificate(&certificate, &[(0x10, &endogenous), (0x10, &reported), (0x40, &linked)]);
    write_apple_link_driver_inputs(&driver_inputs, &[&endogenous]);
    let outputs = NativeOutputPaths {
      dep_info: root.join("fixture.d"),
      artifacts: vec![crate::compiler::observation::NativeOutputArtifact {
        role: NativeOutputRole::Executable,
        path: linked,
      }],
    };
    let mut observation = graduated_observation();
    observation.crate_types = BTreeSet::from(["bin".to_string()]);
    observation.emit_modes = BTreeSet::from(["dep-info".to_string(), "link".to_string()]);

    let (witness, _, _) = capture_apple_linker_witness(&observation, &outputs, &certificate, &driver_inputs, None)
      .expect("symlinked Apple witness");
    assert_eq!(witness.found[0].path, reported.to_string_lossy());
    assert_eq!(witness.found[0].canonical_path, first.to_string_lossy());
    assert!(revalidate_apple_linker_witness(&witness, None, None).is_ok());

    fs::remove_file(&reported).expect("remove reported SDK symlink");
    symlink(&second, &reported).expect("retarget reported SDK symlink");
    assert!(revalidate_apple_linker_witness(&witness, None, None).is_err());
  }

  #[cfg(target_os = "macos")]
  #[test]
  fn apple_link_certificate_rejects_unknown_and_unterminated_records() {
    assert!(parse_apple_link_certificate(b"\0@(#)PROGRAM:ld PROJECT:ld-1").is_err());

    let state = tempfile::tempdir().expect("Apple witness state");
    let root = fs::canonicalize(state.path()).expect("canonical state");
    let linked = root.join("fixture");
    let endogenous = root.join("fixture.0.o");
    let certificate = root.join("linker-dependencies.bin");
    let driver_inputs = root.join("linker-driver-inputs.json");
    fs::write(&linked, b"linked-output").expect("linked output");
    fs::write(&endogenous, b"object").expect("endogenous object");
    write_apple_link_certificate(&certificate, &[(0x10, &endogenous), (0x12, &linked), (0x40, &linked)]);
    write_apple_link_driver_inputs(&driver_inputs, &[&endogenous]);
    let outputs = NativeOutputPaths {
      dep_info: root.join("fixture.d"),
      artifacts: vec![crate::compiler::observation::NativeOutputArtifact {
        role: NativeOutputRole::Executable,
        path: linked,
      }],
    };
    let mut observation = graduated_observation();
    observation.crate_types = BTreeSet::from(["bin".to_string()]);
    observation.emit_modes = BTreeSet::from(["dep-info".to_string(), "link".to_string()]);
    assert!(capture_apple_linker_witness(&observation, &outputs, &certificate, &driver_inputs, None).is_err());
  }

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
  }

  #[test]
  fn transparent_compilation_root_comes_from_the_standard_cargo_output_layout() {
    let workspace = tempfile::tempdir().expect("workspace");
    fs::write(workspace.path().join("Cargo.toml"), "[workspace]\n").expect("workspace manifest");
    let output = workspace.path().join("target/debug/deps");
    fs::create_dir_all(&output).expect("output directory");
    let arguments = vec![
      OsString::from("--out-dir"),
      output.as_os_str().to_owned(),
      OsString::from("--crate-type=lib"),
    ];
    let (_, root) = direct_compilation_root(&arguments).expect("standard Cargo target root");
    assert_eq!(
      root,
      crate::utils::canonicalize_existing(workspace.path()).expect("canonical workspace")
    );

    let custom = workspace.path().join("custom/debug/deps");
    fs::create_dir_all(&custom).expect("custom output directory");
    let unsupported = vec![OsString::from(format!("--out-dir={}", custom.display()))];
    assert!(direct_compilation_root(&unsupported).is_err());
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
      version: 6,
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
      compiler_fact_unit: None,
    }
  }

  #[test]
  fn distributed_candidate_requires_closed_source_dependency_and_argument_authority() -> RailResult<()> {
    let workspace = tempfile::tempdir()?;
    fs::write(
      workspace.path().join("Cargo.toml"),
      "[package]\nname='fixture'\nversion='0.0.0'\n",
    )?;
    fs::create_dir(workspace.path().join("src"))?;
    fs::create_dir_all(workspace.path().join("target/debug/deps"))?;
    let source = workspace.path().join("src/lib.rs");
    fs::write(&source, b"pub fn value() -> u8 { 1 }\n")?;
    let suffix = "-0123456789abcdef";
    let output = workspace.path().join("target/debug/deps");
    let arguments = [
      "--crate-name".to_string(),
      "fixture".to_string(),
      "--crate-type=lib".to_string(),
      "--edition".to_string(),
      "2024".to_string(),
      "--error-format=json".to_string(),
      "--json=diagnostic-rendered-ansi,artifacts,future-incompat".to_string(),
      "--emit".to_string(),
      format!(
        "dep-info={},metadata={},link={}",
        output.join(format!("fixture{suffix}.d")).display(),
        output.join(format!("libfixture{suffix}.rmeta")).display(),
        output.join(format!("libfixture{suffix}.rlib")).display()
      ),
      "-Copt-level=3".to_string(),
      "-Cembed-bitcode=no".to_string(),
      "--check-cfg".to_string(),
      "cfg(docsrs,test)".to_string(),
      "--check-cfg".to_string(),
      "cfg(feature, values())".to_string(),
      "-Cmetadata=0123456789abcdef".to_string(),
      format!("-Cextra-filename={suffix}"),
      "--out-dir".to_string(),
      output.to_string_lossy().into_owned(),
      "-Cstrip=debuginfo".to_string(),
      "-L".to_string(),
      format!("dependency={}", output.display()),
      "--remap-path-prefix".to_string(),
      format!(
        "{}={}",
        workspace.path().display(),
        crate::compiler::distributed::VIRTUAL_WORKSPACE
      ),
      "src/lib.rs".to_string(),
    ]
    .map(OsString::from);
    let observation_directory = tempfile::tempdir()?;
    let recorder = crate::compiler::observation::begin_invocation_in(
      observation_directory.path(),
      workspace.path(),
      workspace.path(),
      OsStr::new("rustc"),
      &arguments,
    )?;
    let output_paths = recorder
      .native_output_paths()
      .ok_or_else(|| RailError::message("test native output paths were unavailable"))?;
    let observation = recorder.observation().clone();
    let capture = NativeActionCapture::capture(&observation, workspace.path())?;
    assert!(matches!(
      distributed_rust_library_candidate(
        &observation,
        &capture,
        &output_paths,
        workspace.path(),
        workspace.path(),
      ),
      Err("distributed_action_class_ineligible")
    ));

    let mut closed = capture.clone();
    closed.generated = None;
    let authority = distributed_rust_library_authority(&observation, &closed, &output_paths, workspace.path())
      .map_err(RailError::message)?;
    assert_eq!(authority.crate_name, "fixture");
    assert_eq!(authority.crate_type, "lib");
    assert_eq!(authority.edition, "2024");
    assert_eq!(
      authority.emission,
      crate::compiler::distributed::RustLibraryEmission::MetadataAndLink
    );
    assert_eq!(authority.metadata, "0123456789abcdef");
    assert_eq!(authority.extra_filename, suffix);
    assert!(authority.execution_options.cargo_json_diagnostics);
    assert_eq!(
      authority.execution_options.check_cfg,
      ["cfg(docsrs,test)", "cfg(feature, values())"]
    );
    assert_eq!(authority.execution_options.codegen.opt_level.as_deref(), Some("3"));
    assert_eq!(authority.execution_options.codegen.embed_bitcode, Some(false));
    assert_eq!(authority.execution_options.codegen.strip.as_deref(), Some("debuginfo"));
    assert!(authority.execution_options.output_dependency_search);
    let canonical_source = crate::utils::canonicalize_existing(&source)?;
    assert!(
      authority
        .sources
        .iter()
        .any(|input| input.path == canonical_source && input.repository_relative_path == "src/lib.rs")
    );
    assert_eq!(authority.source_relative_path, "src/lib.rs");
    assert_eq!(authority.output_relative_directory, "target/debug/deps");
    let mut unnormalized = observation.clone();
    unnormalized
      .compiler_arguments
      .retain(|argument| argument != "--remap-path-prefix" && !distributed_workspace_remap(argument));
    assert!(matches!(
      distributed_rust_library_authority(&unnormalized, &closed, &output_paths, workspace.path()),
      Err("distributed_argument_authority_mismatch")
    ));
    let normalization =
      distributed_rust_library_authority_with_remap(&unnormalized, &closed, &output_paths, workspace.path(), false)
        .map_err(RailError::message)?;
    let DistributedRustLibraryAuthority {
      crate_name,
      crate_type,
      dep_info_name,
      edition,
      emission,
      execution_options,
      extra_filename,
      metadata,
      metadata_name,
      output_relative_directory,
      rlib_name,
      dependencies,
      sources,
      source_relative_path,
      test_mode,
      toolchain_proc_macro,
    } = normalization;
    let normalization = crate::compiler::distributed::RustLibraryCandidate::from_captured_inputs(
      crate::compiler::distributed::RustLibraryCandidateInput {
        crate_name,
        crate_type,
        dep_info_name,
        edition,
        emission,
        metadata,
        metadata_name,
        extra_filename,
        output_relative_directory,
        source_relative_path,
        test_mode,
        toolchain_proc_macro,
        rlib_name,
        options: execution_options,
      },
      sources,
      dependencies,
    )?;
    let temporary = tempfile::tempdir()?;
    let command = normalization.normalized_local_command(OsStr::new("rustc"), workspace.path(), temporary.path())?;
    let normalized_arguments = command
      .get_args()
      .map(|argument| argument.to_string_lossy().into_owned())
      .collect::<Vec<_>>();
    assert!(normalized_arguments.iter().any(|argument| argument == "src/lib.rs"));
    assert!(
      normalized_arguments
        .iter()
        .any(|argument| argument == "--remap-path-prefix")
    );
    assert!(normalized_arguments.iter().any(|argument| {
      argument.as_str()
        == format!(
          "{}={}",
          fs::canonicalize(workspace.path())
            .expect("canonical workspace")
            .display(),
          crate::compiler::distributed::VIRTUAL_WORKSPACE
        )
    }));

    let mut metadata_observation = observation.clone();
    metadata_observation.emit_modes = BTreeSet::from(["dep-info".to_string(), "metadata".to_string()]);
    let emit_value = metadata_observation
      .compiler_arguments
      .iter()
      .position(|argument| argument == "--emit")
      .and_then(|index| metadata_observation.compiler_arguments.get_mut(index + 1))
      .ok_or_else(|| RailError::message("test compiler arguments have no emit value"))?;
    *emit_value = format!(
      "dep-info={},metadata={}",
      output.join(format!("fixture{suffix}.d")).display(),
      output.join(format!("libfixture{suffix}.rmeta")).display()
    );
    let metadata_output_paths = NativeOutputPaths {
      dep_info: output_paths.dep_info.clone(),
      artifacts: vec![crate::compiler::observation::NativeOutputArtifact {
        role: NativeOutputRole::Metadata,
        path: output.join(format!("libfixture{suffix}.rmeta")),
      }],
    };
    let mut metadata_capture = NativeActionCapture::capture(&metadata_observation, workspace.path())?;
    metadata_capture.generated = None;
    let metadata_authority = distributed_rust_library_authority(
      &metadata_observation,
      &metadata_capture,
      &metadata_output_paths,
      workspace.path(),
    )
    .map_err(RailError::message)?;
    assert_eq!(
      metadata_authority.emission,
      crate::compiler::distributed::RustLibraryEmission::Metadata
    );
    let DistributedRustLibraryAuthority {
      crate_name,
      crate_type,
      dependencies,
      dep_info_name,
      edition,
      emission,
      execution_options,
      extra_filename,
      metadata,
      metadata_name,
      output_relative_directory,
      rlib_name,
      sources,
      source_relative_path,
      test_mode,
      toolchain_proc_macro,
    } = metadata_authority;
    let metadata_candidate = crate::compiler::distributed::RustLibraryCandidate::from_captured_inputs(
      crate::compiler::distributed::RustLibraryCandidateInput {
        crate_name,
        crate_type,
        dep_info_name,
        edition,
        emission,
        metadata,
        metadata_name,
        extra_filename,
        output_relative_directory,
        source_relative_path,
        test_mode,
        toolchain_proc_macro,
        rlib_name,
        options: execution_options,
      },
      sources,
      dependencies,
    )?;
    let metadata_command =
      metadata_candidate.normalized_local_command(OsStr::new("rustc"), workspace.path(), temporary.path())?;
    let metadata_emit = metadata_command
      .get_args()
      .map(|argument| argument.to_string_lossy())
      .find(|argument| argument.starts_with("dep-info="))
      .ok_or_else(|| RailError::message("metadata fallback command has no emit contract"))?;
    assert!(metadata_emit.contains(",metadata=") && !metadata_emit.contains(",link="));

    let mut capped = observation.clone();
    capped.compiler_arguments.push("--cap-lints=allow".to_string());
    let capped = distributed_rust_library_authority(&capped, &closed, &output_paths, workspace.path())
      .map_err(RailError::message)?;
    assert_eq!(capped.execution_options.cap_lints.as_deref(), Some("allow"));

    let mut unmodeled = observation.clone();
    unmodeled.compiler_arguments.push("--crate-attr=custom".to_string());
    assert!(matches!(
      distributed_rust_library_authority(&unmodeled, &closed, &output_paths, workspace.path()),
      Err("distributed_argument_shape_ineligible")
    ));

    fs::write(workspace.path().join("src/late.rs"), b"pub fn late() {}\n")?;
    let mut expanded = NativeActionCapture::capture(&observation, workspace.path())?;
    expanded.generated = None;
    let expanded = distributed_rust_library_authority(&observation, &expanded, &output_paths, workspace.path())
      .map_err(RailError::message)?;
    assert!(
      expanded
        .sources
        .iter()
        .any(|input| input.repository_relative_path == "src/late.rs")
    );
    Ok(())
  }

  #[test]
  fn distributed_workspace_remap_accepts_only_the_exact_current_directory() -> RailResult<()> {
    let workspace = tempfile::tempdir()?;
    let virtual_root = crate::compiler::distributed::VIRTUAL_WORKSPACE;
    let physical = format!("{}={virtual_root}", workspace.path().display());
    let sibling = format!(
      "{}={virtual_root}",
      workspace.path().with_extension("sibling").display()
    );

    assert!(distributed_workspace_remap_at(&physical, Some(workspace.path())));
    assert!(!distributed_workspace_remap_at(&physical, None));
    assert!(!distributed_workspace_remap_at(&sibling, Some(workspace.path())));
    assert!(!distributed_workspace_remap_at(
      &format!("{}=/different", workspace.path().display()),
      Some(workspace.path())
    ));
    assert!(distributed_workspace_remap_at(
      &format!("repository:={virtual_root}"),
      None
    ));
    Ok(())
  }

  #[cfg(unix)]
  #[test]
  fn native_staging_parent_modes_are_independent_of_the_callers_umask() -> RailResult<()> {
    let staging = tempfile::tempdir()?;
    let parent = staging.path().join("target/outputs");
    fs::create_dir_all(&parent)?;
    set_native_output_mode(&staging.path().join("target"), 0o775)?;
    set_native_output_mode(&parent, 0o775)?;

    create_native_staging_parent(staging.path(), &parent)?;

    assert_eq!(
      native_output_mode(&fs::symlink_metadata(staging.path().join("target"))?),
      0o755
    );
    assert_eq!(native_output_mode(&fs::symlink_metadata(parent)?), 0o755);
    Ok(())
  }

  #[test]
  fn distributed_result_requires_live_revalidation_before_l1_admission() -> RailResult<()> {
    let workspace = tempfile::tempdir()?;
    fs::write(
      workspace.path().join("Cargo.toml"),
      "[package]\nname='fixture'\nversion='0.0.0'\n",
    )?;
    fs::create_dir(workspace.path().join("src"))?;
    fs::create_dir_all(workspace.path().join("target/debug/deps"))?;
    fs::write(workspace.path().join("src/lib.rs"), b"pub fn value() -> u8 { 1 }\n")?;
    let workspace_root = crate::utils::canonicalize_existing(workspace.path())?;
    let suffix = "-0123456789abcdef";
    let output = workspace_root.join("target/debug/deps");
    let arguments = [
      "--crate-name".to_string(),
      "fixture".to_string(),
      "--crate-type=lib".to_string(),
      "--edition=2024".to_string(),
      "--emit".to_string(),
      format!(
        "dep-info={},metadata={},link={}",
        output.join(format!("fixture{suffix}.d")).display(),
        output.join(format!("libfixture{suffix}.rmeta")).display(),
        output.join(format!("libfixture{suffix}.rlib")).display()
      ),
      "-Cmetadata=0123456789abcdef".to_string(),
      format!("-Cextra-filename={suffix}"),
      "--out-dir".to_string(),
      output.to_string_lossy().into_owned(),
      "--remap-path-prefix".to_string(),
      format!(
        "{}={}",
        workspace_root.display(),
        crate::compiler::distributed::VIRTUAL_WORKSPACE
      ),
      "src/lib.rs".to_string(),
    ]
    .map(OsString::from);
    let observations = tempfile::tempdir()?;
    let recorder = crate::compiler::observation::begin_invocation_in(
      observations.path(),
      &workspace_root,
      &workspace_root,
      OsStr::new("rustc"),
      &arguments,
    )?;
    let output_paths = recorder
      .native_output_paths()
      .ok_or_else(|| RailError::message("test native output paths were unavailable"))?;
    let observation = recorder.observation().clone();
    let capture = NativeActionCapture::capture(&observation, &workspace_root)?;
    let declared = observation
      .declared_inputs
      .first()
      .ok_or_else(|| RailError::message("test source input was unavailable"))?;
    let ObservationPath::Repository(namespace) = &capture.source_state.root else {
      return Err(RailError::message("test source namespace was not repository-owned"));
    };
    let sources = distributed_source_inputs(&capture, namespace, "src/lib.rs", declared).map_err(RailError::message)?;
    let candidate = crate::compiler::distributed::RustLibraryCandidate::from_captured_inputs(
      crate::compiler::distributed::RustLibraryCandidateInput {
        crate_name: "fixture".to_string(),
        crate_type: "lib".to_string(),
        dep_info_name: format!("fixture{suffix}.d"),
        edition: "2024".to_string(),
        emission: crate::compiler::distributed::RustLibraryEmission::MetadataAndLink,
        metadata: "0123456789abcdef".to_string(),
        metadata_name: format!("libfixture{suffix}.rmeta"),
        extra_filename: suffix.to_string(),
        output_relative_directory: "target/debug/deps".to_string(),
        source_relative_path: "src/lib.rs".to_string(),
        test_mode: false,
        toolchain_proc_macro: false,
        rlib_name: Some(format!("libfixture{suffix}.rlib")),
        options: crate::compiler::distributed::RustLibraryExecutionOptions::default(),
      },
      sources,
      Vec::new(),
    )?;
    let dep_info = format!(
      "{}/target/debug/deps/fixture{suffix}.d: src/lib.rs\n",
      crate::compiler::distributed::VIRTUAL_WORKSPACE
    );
    let result = crate::compiler::distributed::StagedExecutionResult::from_test_frames(
      &candidate,
      dep_info.as_bytes(),
      b"metadata bytes",
      b"rlib bytes",
      b"",
      b"",
    )?;
    let session = graduated_session(path_identity(&workspace_root)?);
    let base_action = base_action_key(&session.identity, &session.class, &observation, &capture)?;
    let cache_root = tempfile::tempdir()?;
    let selection = crate::cache::cas::LocalCacheSelection::new(
      cache_root.path().to_path_buf(),
      1024 * 1024 * 1024,
      Some("d".repeat(64)),
    )?;
    let cas = LocalCas::open_selected(&selection)?;
    let (prepared, proof) = prepare_distributed_result(
      &session,
      &capture,
      &base_action,
      &observation,
      &output_paths,
      result,
      &workspace_root,
      &workspace_root,
    )
    .map_err(RailError::message)?;
    let drift_result = crate::compiler::distributed::StagedExecutionResult::from_test_frames(
      &candidate,
      dep_info.as_bytes(),
      b"metadata bytes",
      b"rlib bytes",
      b"",
      b"",
    )?;
    let (drift_prepared, drift_proof) = prepare_distributed_result(
      &session,
      &capture,
      &base_action,
      &observation,
      &output_paths,
      drift_result,
      &workspace_root,
      &workspace_root,
    )
    .map_err(RailError::message)?;
    let (validation, _) = cas.store_native_revalidated(prepared, |validation| {
      validation
        .revalidate_publication(&session, &workspace_root, &proof, None)
        .map(|_| ())
        .map_err(|failure| failure.error)
    })?;
    assert_eq!(
      validation.action_key(),
      action_key(&session.identity, &session.class, &observation, &capture)?
    );
    assert!(matches!(
      cas.native_action(validation.action_key())?,
      crate::cache::cas::NativeActionLookup::Hit(_)
    ));
    assert!(matches!(
      cas.publish_native_environment_selector(&base_action, &[])?,
      crate::cache::cas::NativeEnvironmentSelectorPublication::Created
        | crate::cache::cas::NativeEnvironmentSelectorPublication::Converged
    ));
    let restore_context = NativeCacheContext {
      session: NativeCacheSession::Prepared(session.clone()),
      source_root: workspace_root.clone(),
      source_root_spelling: workspace_root.clone(),
      observation_directory: observations.path().to_path_buf(),
      local_cas: Some(cas.clone()),
      remote: None,
      remote_store: OnceLock::new(),
      installation: None,
      _runtime: None,
    };
    let mut metrics = NativeCacheMetrics::default();
    match cas.native_action(validation.action_key())? {
      crate::cache::cas::NativeActionLookup::Hit(cached) => restore_and_publish(
        &restore_context,
        &cas,
        NativeRestoreSource::Materialized {
          cached: &cached,
          hit_source: NativeHitSource::Distributed {
            base_action_key: &base_action,
          },
        },
        &capture,
        &observation,
        &output_paths,
        &mut metrics,
      )
      .map_err(|failure| match failure {
        RestorePublishFailure::BeforeEffect(error)
        | RestorePublishFailure::AfterEffect(error)
        | RestorePublishFailure::Operational(error) => error,
      })?,
      crate::cache::cas::NativeActionLookup::Packed(_) | crate::cache::cas::NativeActionLookup::Miss(_) => {
        return Err(RailError::message("distributed test L1 authority disappeared"));
      }
    }
    assert_eq!(
      fs::read(output.join(format!("libfixture{suffix}.rmeta")))?,
      b"metadata bytes"
    );
    assert_eq!(
      fs::read(output.join(format!("libfixture{suffix}.rlib")))?,
      b"rlib bytes"
    );
    let restored_dep_info = fs::read(output.join(format!("fixture{suffix}.d")))?;
    assert!(
      restored_dep_info
        .windows(b"src/lib.rs".len())
        .any(|window| window == b"src/lib.rs")
    );
    assert!(
      !restored_dep_info
        .windows(crate::compiler::distributed::VIRTUAL_ROOT.len())
        .any(|window| { window == crate::compiler::distributed::VIRTUAL_ROOT.as_bytes() })
    );
    fs::write(workspace_root.join("src/lib.rs"), b"pub fn changed() {}\n")?;
    assert!(
      cas
        .store_native_revalidated(drift_prepared, |validation| {
          validation
            .revalidate_publication(&session, &workspace_root, &drift_proof, None)
            .map(|_| ())
            .map_err(|failure| failure.error)
        })
        .is_err()
    );

    let changed = crate::compiler::distributed::StagedExecutionResult::from_test_frames(
      &candidate,
      dep_info.as_bytes(),
      b"metadata bytes",
      b"rlib bytes",
      b"",
      b"",
    )?;
    let metadata = changed
      .frame(crate::compiler::distributed::DistributedResultSlot::Metadata)
      .ok_or_else(|| RailError::message("test metadata frame was unavailable"))?;
    fs::write(metadata, b"changed after decode")?;
    assert!(
      prepare_distributed_result(
        &session,
        &capture,
        &base_action,
        &observation,
        &output_paths,
        changed,
        &workspace_root,
        &workspace_root,
      )
      .is_err()
    );
    Ok(())
  }

  #[test]
  fn fast_bypass_only_rejects_shapes_outside_the_graduated_class() {
    let eligible = [
      "--crate-name",
      "fixture",
      "--crate-type=lib",
      "--emit=dep-info,metadata",
      "--error-format=json",
      "--out-dir",
      "target/debug/deps",
      "src/lib.rs",
    ]
    .map(OsString::from);
    assert_eq!(fast_bypass_reason(OsStr::new("rustc"), &eligible), None);
    let default_diagnostics = eligible
      .iter()
      .filter(|argument| argument.to_str() != Some("--error-format=json"))
      .cloned()
      .collect::<Vec<_>>();
    assert_eq!(fast_bypass_reason(OsStr::new("rustc"), &default_diagnostics), None);
    for dependency in [
      ["--extern", "dep=target/debug/deps/libdep.rmeta"],
      ["--extern=noprelude:dep=target/debug/deps/libdep.rlib", ""],
    ] {
      let mut supported = eligible.to_vec();
      supported.push(dependency[0].into());
      if !dependency[1].is_empty() {
        supported.push(dependency[1].into());
      }
      assert_eq!(fast_bypass_reason(OsStr::new("rustc"), &supported), None);
    }

    #[cfg(target_os = "macos")]
    {
      let pathless_proc_macro = [
        "--crate-name",
        "fixture_macros",
        "--crate-type=proc-macro",
        "--emit=dep-info,link",
        "--error-format=json",
        "--out-dir",
        "target/release/deps",
        "--extern",
        "proc_macro",
        "src/lib.rs",
      ]
      .map(OsString::from);
      assert_eq!(fast_bypass_reason(OsStr::new("rustc"), &pathless_proc_macro), None);

      let metadata_only_proc_macro = pathless_proc_macro
        .iter()
        .map(|argument| {
          if argument == "--emit=dep-info,link" {
            OsString::from("--emit=dep-info,metadata")
          } else {
            argument.clone()
          }
        })
        .collect::<Vec<_>>();
      assert_eq!(fast_bypass_reason(OsStr::new("rustc"), &metadata_only_proc_macro), None);

      let mut unowned_pathless_extern = pathless_proc_macro.to_vec();
      let pathless = unowned_pathless_extern
        .iter_mut()
        .find(|argument| argument.as_os_str() == OsStr::new("proc_macro"))
        .expect("pathless proc-macro argument");
      *pathless = OsString::from("dependency_without_path");
      assert_eq!(
        fast_bypass_reason(OsStr::new("rustc"), &unowned_pathless_extern),
        Some("dependency_artifact_path_unavailable")
      );
    }

    for (dependency, reason) in [
      (
        ["--extern", "derive=target/debug/deps/libderive.dylib"],
        "dynamic_dependency_execution_observation_unavailable",
      ),
      (
        ["--extern=derive=target/debug/deps/libderive.wasm", ""],
        "dependency_artifact_format_observation_unavailable",
      ),
    ] {
      let mut unsupported = eligible.to_vec();
      unsupported.push(dependency[0].into());
      if !dependency[1].is_empty() {
        unsupported.push(dependency[1].into());
      }
      assert_eq!(fast_bypass_reason(OsStr::new("rustc"), &unsupported), Some(reason));
    }
    let mut missing_dependency_path = eligible.to_vec();
    missing_dependency_path.extend([OsString::from("--extern"), OsString::from("derive")]);
    assert_eq!(
      fast_bypass_reason(OsStr::new("rustc"), &missing_dependency_path),
      Some("dependency_artifact_path_unavailable")
    );

    let test = [
      "--crate-name",
      "fixture_test",
      "--test",
      "--emit=dep-info,link",
      "--error-format=json",
      "--out-dir",
      "target/debug/deps",
      "tests/fixture.rs",
    ]
    .map(OsString::from);
    assert_eq!(
      fast_bypass_reason(OsStr::new("rustc"), &test),
      platform_linker_bypass_reason(std::env::consts::OS)
    );

    for (argument, reason) in [
      (
        "-Cincremental=target/incremental",
        "incremental_work_product_observation_unavailable",
      ),
      ("-Clinker=/tmp/linker", "explicit_linker_evidence_unavailable"),
      ("-Clink-arg=-dead_strip", "explicit_link_argument_evidence_unavailable"),
      ("@rustc.rsp", "response_file_expansion_unavailable"),
    ] {
      let mut unsupported = eligible.to_vec();
      unsupported.push(argument.into());
      assert_eq!(fast_bypass_reason(OsStr::new("rustc"), &unsupported), Some(reason));
    }
    assert_eq!(
      fast_bypass_reason(OsStr::new("clippy-driver"), &eligible),
      Some("clippy_diagnostic_result_authority_unavailable")
    );
  }

  #[test]
  fn linked_platforms_have_exact_provider_boundaries() {
    assert_eq!(platform_linker_bypass_reason("macos"), None);
    assert_eq!(platform_linker_bypass_reason("linux"), None);
    assert_eq!(
      platform_linker_bypass_reason("windows"),
      Some("coff_linker_evidence_unavailable")
    );
    assert_eq!(
      platform_linker_bypass_reason("unsupported"),
      Some("platform_linker_evidence_unavailable")
    );
  }

  fn graduated_session(source_root_identity: String) -> NativeCompilerSession {
    let class = NativeCompilerClass {
      name: "exact_rustc_result".to_string(),
      platform: "unix-test-x86_64".to_string(),
      host_target: "x86_64-unknown-test".to_string(),
      rustc_release: "1.97.1".to_string(),
    };
    let capability_identity = digest(b"toolchain-capability");
    let compiler_process_environment_identity = digest(b"compiler-process-environment");
    let execution_contract = DIAGNOSTIC_EXECUTION_CONTRACT.to_string();
    let identity = session_identity(
      &class,
      &source_root_identity,
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
      generated: None,
      native_searches: Vec::new(),
      pathless_extern_searches: Vec::new(),
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
      version: 5,
      complete: true,
      source_paths: vec!["lib.rs".to_string()],
      generated_paths: Vec::new(),
      dependency_names,
      environment_names: observation
        .environment_reads
        .iter()
        .map(|entry| entry.name.clone())
        .collect(),
      linker: None,
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
        file_name: "fixture-0123456789abcdef.d".to_string(),
        content_digest: observation.emitted_outputs[0].content_digest.clone(),
        bytes: 8,
        mode: 0o644,
      },
      NativeCompilerOutput {
        role: "metadata".to_string(),
        slot: METADATA_SLOT.to_string(),
        file_name: "libfixture-0123456789abcdef.rmeta".to_string(),
        content_digest: observation.emitted_outputs[1].content_digest.clone(),
        bytes: 8,
        mode: 0o644,
      },
    ];
    NativeCompilerValidation::new(
      &session,
      observation,
      &capture.approved_environment,
      None,
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

  #[test]
  fn zero_byte_metadata_is_an_exact_compiler_output() {
    let mut validation = cas_validation_with_stdout(b"");
    let empty_digest = digest(b"");
    validation.outputs[1].bytes = 0;
    validation.outputs[1].content_digest = empty_digest.clone();
    validation.observation.emitted_outputs[1].content_digest = empty_digest;
    validation.result_key = result_key(
      &validation.action_key,
      &validation.witness,
      &validation.outputs,
      &validation.stdout_digest,
      validation.stdout_bytes,
      &validation.stderr_digest,
      validation.stderr_bytes,
    )
    .expect("zero-byte metadata result identity");
    validation
      .validate_object()
      .expect("rustc may intentionally emit an empty metadata artifact");

    validation.outputs[0].bytes = 0;
    validation.observation.emitted_outputs[0].content_digest = digest(b"");
    validation.outputs[0].content_digest = digest(b"");
    validation
      .validate_object()
      .expect_err("dep-info must still contain an authoritative dependency graph");
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
        crate::cache::cas::NativeEnvironmentSelectorPublication::Created
      );
      assert_eq!(
        cas.native_environment_selector(&key).expect("published selector"),
        Some(first)
      );
      assert_eq!(
        cas
          .publish_native_environment_selector(&key, &second)
          .expect("divergent selector publication"),
        crate::cache::cas::NativeEnvironmentSelectorPublication::Diverged
      );
      cas
        .native_environment_selector(&key)
        .expect_err("an empty/nonempty selector change must fail closed");
    }
  }

  pub(crate) fn prepared_cas_fixture(validation: NativeCompilerValidation) -> PreparedNativeResult {
    let staging = tempfile::tempdir().expect("native result staging");
    for directory in ["target", "target/outputs", "target/streams"] {
      let path = staging.path().join(directory);
      fs::create_dir(&path).expect("slot directory");
      #[cfg(unix)]
      set_native_output_mode(&path, 0o755).expect("slot directory mode");
    }
    for (slot, bytes) in [
      (DEP_INFO_SLOT, b"dep-info".as_slice()),
      (METADATA_SLOT, b"metadata".as_slice()),
      (STDOUT_SLOT, b"portable stdout".as_slice()),
      (STDERR_SLOT, b"".as_slice()),
    ] {
      let path = staging.path().join(slot);
      fs::write(&path, bytes).expect("slot bytes");
      set_native_output_mode(&path, 0o644).expect("slot mode");
    }
    let paths = [DEP_INFO_SLOT, METADATA_SLOT, STDOUT_SLOT, STDERR_SLOT]
      .into_iter()
      .map(|slot| staging.path().join(slot))
      .collect::<Vec<_>>();
    let manifest =
      crate::cache::result::capture_native_compiler_outputs(staging.path(), &paths).expect("native result manifest");
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
          crate::cache::cas::NativeActionLookup::Miss(_)
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
          crate::cache::cas::NativeEnvironmentSelectorPublication::Created
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
      crate::cache::cas::NativeActionLookup::Hit(_)
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
          crate::cache::cas::NativeEnvironmentSelectorPublication::Created
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
      crate::cache::cas::NativeActionLookup::Miss(_)
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
          crate::cache::cas::NativeEnvironmentSelectorPublication::Diverged
        );
      }
      let crate::cache::cas::NativeActionLookup::Hit(hit) = cas.native_action(&action).expect("native action lookup")
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
    let mut moved_capture = capture.clone();
    moved_capture.source_root = PathBuf::from("/moved/workspace/src");
    moved_capture.source_root_spelling = moved_capture.source_root.clone();
    assert_ne!(
      action,
      action_key(&session.identity, &session.class, &base, &moved_capture).expect("moved-root action"),
      "physical compilation roots must partition exact compiler artifacts"
    );
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

  fn attach_generated_capture(capture: &mut NativeActionCapture, root: &Path, generated_root: &Path) {
    let mut budget = NativeCaptureBudget::new(NATIVE_CAPTURE_LIMITS);
    let generated = capture_native_generated_namespace_from(
      Some(generated_root.as_os_str().to_os_string()),
      &[],
      root,
      Instant::now(),
      &mut budget,
    )
    .expect("generated capture")
    .expect("generated namespace");
    capture.generated = Some(generated);
    capture.bytes_hashed = capture.bytes_hashed.saturating_add(budget.bytes_hashed);
  }

  #[test]
  fn generated_namespace_content_is_part_of_the_action_and_witness() {
    let root = tempfile::tempdir().expect("source root");
    fs::create_dir(root.path().join("src")).expect("source directory");
    let source = root.path().join("src/lib.rs");
    fs::write(&source, b"include!(concat!(env!(\"OUT_DIR\"), \"/generated.rs\"));\n").expect("source");
    let generated_root = root.path().join("target/out");
    fs::create_dir_all(&generated_root).expect("generated directory");
    let generated = generated_root.join("generated.rs");
    fs::write(&generated, b"pub const VALUE: u8 = 1;\n").expect("generated source");

    let source_observation = FileObservation::capture(&source, root.path(), root.path()).expect("source observation");
    let generated_observation =
      FileObservation::capture(&generated, root.path(), root.path()).expect("generated observation");
    let mut observation = graduated_observation();
    observation.declared_inputs = vec![source_observation.clone()];
    observation.observed_reads = vec![source_observation, generated_observation];
    let session = graduated_session(path_identity(root.path()).expect("root identity"));
    let mut initial = NativeActionCapture::capture(&observation, root.path()).expect("initial source capture");
    attach_generated_capture(&mut initial, root.path(), &generated_root);
    let initial_action = action_key(&session.identity, &session.class, &observation, &initial).expect("initial action");
    let witness = initial.witness(&observation, root.path()).expect("complete witness");
    assert_eq!(witness.source_paths, ["lib.rs"]);
    assert_eq!(witness.generated_paths, ["generated.rs"]);
    assert!(initial.validates_witness(&witness, &observation));

    fs::write(&generated, b"pub const VALUE: u8 = 2;\n").expect("changed generated source");
    let mut changed = NativeActionCapture::capture(&observation, root.path()).expect("changed source capture");
    attach_generated_capture(&mut changed, root.path(), &generated_root);
    assert_ne!(
      action_key(&session.identity, &session.class, &observation, &changed).expect("changed action"),
      initial_action
    );
  }

  #[cfg(any(unix, windows))]
  #[test]
  fn generated_restore_revalidation_rejects_a_same_size_x_to_y_to_x_mutation() {
    let root = tempfile::tempdir().expect("source root");
    fs::create_dir(root.path().join("src")).expect("source directory");
    let source = root.path().join("src/lib.rs");
    fs::write(&source, b"pub const SOURCE: u8 = 1;\n").expect("source");
    let generated_root = root.path().join("target/out");
    fs::create_dir_all(&generated_root).expect("generated directory");
    let generated = generated_root.join("generated.rs");
    let original = b"pub const VALUE: u8 = 1;\n";
    fs::write(&generated, original).expect("generated source");
    let original_modified = fs::metadata(&generated)
      .and_then(|metadata| metadata.modified())
      .expect("generated mtime");

    let source_observation = FileObservation::capture(&source, root.path(), root.path()).expect("source observation");
    let mut observation = graduated_observation();
    observation.declared_inputs = vec![source_observation.clone()];
    observation.observed_reads = vec![source_observation];
    let mut capture = NativeActionCapture::capture(&observation, root.path()).expect("source capture");
    attach_generated_capture(&mut capture, root.path(), &generated_root);
    capture
      .revalidate_generated_before_restore_commit(root.path(), Some(generated_root.as_os_str().to_os_string()))
      .expect("unchanged generated capture");

    fs::write(&generated, b"pub const VALUE: u8 = 2;\n").expect("same-size mutation");
    fs::write(&generated, original).expect("restored generated bytes");
    File::options()
      .write(true)
      .open(&generated)
      .and_then(|file| file.set_times(fs::FileTimes::new().set_modified(original_modified)))
      .expect("restore generated mtime");

    let error = capture
      .revalidate_generated_before_restore_commit(root.path(), Some(generated_root.as_os_str().to_os_string()))
      .expect_err("a restored generated size, digest, and mtime must not erase the generation change");
    assert!(error.to_string().contains("generated input changed"), "{error}");
  }

  #[cfg(unix)]
  #[test]
  fn generated_capture_rejects_a_symlinked_out_dir() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("source root");
    let generated_root = root.path().join("real-out");
    fs::create_dir(&generated_root).expect("generated directory");
    let selected = root.path().join("selected-out");
    symlink(&generated_root, &selected).expect("generated symlink");
    let mut budget = NativeCaptureBudget::new(NATIVE_CAPTURE_LIMITS);
    let error = capture_native_generated_namespace_from(
      Some(selected.as_os_str().to_os_string()),
      &[],
      root.path(),
      Instant::now(),
      &mut budget,
    )
    .expect_err("a symlinked OUT_DIR must fail closed");
    assert!(error.to_string().contains("not a real directory"), "{error}");
  }

  #[test]
  fn absent_out_dir_is_not_a_generated_input_capability() {
    let root = tempfile::tempdir().expect("source root");
    let mut budget = NativeCaptureBudget::new(NATIVE_CAPTURE_LIMITS);
    assert!(
      capture_native_generated_namespace_from(
        Some(root.path().join("absent-out").into_os_string()),
        &[],
        root.path(),
        Instant::now(),
        &mut budget,
      )
      .expect("absent OUT_DIR classification")
      .is_none()
    );
    assert_eq!(budget.entries, 0);
    assert_eq!(budget.bytes_hashed, 0);
  }

  #[test]
  fn compiler_owned_output_subtree_is_not_a_generated_input() {
    let root = tempfile::tempdir().expect("source root");
    fs::create_dir(root.path().join("src")).expect("source directory");
    let source = root.path().join("src/lib.rs");
    fs::write(&source, b"pub const VALUE: u8 = 1;\n").expect("source");
    let generated_root = root.path().join("target/out");
    let output_root = generated_root.join("probe");
    fs::create_dir_all(&output_root).expect("compiler output directory");
    fs::write(generated_root.join("generated.rs"), b"pub const GENERATED: u8 = 1;\n").expect("generated input");
    let compiler_output = output_root.join("libprobe.rmeta");
    fs::write(&compiler_output, b"output-one").expect("stale compiler output");

    let mut budget = NativeCaptureBudget::new(NATIVE_CAPTURE_LIMITS);
    let generated = capture_native_generated_namespace_from(
      Some(generated_root.as_os_str().to_os_string()),
      &[crate::utils::canonicalize_existing(&output_root).expect("canonical output")],
      root.path(),
      Instant::now(),
      &mut budget,
    )
    .expect("generated capture")
    .expect("generated namespace");
    assert!(generated.state.entries.iter().any(|entry| entry.path == "generated.rs"));
    assert!(
      generated
        .state
        .entries
        .iter()
        .all(|entry| !entry.path.starts_with("probe"))
    );

    let source_observation = FileObservation::capture(&source, root.path(), root.path()).expect("source observation");
    let output_observation =
      FileObservation::capture(&compiler_output, root.path(), root.path()).expect("output observation");
    let mut observation = graduated_observation();
    observation.declared_inputs = vec![source_observation.clone()];
    observation.observed_reads = vec![source_observation, output_observation];
    let mut capture = NativeActionCapture::capture(&observation, root.path()).expect("source capture");
    capture.generated = Some(generated);
    assert!(
      capture.witness(&observation, root.path()).is_err(),
      "a compiler read from its excluded output subtree must not be admitted"
    );

    fs::write(&compiler_output, b"output-two").expect("compiler output mutation");
    capture
      .revalidate_generated_before_restore_commit(root.path(), Some(generated_root.as_os_str().to_os_string()))
      .expect("compiler-owned output mutation must not alter generated inputs");
  }

  #[test]
  fn standard_target_root_is_not_build_script_source_authority() {
    let root = tempfile::tempdir().expect("source root");
    let source = root.path().join("build.rs");
    fs::write(&source, b"fn main() {}\n").expect("build script");
    fs::write(root.path().join("Cargo.toml"), b"[package]\nname = \"fixture\"\n").expect("manifest");
    fs::write(root.path().join("owned.txt"), b"owned input").expect("owned input");
    let target = root.path().join("target/release/build/fixture");
    fs::create_dir_all(&target).expect("standard target root");
    let compiler_output = target.join("build-script-build");
    fs::write(&compiler_output, b"first output").expect("compiler output");

    let source_observation = FileObservation::capture(&source, root.path(), root.path()).expect("source observation");
    let mut observation = graduated_observation();
    observation.declared_inputs = vec![source_observation.clone()];
    observation.observed_reads = vec![source_observation];
    let initial = NativeActionCapture::capture(&observation, root.path()).expect("initial build-script capture");
    validate_native_source_state(&initial.source_state).expect("workspace-root source-state capability");
    assert!(
      initial
        .source_state
        .entries
        .iter()
        .any(|entry| entry.path == "Cargo.toml")
    );
    assert!(
      initial
        .source_state
        .entries
        .iter()
        .any(|entry| entry.path == "owned.txt")
    );
    assert!(
      initial
        .source_state
        .entries
        .iter()
        .all(|entry| entry.path != "target" && !entry.path.starts_with("target/"))
    );

    fs::write(&compiler_output, b"second output").expect("mutated compiler output");
    let recaptured = NativeActionCapture::capture(&observation, root.path()).expect("recaptured build-script source");
    assert!(recaptured.unchanged_from(&initial));
  }

  fn native_static_observation(root: &Path, native_root: &Path) -> RawCompilerInvocation {
    let source = root.join("src/lib.rs");
    let source_observation = FileObservation::capture(&source, root, root).expect("source observation");
    let mut observation = graduated_observation();
    observation.declared_inputs = vec![source_observation.clone()];
    observation.observed_reads = vec![source_observation];
    observation.compiler_arguments.extend([
      "-L".to_string(),
      format!("native={}", native_root.display()),
      "-l".to_string(),
      "static=fixture_native".to_string(),
    ]);
    observation
  }

  #[test]
  fn native_search_namespace_content_is_part_of_the_action() {
    let root = tempfile::tempdir().expect("source root");
    fs::create_dir(root.path().join("src")).expect("source directory");
    fs::write(root.path().join("src/lib.rs"), b"pub const VALUE: u8 = 1;\n").expect("source");
    let native_root = root.path().join("target/native");
    fs::create_dir_all(&native_root).expect("native directory");
    let archive = native_root.join("libfixture_native.a");
    fs::write(&archive, b"archive-one").expect("native archive");

    let observation = native_static_observation(root.path(), &native_root);
    let session = graduated_session(path_identity(root.path()).expect("root identity"));
    let initial = NativeActionCapture::capture(&observation, root.path()).expect("initial capture");
    assert_eq!(initial.native_searches.len(), 1);
    let initial_action = action_key(&session.identity, &session.class, &observation, &initial).expect("initial action");
    let witness = initial.witness(&observation, root.path()).expect("complete witness");
    assert!(initial.validates_witness(&witness, &observation));

    fs::write(&archive, b"archive-two").expect("same-size native archive mutation");
    let changed = NativeActionCapture::capture(&observation, root.path()).expect("changed capture");
    assert_ne!(
      action_key(&session.identity, &session.class, &observation, &changed).expect("changed action"),
      initial_action
    );
  }

  #[cfg(target_os = "macos")]
  #[test]
  fn pathless_proc_macro_extern_binds_ordered_search_candidates() {
    let root = tempfile::tempdir().expect("source root");
    fs::create_dir(root.path().join("src")).expect("source directory");
    let source = root.path().join("src/lib.rs");
    fs::write(
      &source,
      b"extern crate proc_macro;\n#[proc_macro]\npub fn fixture(input: proc_macro::TokenStream) -> proc_macro::TokenStream { input }\n",
    )
    .expect("proc-macro source");
    let dependency_root = root.path().join("target/release/deps");
    fs::create_dir_all(&dependency_root).expect("dependency search directory");
    fs::write(dependency_root.join("libproc_macro-known.rlib"), b"known candidate").expect("known candidate");

    let source_observation = FileObservation::capture(&source, root.path(), root.path()).expect("source observation");
    let mut observation = graduated_observation();
    observation.crate_name = Some("fixture_macros".to_string());
    observation.crate_types = BTreeSet::from(["proc-macro".to_string()]);
    observation.emit_modes = BTreeSet::from(["dep-info".to_string(), "link".to_string()]);
    observation.compiler_arguments = [
      "--crate-name".to_string(),
      "fixture_macros".to_string(),
      "--crate-type=proc-macro".to_string(),
      "--emit=dep-info,link".to_string(),
      "--error-format=json".to_string(),
      "--out-dir".to_string(),
      dependency_root.to_string_lossy().into_owned(),
      "-L".to_string(),
      format!("dependency={}", dependency_root.display()),
      "--extern".to_string(),
      "proc_macro".to_string(),
      source.to_string_lossy().into_owned(),
    ]
    .to_vec();
    observation.declared_inputs = vec![source_observation.clone()];
    observation.observed_reads = vec![source_observation];
    observation.emitted_outputs.clear();
    assert_eq!(
      invocation_bypass_reason(
        &observation,
        false,
        &graduated_session(digest(b"source-root")).class.host_target
      ),
      None
    );

    let session = graduated_session(path_identity(root.path()).expect("root identity"));
    let initial = NativeActionCapture::capture(&observation, root.path()).expect("initial capture");
    assert_eq!(initial.pathless_extern_searches.len(), 1);
    assert_eq!(
      initial.pathless_extern_searches[0]
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect::<Vec<_>>(),
      ["libproc_macro-known.rlib"]
    );
    let initial_action = action_key(&session.identity, &session.class, &observation, &initial).expect("initial action");
    initial
      .revalidate_pathless_extern_searches_before_restore_commit(&observation, root.path())
      .expect("unchanged candidate set");

    fs::write(dependency_root.join("libproc_macro-shadow.rlib"), b"shadow candidate").expect("shadow candidate");
    let error = initial
      .revalidate_pathless_extern_searches_before_restore_commit(&observation, root.path())
      .expect_err("an earlier pathless extern candidate must invalidate restore");
    assert!(error.to_string().contains("candidates changed"), "{error}");
    let changed = NativeActionCapture::capture(&observation, root.path()).expect("changed capture");
    assert_ne!(
      action_key(&session.identity, &session.class, &observation, &changed).expect("changed action"),
      initial_action
    );
  }

  #[test]
  fn dependency_search_changes_do_not_create_a_pathless_extern_witness() {
    let root = tempfile::tempdir().expect("source root");
    fs::create_dir(root.path().join("src")).expect("source directory");
    let source = root.path().join("src/lib.rs");
    fs::write(&source, b"pub fn value() -> u8 { 1 }\n").expect("source");
    let dependency_root = root.path().join("target/debug/deps");
    fs::create_dir_all(&dependency_root).expect("dependency search directory");

    let source_observation = FileObservation::capture(&source, root.path(), root.path()).expect("source observation");
    let mut observation = graduated_observation();
    observation
      .compiler_arguments
      .extend(["-L".to_string(), format!("dependency={}", dependency_root.display())]);
    observation.declared_inputs = vec![source_observation.clone()];
    observation.observed_reads = vec![source_observation];

    let capture = NativeActionCapture::capture(&observation, root.path()).expect("ordinary library capture");
    assert!(capture.pathless_extern_searches.is_empty());
    capture
      .revalidate_pathless_extern_searches_before_restore_commit(&observation, root.path())
      .expect("an ordinary dependency search is not a pathless extern witness");

    fs::write(
      dependency_root.join("libproc_macro-shadow.rlib"),
      b"unrelated candidate",
    )
    .expect("unrelated candidate");
    capture
      .revalidate_pathless_extern_searches_before_restore_commit(&observation, root.path())
      .expect("an unrelated candidate cannot invalidate an ordinary action");
  }

  #[cfg(any(unix, windows))]
  #[test]
  fn native_search_restore_revalidation_rejects_a_same_size_x_to_y_to_x_mutation() {
    let root = tempfile::tempdir().expect("source root");
    fs::create_dir(root.path().join("src")).expect("source directory");
    fs::write(root.path().join("src/lib.rs"), b"pub const VALUE: u8 = 1;\n").expect("source");
    let native_root = root.path().join("target/native");
    fs::create_dir_all(&native_root).expect("native directory");
    let archive = native_root.join("libfixture_native.a");
    let original = b"archive-one";
    fs::write(&archive, original).expect("native archive");
    let original_modified = fs::metadata(&archive)
      .and_then(|metadata| metadata.modified())
      .expect("archive mtime");

    let observation = native_static_observation(root.path(), &native_root);
    let capture = NativeActionCapture::capture(&observation, root.path()).expect("native capture");
    capture
      .revalidate_before_restore_commit(&observation, root.path(), root.path())
      .expect("unchanged native capture");

    fs::write(&archive, b"archive-two").expect("same-size native mutation");
    fs::write(&archive, original).expect("restored archive bytes");
    File::options()
      .write(true)
      .open(&archive)
      .and_then(|file| file.set_times(fs::FileTimes::new().set_modified(original_modified)))
      .expect("restore archive mtime");

    let error = capture
      .revalidate_before_restore_commit(&observation, root.path(), root.path())
      .expect_err("a restored native size, digest, and mtime must not erase the generation change");
    assert!(
      error.to_string().contains("native library search input changed"),
      "{error}"
    );
  }

  #[test]
  fn native_search_capture_deduplicates_the_generated_namespace() {
    let root = tempfile::tempdir().expect("source root");
    let generated_root = root.path().join("target/out");
    fs::create_dir_all(&generated_root).expect("generated directory");
    fs::write(generated_root.join("libfixture_native.a"), b"archive").expect("native archive");
    let mut budget = NativeCaptureBudget::new(NATIVE_CAPTURE_LIMITS);
    let generated = capture_native_generated_namespace_from(
      Some(generated_root.as_os_str().to_os_string()),
      &[],
      root.path(),
      Instant::now(),
      &mut budget,
    )
    .expect("generated capture")
    .expect("generated namespace");
    let bytes_after_generated = budget.bytes_hashed;
    let searches = capture_native_search_namespaces(
      &["-L".to_string(), format!("native={}", generated_root.display())],
      Some(&generated),
      root.path(),
      Instant::now(),
      &mut budget,
    )
    .expect("native search capture");
    assert!(searches.is_empty());
    assert_eq!(budget.bytes_hashed, bytes_after_generated);
  }

  #[test]
  fn native_search_paths_resolve_only_valid_recorder_repository_capabilities() {
    let root = Path::new("/workspace");
    assert_eq!(
      native_search_paths(
        &[
          "-L".to_string(),
          "native=repository:/target/native".to_string(),
          "-Ldependency=repository:/target/deps".to_string(),
        ],
        Path::new("/workspace/package"),
        root,
      )
      .expect("portable native search paths"),
      [PathBuf::from("/workspace/target/native")]
    );
    assert!(
      native_search_paths(
        &["-Lnative=repository:/../outside".to_string()],
        Path::new("/workspace/package"),
        root,
      )
      .is_err()
    );
  }

  #[cfg(unix)]
  #[test]
  fn native_search_capture_rejects_a_symlinked_namespace() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("source root");
    let native_root = root.path().join("real-native");
    fs::create_dir(&native_root).expect("native directory");
    let selected = root.path().join("selected-native");
    symlink(&native_root, &selected).expect("native symlink");
    let mut budget = NativeCaptureBudget::new(NATIVE_CAPTURE_LIMITS);
    let error = capture_native_search_namespaces(
      &["-L".to_string(), format!("native={}", selected.display())],
      None,
      root.path(),
      Instant::now(),
      &mut budget,
    )
    .expect_err("a symlinked native search namespace must fail closed");
    assert!(error.to_string().contains("not a real directory"), "{error}");
  }

  #[cfg(any(unix, windows))]
  #[test]
  fn restore_revalidation_rejects_a_same_size_x_to_y_to_x_mutation() {
    let root = tempfile::tempdir().expect("source root");
    fs::create_dir(root.path().join("src")).expect("source directory");
    let source = root.path().join("src/lib.rs");
    let original = b"pub const VALUE: u8 = 1;\n";
    fs::write(&source, original).expect("source");
    let original_modified = fs::metadata(&source)
      .and_then(|metadata| metadata.modified())
      .expect("source mtime");
    let source_observation = FileObservation::capture(&source, root.path(), root.path()).expect("source observation");
    let mut observation = graduated_observation();
    observation.declared_inputs = vec![source_observation.clone()];
    observation.observed_reads = vec![source_observation];
    let initial = NativeActionCapture::capture(&observation, root.path()).expect("initial capture");
    initial
      .revalidate_before_restore_commit(&observation, root.path(), root.path())
      .expect("unchanged capture");

    fs::write(&source, b"pub const VALUE: u8 = 2;\n").expect("same-size mutation");
    fs::write(&source, original).expect("restored source bytes");
    File::options()
      .write(true)
      .open(&source)
      .and_then(|file| file.set_times(fs::FileTimes::new().set_modified(original_modified)))
      .expect("restore source mtime");

    let error = initial
      .revalidate_before_restore_commit(&observation, root.path(), root.path())
      .expect_err("a restored size, digest, and mtime must not erase the generation change");
    assert!(error.to_string().contains("action input changed"), "{error}");
  }

  #[test]
  fn restore_revalidation_rejects_a_transient_namespace_entry() {
    let root = tempfile::tempdir().expect("source root");
    fs::create_dir(root.path().join("src")).expect("source directory");
    let source = root.path().join("src/lib.rs");
    fs::write(&source, b"pub const VALUE: u8 = 1;\n").expect("source");
    let source_observation = FileObservation::capture(&source, root.path(), root.path()).expect("source observation");
    let mut observation = graduated_observation();
    observation.declared_inputs = vec![source_observation.clone()];
    observation.observed_reads = vec![source_observation];
    let initial = NativeActionCapture::capture(&observation, root.path()).expect("initial capture");

    let transient = root.path().join("src/transient.rs");
    fs::write(&transient, b"transient\n").expect("transient source");
    fs::remove_file(&transient).expect("remove transient source");

    let error = initial
      .revalidate_before_restore_commit(&observation, root.path(), root.path())
      .expect_err("a transient namespace member must alter its parent generation");
    assert!(error.to_string().contains("action input changed"), "{error}");
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
  fn only_certified_compiler_classes_are_graduated() {
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
    assert_bypass("rustdoc_output_tree_observation_unavailable", |value| {
      value.mode = CompilerMode::Rustdoc;
    });
    assert_bypass("doctest_execution_result_authority_unavailable", |value| {
      value.mode = CompilerMode::Rustdoc;
      value.test_mode = true;
    });
    assert_bypass("cross_target_toolchain_evidence_unavailable", |value| {
      value.target_argument = Some("x86_64-unknown-linux-gnu".to_string());
    });
    let mut test = baseline.clone();
    test.crate_name = Some("fixture_test".to_string());
    test.crate_types.clear();
    test.test_mode = true;
    test.emit_modes = BTreeSet::from(["dep-info".to_string(), "link".to_string()]);
    test.compiler_arguments = [
      "--crate-name",
      "fixture_test",
      "--edition=2024",
      "--error-format=json",
      "tests/fixture.rs",
      "--test",
      "--emit=dep-info,link",
      "-C",
      "metadata=0123456789abcdef",
      "-Cextra-filename=-0123456789abcdef",
      "--out-dir",
      "target/debug/deps",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    assert_eq!(
      invocation_bypass_reason(&test, true, &session.class.host_target),
      platform_linker_bypass_reason(std::env::consts::OS)
    );
    assert_eq!(
      linked_observation(&test),
      cfg!(any(target_os = "macos", target_os = "linux"))
    );
    for (crate_type, crate_name) in [
      ("proc-macro", "fixture_macros"),
      ("dylib", "fixture_dylib"),
      ("cdylib", "fixture_cdylib"),
      ("bin", "fixture"),
      ("bin", "build_script_build"),
    ] {
      let mut linked = baseline.clone();
      linked.crate_types = BTreeSet::from([crate_type.to_string()]);
      linked.crate_name = Some(crate_name.to_string());
      linked.emit_modes = BTreeSet::from(["dep-info".to_string(), "link".to_string()]);
      *linked
        .compiler_arguments
        .iter_mut()
        .find(|argument| *argument == "lib")
        .expect("crate type") = crate_type.to_string();
      *linked
        .compiler_arguments
        .iter_mut()
        .find(|argument| argument.starts_with("--emit="))
        .expect("emit modes") = "--emit=dep-info,link".to_string();
      assert_eq!(
        invocation_bypass_reason(&linked, true, &session.class.host_target),
        platform_linker_bypass_reason(std::env::consts::OS),
        "{crate_name}"
      );
    }
    let mut staticlib = baseline.clone();
    staticlib.crate_types = BTreeSet::from(["staticlib".to_string()]);
    staticlib.emit_modes = BTreeSet::from(["dep-info".to_string(), "link".to_string()]);
    assert_eq!(
      invocation_bypass_reason(&staticlib, true, &session.class.host_target),
      None,
      "rustc owns static archive creation"
    );
    let mut explicit_rlib = baseline.clone();
    explicit_rlib.crate_types = BTreeSet::from(["rlib".to_string()]);
    assert_eq!(
      invocation_bypass_reason(&explicit_rlib, true, &session.class.host_target),
      None
    );
    assert_bypass("compiler_emit_contract_unavailable", |value| {
      value.emit_modes.insert("llvm-bc".to_string());
    });
    assert_bypass("compiler_stdin_observation_unavailable", |value| {
      value.compiler_arguments.push("-".to_string());
    });
    let mut human_diagnostics = baseline.clone();
    *human_diagnostics
      .compiler_arguments
      .iter_mut()
      .find(|argument| argument.starts_with("--error-format="))
      .expect("diagnostic format") = "--error-format=human".to_string();
    assert_eq!(
      invocation_bypass_reason(&human_diagnostics, true, &session.class.host_target),
      None
    );
    assert_bypass("compiler_diagnostic_replay_unavailable", |value| {
      *value
        .compiler_arguments
        .iter_mut()
        .find(|argument| argument.starts_with("--error-format="))
        .expect("diagnostic format") = "--error-format=future".to_string();
    });
    let mut native_static = baseline.clone();
    native_static.compiler_arguments.extend([
      "-L".to_string(),
      "native=/tmp".to_string(),
      "-l".to_string(),
      "static=fixture".to_string(),
    ]);
    assert_eq!(
      invocation_bypass_reason(&native_static, true, &session.class.host_target),
      None
    );
    let mut thin_lto = baseline.clone();
    thin_lto
      .compiler_arguments
      .extend(["-C".to_string(), "lto=thin".to_string()]);
    assert_eq!(
      invocation_bypass_reason(&thin_lto, true, &session.class.host_target),
      None,
      "the exact compiler arguments and linked witness bind LTO mode"
    );
    let mut prefer_dynamic = baseline.clone();
    prefer_dynamic
      .compiler_arguments
      .extend(["-C".to_string(), "prefer-dynamic".to_string()]);
    assert_eq!(
      invocation_bypass_reason(&prefer_dynamic, true, &session.class.host_target),
      None,
      "the exact compiler arguments bind rustc's dynamic-link preference"
    );
    assert_bypass("dynamic_native_library_search_evidence_unavailable", |value| {
      value
        .compiler_arguments
        .extend(["-l".to_string(), "dylib=fixture".to_string()]);
    });
    assert_bypass("explicit_linker_evidence_unavailable", |value| {
      value
        .compiler_arguments
        .extend(["-C".to_string(), "linker=/tmp/linker".to_string()]);
    });
    assert_bypass("explicit_link_argument_evidence_unavailable", |value| {
      value.compiler_arguments.push("-Clink-arg=-dead_strip".to_string());
    });
    assert_bypass("incremental_work_product_observation_unavailable", |value| {
      value
        .compiler_arguments
        .extend(["-C".to_string(), "incremental=target/incremental".to_string()]);
    });
    assert_bypass("unstable_compiler_option_evidence_unavailable", |value| {
      value.compiler_arguments.push("-Zunproven".to_string());
    });
    assert_bypass("remapped_path_observation_unavailable", |value| {
      value
        .compiler_arguments
        .push("--remap-path-prefix=/workspace=/other".to_string());
    });
    assert_bypass("remapped_path_observation_unavailable", |value| {
      value.compiler_arguments.push("--remap-path-scope=all".to_string());
    });
    assert_bypass("external_codegen_backend_identity_unavailable", |value| {
      value
        .compiler_arguments
        .push("-Zcodegen-backend=/opt/backend.so".to_string());
    });
    assert_bypass("dynamic_dependency_execution_observation_unavailable", |value| {
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
    assert_bypass("declared_input_bytes_unavailable", |value| {
      value.bypasses.insert("declared_input_bytes_unavailable".to_string());
    });
    assert_bypass("dep_info_output_bytes_unavailable", |value| {
      value.bypasses.insert("dep_info_output_bytes_unavailable".to_string());
    });
    assert_bypass("dep_info_output_symlink_unavailable", |value| {
      value.bypasses.insert("dep_info_output_symlink_unavailable".to_string());
    });
    assert_bypass("compiler_emitted_output_bytes_unavailable", |value| {
      value.bypasses.insert("emitted_output_bytes_unavailable".to_string());
    });
    assert_bypass("compiler_emitted_output_symlink_unavailable", |value| {
      value.bypasses.insert("emitted_output_symlink_unavailable".to_string());
    });
    assert_bypass("declared_compiler_inputs_unavailable", |value| {
      value.declared_inputs.clear();
    });
    assert_bypass("compiler_observed_read_set_unavailable", |value| {
      value.observed_reads.clear();
    });
    assert_bypass("compiler_emitted_output_set_unavailable", |value| {
      value.emitted_outputs.pop();
    });
  }

  #[test]
  fn successful_default_diagnostic_library_result_is_graduated() {
    let mut observation = graduated_observation();
    observation
      .compiler_arguments
      .retain(|argument| !argument.starts_with("--error-format="));
    let session = graduated_session(digest(b"source-root"));
    assert_eq!(
      invocation_bypass_reason(&observation, true, &session.class.host_target),
      None
    );
    graduated_validation(observation)
      .validate_object()
      .expect("default-format result validation");
  }

  #[cfg(target_os = "linux")]
  #[test]
  fn elf_linker_witness_accepts_a_rustc_object_removed_after_linking() {
    let root = tempfile::tempdir().expect("link root");
    let current_directory = crate::utils::canonicalize_existing(root.path()).expect("canonical link root");
    let output = current_directory.join("fixture-bin");
    fs::write(&output, b"linked output").expect("linked output");
    let object = current_directory.join("fixture.fixture.abc-cgu.0.rcgu.o");
    fs::write(&object, b"object").expect("rustc object");
    let object = crate::utils::canonicalize_existing(&object).expect("canonical rustc object");
    fs::remove_file(&object).expect("rustc removed its codegen object");
    let rustc_temporary = current_directory.join("rustcABC123");
    fs::create_dir(&rustc_temporary).expect("rustc temporary directory");
    let response_list = rustc_temporary.join("list");
    fs::write(&response_list, b"object list").expect("rustc response list");
    let response_list = crate::utils::canonicalize_existing(&response_list).expect("canonical response list");
    fs::remove_dir_all(&rustc_temporary).expect("rustc removed its temporary directory");

    let dependencies = current_directory.join("link.d");
    fs::write(
      &dependencies,
      format!(
        "{}: {} {}\n",
        output.display(),
        object.display(),
        response_list.display()
      ),
    )
    .expect("link dependencies");
    let driver = crate::utils::canonicalize_existing(Path::new("/usr/bin/cc")).expect("system C driver");
    let linker = resolve_selected_elf_linker(&driver, &current_directory).expect("selected ELF linker");
    let evidence = ElfLinkDriverEvidence {
      version: 1,
      current_directory: current_directory.to_string_lossy().into_owned(),
      driver: driver.to_string_lossy().into_owned(),
      linker: linker.to_string_lossy().into_owned(),
      direct_inputs: vec![object.to_string_lossy().into_owned()],
      tool_inputs: Vec::new(),
      search_directories: Vec::new(),
    };
    let driver_inputs = current_directory.join("driver.json");
    fs::write(&driver_inputs, serde_json::to_vec(&evidence).expect("driver evidence")).expect("driver evidence file");
    let outputs = NativeOutputPaths {
      dep_info: current_directory.join("fixture-bin.d"),
      artifacts: vec![crate::compiler::observation::NativeOutputArtifact {
        role: NativeOutputRole::Executable,
        path: output,
      }],
    };

    let (witness, _, _) =
      capture_elf_linker_witness(&graduated_observation(), &outputs, &dependencies, &driver_inputs, None)
        .expect("ELF linker witness");
    assert_eq!(witness.endogenous_objects, 2);

    let external = tempfile::tempdir().expect("external link root");
    let external_rustc_temporary = external.path().join("rustcABC123");
    fs::create_dir(&external_rustc_temporary).expect("external rustc-shaped directory");
    let external_response_list = external_rustc_temporary.join("list");
    fs::write(&external_response_list, b"external object list").expect("external response list");
    let external_response_list =
      crate::utils::canonicalize_existing(&external_response_list).expect("canonical external response list");
    fs::remove_dir_all(&external_rustc_temporary).expect("remove external rustc-shaped directory");
    fs::write(
      &dependencies,
      format!(
        "{}: {}\n",
        outputs.artifacts[0].path.display(),
        external_response_list.display()
      ),
    )
    .expect("external link dependencies");
    let error = capture_elf_linker_witness(&graduated_observation(), &outputs, &dependencies, &driver_inputs, None)
      .expect_err("rustc-shaped input outside the linked output directory must remain exogenous");
    assert!(error.to_string().contains("is unavailable"));
  }

  #[cfg(target_os = "linux")]
  #[test]
  fn linked_output_roles_accept_elf_shared_objects() {
    assert!(output_role_path_matches("proc_macro", Path::new("libfixture.so")));
    assert!(output_role_path_matches("dylib", Path::new("libfixture.so")));
    assert!(output_role_path_matches("cdylib", Path::new("libfixture.so")));
    assert!(!output_role_path_matches("proc_macro", Path::new("libfixture.dylib")));
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
  fn publication_root_must_remain_inside_the_standard_target_root() {
    let source = tempfile::tempdir().expect("source root");
    let external = tempfile::tempdir().expect("external root");
    let internal = source.path().join("target/debug/deps");
    fs::create_dir_all(&internal).expect("internal target");
    let valid = metadata_output_paths(internal.join("fixture.d"), internal.join("libfixture.rmeta"));
    assert!(validated_output_parent(&valid, source.path()).is_ok());

    let source_output = source.path().join("generated");
    fs::create_dir(&source_output).expect("source output");
    let source_mutation =
      metadata_output_paths(source_output.join("fixture.d"), source_output.join("libfixture.rmeta"));
    assert!(validated_output_parent(&source_mutation, source.path()).is_err());

    let escaped = metadata_output_paths(
      external.path().join("fixture.d"),
      external.path().join("libfixture.rmeta"),
    );
    assert!(validated_output_parent(&escaped, source.path()).is_err());
  }

  #[cfg(any(unix, windows))]
  #[test]
  fn restore_commit_moves_the_verified_cas_copy_with_the_registered_identity() {
    let root = tempfile::tempdir().expect("restore root");
    let staging = root.path().join("private-staging");
    let output = root.path().join("target/debug/deps");
    fs::create_dir_all(&staging).expect("staging directory");
    fs::create_dir_all(&output).expect("output directory");
    let source = staging.join("libfixture.rmeta");
    let destination = output.join("libfixture.rmeta");
    let expected = NativeCompilerOutput {
      role: "metadata".to_string(),
      slot: METADATA_SLOT.to_string(),
      file_name: "libfixture.rmeta".to_string(),
      content_digest: digest(b"verified metadata"),
      bytes: 17,
      mode: 0o644,
    };
    write_new_file(&source, b"verified metadata", expected.mode, true).expect("verified CAS copy");

    let prepared = prepare_restore_output(&source, &destination, &expected, root.path()).expect("prepared restore");
    let before = prepared.source_identity.clone();
    let member = NativeRestoreMember::Output {
      source: source.to_str().expect("UTF-8 source").to_string(),
      destination: destination.to_str().expect("UTF-8 destination").to_string(),
      source_identity: before.clone(),
      previous_identity: None,
      content_digest: expected.content_digest.clone(),
    };
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
    let expected = NativeCompilerOutput {
      role: "metadata".to_string(),
      slot: METADATA_SLOT.to_string(),
      file_name: "libfixture.rmeta".to_string(),
      content_digest: digest(b"verified metadata"),
      bytes: 17,
      mode: 0o644,
    };
    write_new_file(&source, b"verified metadata", expected.mode, true).expect("verified CAS copy");
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
    let cache = tempfile::tempdir().expect("cache root");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let output = root.path().join("target/debug/deps");
    let observations = root.path().join("observations");
    fs::create_dir_all(&output).expect("output directory");
    fs::create_dir(&observations).expect("observation directory");
    let outputs = metadata_output_paths(output.join("fixture.d"), output.join("libfixture.rmeta"));
    let paths = restore_commit_paths(&outputs, root.path()).expect("restore paths");
    fs::create_dir(&paths.transaction_directory).expect("unregistered transaction");
    fs::write(paths.transaction_directory.join(RESTORE_REGISTRATION_FILE), b"{").expect("partial registration");
    recover_restore_commit_in(&cas, &outputs, root.path(), &observations).expect("partial registration recovery");
    assert!(!paths.transaction_directory.exists());

    let action_key = format!("{ACTION_KEY_PREFIX}{}", "a".repeat(64));
    let transaction = begin_restore_transaction_in(&cas, &outputs, root.path(), &observations, &action_key)
      .expect("registered transaction");
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

    recover_restore_commit_in(&cas, &outputs, root.path(), &observations).expect("partial pending-commit recovery");
    assert!(!transaction_directory.exists());
  }

  #[test]
  fn restore_transaction_rejects_an_rlib_for_a_metadata_only_action() {
    let root = tempfile::tempdir().expect("restore root");
    let cache = tempfile::tempdir().expect("cache root");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let output = root.path().join("target/debug/deps");
    let observations = root.path().join("observations");
    fs::create_dir_all(&output).expect("output directory");
    fs::create_dir(&observations).expect("observation directory");
    let outputs = metadata_output_paths(output.join("fixture.d"), output.join("libfixture.rmeta"));
    let action_key = format!("{ACTION_KEY_PREFIX}{}", "b".repeat(64));
    let mut transaction = begin_restore_transaction_in(&cas, &outputs, root.path(), &observations, &action_key)
      .expect("registered transaction");
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
        &session.source_root_identity,
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
      &session.source_root_identity,
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
  fn session_identity_and_session_file_are_root_bound() {
    let first = tempfile::tempdir().expect("first source root");
    let second = tempfile::tempdir().expect("second source root");
    let first_session = graduated_session(path_identity(first.path()).expect("first root identity"));
    let second_session = graduated_session(path_identity(second.path()).expect("second root identity"));
    assert_ne!(first_session.identity, second_session.identity);

    let session_file = first.path().join("session.json");
    fs::write(&session_file, serde_json::to_vec(&first_session).expect("session JSON")).expect("session file");
    NativeCompilerSession::load(&session_file, first.path()).expect("matching physical root");
    NativeCompilerSession::load(&session_file, second.path()).expect_err("replayed session file must fail closed");
  }

  #[test]
  fn cold_execution_preserves_the_exact_compiler_arguments() {
    let mut command = Command::new("rustc");
    let compiler_arguments = [OsString::from("src/lib.rs")];
    let observation = graduated_observation();
    let directory = tempfile::tempdir().expect("observation directory");
    prepare_observed_cold_child(
      &mut command,
      OsStr::new("rustc"),
      &compiler_arguments,
      false,
      &observation,
      directory.path(),
    );
    let arguments = command.get_args().collect::<Vec<_>>();
    assert_eq!(arguments, [OsStr::new("src/lib.rs")]);
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
      file_name: match role {
        "dep_info" => "fixture-0123456789abcdef.d",
        "metadata" => "libfixture-0123456789abcdef.rmeta",
        "rlib" => "libfixture-0123456789abcdef.rlib",
        _ => unreachable!(),
      }
      .to_string(),
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
      None,
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
    let original_outputs = metadata_output_paths(
      original_directory.join("fixture-0123456789abcdef.d"),
      original_directory.join("libfixture-0123456789abcdef.rmeta"),
    );
    let output_directory = source_root.path().join("build-two/debug/deps");
    fs::create_dir_all(&output_directory).expect("current output directory");
    let outputs = metadata_output_paths(
      output_directory.join("fixture-0123456789abcdef.d"),
      output_directory.join("libfixture-0123456789abcdef.rmeta"),
    );
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
      metadata_output_paths(
        directory.join("fixture-0123456789abcdef.d"),
        directory.join("libfixture-0123456789abcdef.rmeta"),
      )
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
    let outputs = metadata_output_paths(
      directory.join("fixture-0123456789abcdef.d"),
      directory.join("libfixture-0123456789abcdef.rmeta"),
    );
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
    let outputs = metadata_output_paths(
      directory.join("fixture-0123456789abcdef.d"),
      directory.join("libfixture-0123456789abcdef.rmeta"),
    );
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
    let first_outputs = metadata_output_paths(
      first_directory.join("fixture-0123456789abcdef.d"),
      first_directory.join("libfixture-0123456789abcdef.rmeta"),
    );
    let cold = br"build\ one\\debug\\deps\\libfixture-0123456789abcdef.rmeta: src\\lib.rs\n";
    let portable = portable_dep_info_output_bindings(cold, &first_outputs, source_root.path(), &capture)
      .expect("portable Windows dep-info");

    let validation = graduated_validation(observation);
    let second_directory = source_root.path().join("build two/debug/deps");
    fs::create_dir_all(&second_directory).expect("second output directory");
    let second_outputs = metadata_output_paths(
      second_directory.join("fixture-0123456789abcdef.d"),
      second_directory.join("libfixture-0123456789abcdef.rmeta"),
    );
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
  fn dep_info_source_root_rebinding_preserves_exact_path_spelling() {
    let parent = tempfile::tempdir().expect("package parent");
    let first_package = parent.path().join("first package");
    let second_package = parent.path().join("second package");
    fs::create_dir(&first_package).expect("first package");
    fs::create_dir(&second_package).expect("second package");
    let workspace = tempfile::tempdir().expect("workspace");
    let package_capture = |package: &Path| {
      let mut capture = synthetic_capture(&graduated_observation());
      capture.package_binding = Some(NativePackageBinding {
        root: crate::utils::canonicalize_existing(package).expect("canonical package"),
        spelling: package.to_path_buf(),
        source_relative: "src/lib.rs".to_string(),
      });
      capture
    };
    let first_bindings =
      dep_info_source_root_replacements(&first_package, PORTABLE_PACKAGE_ROOT, true).expect("first bindings");
    let second_bindings =
      dep_info_source_root_replacements(&second_package, PORTABLE_PACKAGE_ROOT, false).expect("second bindings");
    let canonical_package = crate::utils::canonicalize_existing(&second_package).expect("canonical second package");
    let converged_bindings =
      dep_info_source_root_replacements(&canonical_package, PORTABLE_PACKAGE_ROOT, false).expect("converged bindings");
    assert!(converged_bindings.iter().any(|(token, root)| {
      token.starts_with(format!("{PORTABLE_PACKAGE_ROOT}/dep-info/canonical/").as_bytes())
        && root == canonical_package.as_os_str().as_encoded_bytes()
    }));

    for (cold_root, portable_root) in first_bindings {
      let expected_root = second_bindings
        .iter()
        .find_map(|(token, root)| (token == &portable_root).then_some(root))
        .expect("same root spelling in second package");
      let portable = portable_dep_info_source_roots(&cold_root, workspace.path(), &package_capture(&first_package))
        .expect("portable package root");
      assert_eq!(portable, portable_root);
      let restored = rebind_dep_info_source_roots(&portable, workspace.path(), &package_capture(&second_package))
        .expect("restored package root");
      assert_eq!(&restored, expected_root);
    }
  }

  #[test]
  fn dep_info_cas_bytes_do_not_depend_on_cargo_output_directory() {
    let source_root = tempfile::tempdir().expect("source root");
    let capture = synthetic_capture(&graduated_observation());
    let output_paths = |directory: &str| {
      let directory = source_root.path().join(directory);
      fs::create_dir_all(&directory).expect("output directory");
      metadata_output_paths(
        directory.join("fixture-0123456789abcdef.d"),
        directory.join("libfixture-0123456789abcdef.rmeta"),
      )
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

  #[cfg(unix)]
  #[test]
  fn dep_info_root_rebinding_preserves_literal_unix_backslashes() {
    let parent = tempfile::tempdir().expect("source parent");
    let source_root = parent.path().join("source\\root");
    fs::create_dir(&source_root).expect("source root");
    let capture = synthetic_capture(&graduated_observation());
    let spelling = source_root.as_os_str().as_encoded_bytes();

    assert_eq!(
      rebind_dep_info_source_roots(PORTABLE_SOURCE_ROOT.as_bytes(), &source_root, &capture)
        .expect("rebound dep-info root"),
      escape_dep_info_path(spelling)
    );
    assert_eq!(source_root_path_spellings(&source_root), vec![spelling.to_vec()]);
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
}
