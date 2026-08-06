//! Immutable machine-local storage for verified hermetic action outputs.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{FastCacheValidation, OutputEntry, OutputEntryKind, OutputManifest};
use crate::compiler::diagnostics_store::{
  CompilerEvidenceObject, CompilerEvidenceValidation, EVIDENCE_ACTION_KEY_PREFIX, EVIDENCE_CANDIDATE_KEY_PREFIX,
  EVIDENCE_OBJECT_PREFIX, validate_evidence_action_key, validate_evidence_candidate_key, validate_evidence_object,
};
use crate::compiler::native_cache::{NativeCompilerValidation, PreparedNativeOrigin, PreparedNativeResult};
use crate::error::{RailError, RailResult};

const CAS_VERSION: u32 = 2;
const CAS_ROOT_NAME: &str = "local-cas-v2";
const LEGACY_CAS_ROOT_NAME: &str = "local-cas-v1";
const LEGACY_OWNER_MARKER: &[u8] = b"cargo-rail-local-cas\nschema=1\n";
const OWNER_MARKER_PREFIX: &str = "cargo-rail-local-cas\nschema=2\ntrust-domain=";
const DEFAULT_TRUST_DOMAIN_FILE: &str = "LOCAL_TRUST_DOMAIN";
pub(crate) const CACHE_BASE_ENV: &str = "CARGO_RAIL_CACHE_DIR";
pub(crate) const CACHE_MAX_BYTES_ENV: &str = "CARGO_RAIL_CACHE_MAX_BYTES";
pub(crate) const CACHE_TRUST_DOMAIN_ENV: &str = "CARGO_RAIL_CACHE_TRUST_DOMAIN";
const DEFAULT_CACHE_MAX_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const MAX_RESULT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_OBJECT_METADATA_BYTES: u64 = 64 * 1024 * 1024;
const MAX_LOOKUP_BYTES: u64 = MAX_RESULT_BYTES + MAX_OBJECT_METADATA_BYTES + 1;
const MAX_TREE_DEPTH: usize = 128;
const MAX_PATH_BYTES: usize = 4 * 1024;
const MAX_NAME_BYTES: usize = 255;
const MAX_ENTRIES: usize = 1_000_000;
#[cfg(any(unix, windows, test))]
const MAX_CANDIDATE_PINS: usize = 4096;
const IO_BUFFER_BYTES: usize = 64 * 1024;
const STALE_LEASE_SECONDS: u64 = 24 * 60 * 60;
const EVIDENCE_CANDIDATE_INDEX_VERSION: u32 = 1;
const EVIDENCE_CANDIDATE_INDEX_DIRECTORY: &str = "compiler-evidence-candidates";
const NATIVE_ENVIRONMENT_SELECTOR_DIRECTORY: &str = "native-environment-selectors-v1";
const MAX_NATIVE_ENVIRONMENT_SELECTOR_BYTES: u64 = 1024 * 1024;
const NATIVE_ENVIRONMENT_SELECTOR_CONFLICT_BYTES: &[u8] =
  b"cargo-rail-native-environment-selector-conflict\nschema=1\n";
const NATIVE_ACTION_STATE_VERSION: u32 = 2;
const NATIVE_ACTION_STATE_DIRECTORY: &str = "native-actions-v2";
const LEGACY_NATIVE_ACTION_STATE_DIRECTORY: &str = "native-actions";
const CAPACITY_STATE_FILE: &str = "CAPACITY.json";
const NATIVE_LEDGER_STATE_FILE: &str = "NATIVE_LEDGER.json";
const MAX_NATIVE_TERMINAL_STATES: u64 = 32 * 1024;
const MAX_NATIVE_TERMINAL_BYTES: u64 = 16 * 1024 * 1024;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
const SYSROOT_IDENTITY_MEMO_DIRECTORY: &str = "sysroot-identities";

const BLOB_PREFIX: &str = "blob-v1-sha256-";
const TREE_PREFIX: &str = "tree-v1-sha256-";
const MANIFEST_PREFIX: &str = "output-manifest-v1-sha256-";
const ACTION_RESULT_PREFIX: &str = "action-result-v1-sha256-";
const ACTION_KEY_PREFIX: &str = "hermetic-action-v1-sha256-";
const VALIDATION_PREFIX: &str = "validation-v1-sha256-";
const LOOKUP_PREFIX: &str = "local-lookup-v1-sha256-";

/// A verified cache lookup restored into an isolated output root.
#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) struct CacheHit {
  pub(super) action_result: String,
  pub(super) result_digest: String,
  pub(super) output_manifest: OutputManifest,
  pub(super) compiler_units: usize,
  pub(super) objects_verified: u64,
  pub(super) bytes_read: u64,
  pub(super) bytes_restored: u64,
}

/// A fail-closed lookup outcome that permits ordinary cold execution.
#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) struct CacheMiss {
  pub(super) kind: CacheMissKind,
  pub(super) reason: String,
  pub(super) objects_verified: u64,
  pub(super) bytes_read: u64,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CacheMissKind {
  Miss,
  Corrupt,
  Incompatible,
}

#[cfg(any(target_os = "macos", test))]
pub(super) enum CacheLookup {
  Hit(CacheHit),
  Miss(CacheMiss),
}

/// A fully verified action-result bundle that may be checked against current raw inputs.
#[cfg(target_os = "macos")]
pub(super) struct CacheCandidate {
  pub(super) action_key: String,
  pub(super) validation: FastCacheValidation,
}

#[cfg(any(unix, windows, test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct NativeCacheHit {
  pub(crate) bytes_read: u64,
  pub(crate) bytes_restored: u64,
}

#[cfg(any(unix, windows, test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct NativeCacheMiss {
  pub(crate) reason: String,
  pub(crate) bytes_read: u64,
}

#[cfg(any(unix, windows, test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum NativeCacheLookup {
  Hit(NativeCacheHit),
  Miss(NativeCacheMiss),
}

/// Outcome of immutably publishing one native compiler environment selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeEnvironmentSelectorPublication {
  Created,
  Converged,
  Diverged,
}

/// Exact lookup outcome for one complete pre-executable native action.
pub(crate) enum NativeActionLookup<'a> {
  Hit(Box<NativeActionHit<'a>>),
  Miss(NativeCacheMiss),
}

/// One uniquely authoritative native result held under a stable local CAS view.
pub(crate) struct NativeActionHit<'a> {
  pub(crate) validation: NativeCompilerValidation,
  pub(crate) bytes_read: u64,
  cas: &'a LocalCas,
  _lock: LocalCasLifecycleLock,
  verified: VerifiedResult,
}

/// One verified compiler-evidence candidate loaded without granting reuse authority.
pub(crate) struct CompilerEvidenceCandidate {
  pub(crate) validation: CompilerEvidenceValidation,
  pub(crate) evidence: CompilerEvidenceObject,
  pub(crate) created_unix_nanos: u128,
}

#[derive(Debug, Default)]
pub(crate) struct StoreStats {
  pub(crate) action_result: Option<String>,
  pub(crate) objects_written: u64,
  pub(crate) bytes_written: u64,
}

pub(super) struct StoreRequest<'a> {
  pub(super) action_key: &'a str,
  pub(super) lookup_key: &'a str,
  pub(super) result_digest: &'a str,
  pub(super) manifest: &'a OutputManifest,
  pub(super) validation: &'a FastCacheValidation,
  pub(super) compiler_units: usize,
  pub(super) source_root: &'a Path,
}

pub(crate) struct CompilerEvidenceStoreRequest<'a> {
  pub(crate) validation: &'a CompilerEvidenceValidation,
  pub(crate) evidence: &'a CompilerEvidenceObject,
}

struct ValidatedStoreRequest<'a> {
  action_key: &'a str,
  lookup_key: &'a str,
  result_digest: &'a str,
  manifest: &'a OutputManifest,
  validation: StoredValidationRef<'a>,
  compiler_units: usize,
  source_root: &'a Path,
  native_origins: Option<NativeResultOrigins>,
  move_preverified_blobs: bool,
  before_authority: Option<&'a mut dyn FnMut() -> RailResult<()>>,
}

/// One validated local CAS rooted outside any physical checkout.
#[derive(Debug)]
pub(crate) struct LocalCas {
  root: PathBuf,
  lifecycle_lock: PathBuf,
  max_bytes: u64,
}

struct SelectedCacheAuthority {
  root_name: String,
  trust_domain: String,
}

/// Read-only measurements for one validated shared local CAS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct LocalCasStatus {
  pub(crate) root: String,
  pub(crate) trust_domain: String,
  pub(crate) bytes: u64,
  pub(crate) max_bytes: u64,
  pub(crate) committed_result_bytes: u64,
  pub(crate) results: u64,
  pub(crate) pins: u64,
  pub(crate) native_actions: u64,
  pub(crate) native_unique: u64,
  pub(crate) native_conflicted: u64,
  pub(crate) native_quarantined: u64,
  pub(crate) native_local_origins: u64,
  pub(crate) native_remote_origins: u64,
  pub(crate) native_ledger_bytes: u64,
  pub(crate) native_ledger_max_bytes: u64,
  pub(crate) native_ledger_disabled: bool,
  pub(crate) objects: u64,
  pub(crate) active_leases: u64,
  pub(crate) stale_leases: u64,
  pub(crate) staging_entries: u64,
  pub(crate) staging_bytes: u64,
  pub(crate) index_files: u64,
  pub(crate) reclaimable_bytes: u64,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub(crate) oldest_used_unix_ms: Option<u64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub(crate) newest_used_unix_ms: Option<u64>,
}

/// Exclusive authority over one shared CAS lifecycle mutation or snapshot.
struct LocalCasLifecycleLock {
  _file: File,
}

impl NativeActionHit<'_> {
  /// Revalidate the selector that authorized this action without reacquiring the lifecycle lock.
  ///
  /// This hit retains the shared lifecycle lock, so an exclusive selector publisher cannot
  /// create a conflict between this check and the caller's restore.
  pub(crate) fn validate_environment_selector<'a>(
    &self,
    base_action_key: &str,
    expected_names: impl IntoIterator<Item = &'a str>,
  ) -> RailResult<()> {
    if self.cas.native_environment_selector_conflicted(base_action_key)? {
      return Err(RailError::message(
        "local CAS native environment selector is durably conflicted",
      ));
    }
    let Some(actual_names) = self.cas.load_native_environment_selector(base_action_key)? else {
      return Err(RailError::message(
        "local CAS native environment selector is absent for an authoritative action",
      ));
    };
    if !actual_names.iter().map(String::as_str).eq(expected_names) {
      return Err(RailError::message(
        "local CAS native environment selector does not match the authoritative action",
      ));
    }
    if self.cas.native_environment_selector_conflicted(base_action_key)? {
      return Err(RailError::message(
        "local CAS native environment selector is durably conflicted",
      ));
    }
    Ok(())
  }

  /// Verify the action-to-base binding and its terminal selector immediately
  /// before exposing this result to a remote authority.
  pub(crate) fn validate_remote_publication<'a>(&'a self, base_action_key: &str) -> RailResult<&'a [String]> {
    let expected_names = self.validation.remote_publication_environment_names(base_action_key)?;
    self.validate_environment_selector(base_action_key, expected_names.iter().map(String::as_str))?;
    Ok(expected_names)
  }

  pub(crate) fn association(&self) -> RailResult<crate::compiler::native_cache::pack::NativeAssociation> {
    crate::compiler::native_cache::pack::association(&self.validation)
  }

  /// Stream the fixed canonical result pack directly from immutable CAS blobs
  /// while this verified action view holds the lifecycle read authority.
  pub(crate) fn export_pack<W: std::io::Write>(
    &self,
    writer: W,
  ) -> RailResult<crate::compiler::native_cache::pack::NativePackExport> {
    crate::compiler::native_cache::pack::export(&self.validation, writer, |slot| {
      let identity = blob_id(slot.digest, slot.bytes).map_err(fault_to_error)?;
      let hex = validated_id_hex(&identity, BLOB_PREFIX)?;
      let path = self.verified.bundle.join("blobs").join(format!("{hex}.blob"));
      let file = File::open(&path)?;
      if !crate::utils::private_file_matches_path(&file, &path, slot.bytes)? {
        return Err(RailError::message(
          "native result pack source is not the expected private immutable blob",
        ));
      }
      Ok(file)
    })
  }

  /// Materialize the already verified unique result into private staging.
  #[cfg(test)]
  pub(crate) fn restore(&self, destination: &Path) -> NativeCacheLookup {
    let mut stats = ReadStats::default();
    if let Err(fault) = self.cas.materialize(&self.verified, destination, &mut stats) {
      return NativeCacheLookup::Miss(NativeCacheMiss {
        reason: fault.reason,
        bytes_read: stats.bytes,
      });
    }
    if let Ok(key_hex) = validated_action_key_hex(self.validation.action_key()) {
      let state_path = self
        .cas
        .root
        .join(NATIVE_ACTION_STATE_DIRECTORY)
        .join(format!("{key_hex}.json"));
      let _ = OpenOptions::new()
        .read(true)
        .write(true)
        .open(state_path)
        .and_then(|file| file.set_modified(SystemTime::now()));
    }
    NativeCacheLookup::Hit(NativeCacheHit {
      bytes_read: stats.bytes,
      bytes_restored: stats.restored,
    })
  }

  /// Materialize into one caller-registered exact staging directory.
  ///
  /// Restore transactions use this path so process-death recovery never has
  /// to discover a random CAS temporary directory by scanning its parent.
  pub(crate) fn restore_registered(&self, destination: &Path, staging: &Path) -> NativeCacheLookup {
    let mut stats = ReadStats::default();
    if let Err(fault) = self
      .cas
      .materialize_registered(&self.verified, destination, staging, &mut stats)
    {
      return NativeCacheLookup::Miss(NativeCacheMiss {
        reason: fault.reason,
        bytes_read: stats.bytes,
      });
    }
    if let Ok(key_hex) = validated_action_key_hex(self.validation.action_key()) {
      let state_path = self
        .cas
        .root
        .join(NATIVE_ACTION_STATE_DIRECTORY)
        .join(format!("{key_hex}.json"));
      let _ = OpenOptions::new()
        .read(true)
        .write(true)
        .open(state_path)
        .and_then(|file| file.set_modified(SystemTime::now()));
    }
    NativeCacheLookup::Hit(NativeCacheHit {
      bytes_read: stats.bytes,
      bytes_restored: stats.restored,
    })
  }
}

#[derive(Clone, Copy)]
enum LockMode {
  Shared,
  Exclusive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TreeObject {
  version: u32,
  entries: Vec<TreeEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct TreeEntry {
  name: String,
  #[serde(flatten)]
  kind: TreeEntryKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum TreeEntryKind {
  File {
    blob: String,
    content_digest: String,
    bytes: u64,
    mode: u32,
  },
  Directory {
    tree: String,
    mode: u32,
  },
  Symlink {
    target: String,
    directory: bool,
  },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionResultObject {
  version: u32,
  action_key: String,
  lookup_key: String,
  result_digest: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  output_manifest: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  output_tree: Option<String>,
  validation: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  compiler_units: Option<usize>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  compiler_evidence: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionPin {
  version: u32,
  action_key: String,
  action_result: String,
  lookup_key: String,
  created_unix_nanos: u128,
}

/// Durable native action authority. Conflict and quarantine states are terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeActionState {
  version: u32,
  action_key: String,
  state: NativeActionStateKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum NativeActionStateKind {
  UniqueResult {
    result_key: String,
    action_result: String,
    origins: NativeResultOrigins,
  },
  ConflictedResults {
    first_result_key: String,
    second_result_key: String,
  },
  Quarantined {
    fault: NativeStateFault,
    evidence_digest: String,
  },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeResultOrigins {
  local: bool,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  remote: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum NativeStateFault {
  Unreadable,
  Malformed,
  Incompatible,
  AmbiguousReplacement,
}

/// Disposable discovery pointer for one compiler-evidence configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceCandidateIndexEntry {
  version: u32,
  action_key: String,
  candidate_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseRecord {
  version: u32,
  action_result: String,
  created_unix_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapacityState {
  version: u32,
  result_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeLedgerState {
  version: u32,
  terminal_states: u64,
  terminal_bytes: u64,
  disabled: bool,
}

#[derive(Default)]
struct ReadStats {
  objects: u64,
  bytes: u64,
  #[cfg(any(unix, windows, test))]
  restored: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FaultKind {
  #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
  Miss,
  Corrupt,
  Incompatible,
}

#[derive(Debug)]
struct Fault {
  kind: FaultKind,
  reason: String,
}

impl Fault {
  fn corrupt(reason: impl Into<String>) -> Self {
    Self {
      kind: FaultKind::Corrupt,
      reason: reason.into(),
    }
  }

  #[cfg(any(target_os = "macos", test))]
  fn miss(reason: impl Into<String>) -> Self {
    Self {
      kind: FaultKind::Miss,
      reason: reason.into(),
    }
  }

  fn incompatible(reason: impl Into<String>) -> Self {
    Self {
      kind: FaultKind::Incompatible,
      reason: reason.into(),
    }
  }
}

struct LeaseGuard {
  path: PathBuf,
}

impl Drop for LeaseGuard {
  fn drop(&mut self) {
    let _ = fs::remove_file(&self.path);
  }
}

#[derive(Debug)]
struct PreparedBlob {
  source: PathBuf,
  content_digest: String,
  bytes: u64,
}

#[derive(Debug)]
struct PreparedTree {
  root: String,
  trees: BTreeMap<String, Vec<u8>>,
  blobs: BTreeMap<String, PreparedBlob>,
}

struct BundlePublication<'a> {
  object: &'a ActionResultObject,
  object_bytes: &'a [u8],
  manifest: &'a OutputManifest,
  manifest_bytes: &'a [u8],
  validation: StoredValidationRef<'a>,
  validation_bytes: &'a [u8],
  prepared: &'a PreparedTree,
  move_preverified_blobs: bool,
}

struct StagedBundle {
  _temporary: tempfile::TempDir,
  _active: File,
  payload: PathBuf,
  stats: StoreStats,
}

/// One native result whose immutable bundle is ready for a later authority
/// commit. The command coordinator retains this value so preparation can
/// overlap rustc while one empty-authority batch owns the durability barriers.
pub(crate) struct StagedNativeResult {
  validation: NativeCompilerValidation,
  origins: NativeResultOrigins,
  object: ActionResultObject,
  action_result: String,
  incoming: u64,
  staged: StagedBundle,
}

impl StagedNativeResult {
  pub(crate) fn validation(&self) -> &NativeCompilerValidation {
    &self.validation
  }
}

struct CompilerEvidencePublication<'a> {
  action_result: &'a str,
  object: &'a ActionResultObject,
  object_bytes: &'a [u8],
  validation: &'a CompilerEvidenceValidation,
  validation_bytes: &'a [u8],
  evidence: &'a CompilerEvidenceObject,
  evidence_bytes: &'a [u8],
}

#[derive(Clone, Copy, Serialize)]
#[serde(untagged)]
enum StoredValidationRef<'a> {
  Hermetic(&'a FastCacheValidation),
  NativeCompiler(&'a NativeCompilerValidation),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum StoredValidation {
  Hermetic(Box<FastCacheValidation>),
  NativeCompiler(Box<NativeCompilerValidation>),
}

impl StoredValidation {
  fn action_key(&self) -> &str {
    match self {
      Self::Hermetic(validation) => &validation.action_key,
      Self::NativeCompiler(validation) => validation.action_key(),
    }
  }

  fn lookup_key(&self) -> &str {
    match self {
      Self::Hermetic(validation) => &validation.lookup_key,
      Self::NativeCompiler(validation) => validation.action_key(),
    }
  }

  fn validate_object(&self) -> RailResult<()> {
    match self {
      Self::Hermetic(validation) => validation.validate_object(),
      Self::NativeCompiler(validation) => validation.validate_object(),
    }
  }

  fn result_digest(&self, output_manifest: &str) -> String {
    match self {
      Self::Hermetic(validation) => super::hermetic_result_digest(&validation.action_key, output_manifest),
      Self::NativeCompiler(validation) => validation.result_digest(output_manifest),
    }
  }
}

#[derive(Default)]
struct BuildDirectory {
  children: BTreeMap<String, BuildNode>,
}

enum BuildNode {
  File {
    source: PathBuf,
    content_digest: String,
    bytes: u64,
    mode: u32,
  },
  Directory {
    mode: u32,
    contents: BuildDirectory,
  },
  Symlink {
    target: String,
    directory: bool,
  },
}

impl LocalCas {
  #[cfg(test)]
  pub(crate) fn root(&self) -> &Path {
    &self.root
  }

  fn lock(&self) -> RailResult<LocalCasLifecycleLock> {
    lock_local_cas(&self.lifecycle_lock, false, LockMode::Exclusive)?
      .ok_or_else(|| RailError::message("local CAS lifecycle lock disappeared"))
  }

  fn read_lock(&self) -> RailResult<LocalCasLifecycleLock> {
    lock_local_cas(&self.lifecycle_lock, false, LockMode::Shared)?
      .ok_or_else(|| RailError::message("local CAS lifecycle lock disappeared"))
  }

  /// Load the compiler-selected environment names for one environment-neutral action.
  pub(crate) fn native_environment_selector(&self, base_action_key: &str) -> RailResult<Option<Vec<String>>> {
    let _lock = self.read_lock()?;
    if self.native_environment_selector_conflicted(base_action_key)? {
      return Err(RailError::message(
        "local CAS native environment selector is durably conflicted",
      ));
    }
    let selector = self.load_native_environment_selector(base_action_key)?;
    if self.native_environment_selector_conflicted(base_action_key)? {
      return Err(RailError::message(
        "local CAS native environment selector is durably conflicted",
      ));
    }
    Ok(selector)
  }

  /// Immutably publish compiler-selected environment names for one environment-neutral action.
  pub(crate) fn publish_native_environment_selector(
    &self,
    base_action_key: &str,
    names: &[String],
  ) -> RailResult<NativeEnvironmentSelectorPublication> {
    let _lock = self.lock()?;
    let bytes = encode_native_environment_selector(names)?;
    let destination = self.native_environment_selector_path(base_action_key)?;
    let directory = destination
      .parent()
      .ok_or_else(|| RailError::message("native environment selector has no parent directory"))?;
    validate_real_directory(directory, "local CAS native environment selector directory")?;
    if self.native_environment_selector_conflicted(base_action_key)? {
      return Ok(NativeEnvironmentSelectorPublication::Diverged);
    }

    if let Some(existing) = self.load_native_environment_selector(base_action_key)? {
      if existing != names {
        self.publish_native_environment_selector_conflict(base_action_key)?;
        return Ok(NativeEnvironmentSelectorPublication::Diverged);
      }
      return Ok(if self.native_environment_selector_conflicted(base_action_key)? {
        NativeEnvironmentSelectorPublication::Diverged
      } else {
        NativeEnvironmentSelectorPublication::Converged
      });
    }

    let mut temporary = tempfile::NamedTempFile::new_in(self.root.join("staging"))?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    match persist_noclobber_committed(temporary, &destination) {
      Ok(file) => {
        if !crate::utils::private_file_matches_path(&file, &destination, bytes.len() as u64)? {
          return Err(RailError::message(
            "published local CAS native environment selector is not a private regular file",
          ));
        }
        sync_directory(directory)?;
        Ok(if self.native_environment_selector_conflicted(base_action_key)? {
          NativeEnvironmentSelectorPublication::Diverged
        } else {
          NativeEnvironmentSelectorPublication::Created
        })
      }
      Err(error) => {
        if self.native_environment_selector_conflicted(base_action_key)? {
          return Ok(NativeEnvironmentSelectorPublication::Diverged);
        }
        match self.load_native_environment_selector(base_action_key)? {
          Some(existing) if existing == names => Ok(if self.native_environment_selector_conflicted(base_action_key)? {
            NativeEnvironmentSelectorPublication::Diverged
          } else {
            NativeEnvironmentSelectorPublication::Converged
          }),
          Some(_) => {
            self.publish_native_environment_selector_conflict(base_action_key)?;
            Ok(NativeEnvironmentSelectorPublication::Diverged)
          }
          None => Err(RailError::message(format!(
            "failed to atomically publish local CAS native environment selector '{}': {}",
            destination.display(),
            error.error
          ))),
        }
      }
    }
  }

  fn native_environment_selector_conflicted(&self, base_action_key: &str) -> RailResult<bool> {
    let directory = self.root.join(NATIVE_ENVIRONMENT_SELECTOR_DIRECTORY);
    validate_real_directory(&directory, "local CAS native environment selector directory")?;
    let path = self.native_environment_selector_conflict_path(base_action_key)?;
    let metadata = match fs::symlink_metadata(&path) {
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
      Err(error) => return Err(error.into()),
      Ok(metadata) => metadata,
    };
    if !metadata.is_file()
      || is_link_or_reparse(&metadata)
      || !has_single_link(&metadata)
      || metadata.len() != NATIVE_ENVIRONMENT_SELECTOR_CONFLICT_BYTES.len() as u64
    {
      return Err(RailError::message(format!(
        "local CAS native environment selector conflict '{}' is not a private canonical file",
        path.display()
      )));
    }
    let mut file = File::open(&path)?;
    if !crate::utils::private_file_matches_path(&file, &path, metadata.len())? {
      return Err(RailError::message(format!(
        "local CAS native environment selector conflict '{}' changed before it was read",
        path.display()
      )));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    (&mut file)
      .take(metadata.len().saturating_add(1))
      .read_to_end(&mut bytes)?;
    if bytes != NATIVE_ENVIRONMENT_SELECTOR_CONFLICT_BYTES
      || !crate::utils::private_file_matches_path(&file, &path, metadata.len())?
    {
      return Err(RailError::message(format!(
        "local CAS native environment selector conflict '{}' is malformed or changed while it was read",
        path.display()
      )));
    }
    crate::instrumentation::record_cas_read(bytes.len() as u64);
    Ok(true)
  }

  fn publish_native_environment_selector_conflict(&self, base_action_key: &str) -> RailResult<()> {
    let destination = self.native_environment_selector_conflict_path(base_action_key)?;
    let directory = destination
      .parent()
      .ok_or_else(|| RailError::message("native environment selector conflict has no parent directory"))?;
    validate_real_directory(directory, "local CAS native environment selector directory")?;
    let mut temporary = tempfile::NamedTempFile::new_in(self.root.join("staging"))?;
    temporary.write_all(NATIVE_ENVIRONMENT_SELECTOR_CONFLICT_BYTES)?;
    temporary.as_file().sync_all()?;
    match persist_noclobber_committed(temporary, &destination) {
      Ok(file) => {
        if !crate::utils::private_file_matches_path(
          &file,
          &destination,
          NATIVE_ENVIRONMENT_SELECTOR_CONFLICT_BYTES.len() as u64,
        )? {
          return Err(RailError::message(
            "published local CAS native environment selector conflict is not a private regular file",
          ));
        }
        sync_directory(directory)
      }
      Err(error) => {
        if self.native_environment_selector_conflicted(base_action_key)? {
          Ok(())
        } else {
          Err(RailError::message(format!(
            "failed to atomically publish local CAS native environment selector conflict '{}': {}",
            destination.display(),
            error.error
          )))
        }
      }
    }
  }

  fn load_native_environment_selector(&self, base_action_key: &str) -> RailResult<Option<Vec<String>>> {
    let path = self.native_environment_selector_path(base_action_key)?;
    let directory = self.root.join(NATIVE_ENVIRONMENT_SELECTOR_DIRECTORY);
    validate_real_directory(&directory, "local CAS native environment selector directory")?;
    let metadata = match fs::symlink_metadata(&path) {
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
      Err(error) => return Err(error.into()),
      Ok(metadata) => metadata,
    };
    if !metadata.is_file()
      || is_link_or_reparse(&metadata)
      || !has_single_link(&metadata)
      || metadata.len() > MAX_NATIVE_ENVIRONMENT_SELECTOR_BYTES
    {
      return Err(RailError::message(format!(
        "local CAS native environment selector '{}' is not a private bounded regular file",
        path.display()
      )));
    }
    let mut file = File::open(&path).map_err(|error| {
      RailError::message(format!(
        "failed to open local CAS native environment selector '{}': {error}",
        path.display()
      ))
    })?;
    if !crate::utils::private_file_matches_path(&file, &path, metadata.len())? {
      return Err(RailError::message(format!(
        "local CAS native environment selector '{}' changed before it was read",
        path.display()
      )));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    (&mut file)
      .take(metadata.len().saturating_add(1))
      .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.len() || !crate::utils::private_file_matches_path(&file, &path, metadata.len())? {
      return Err(RailError::message(format!(
        "local CAS native environment selector '{}' changed while it was read",
        path.display()
      )));
    }
    crate::instrumentation::record_cas_read(bytes.len() as u64);
    let names: Vec<String> = serde_json::from_slice(&bytes).map_err(|error| {
      RailError::message(format!(
        "local CAS native environment selector '{}' is malformed: {error}",
        path.display()
      ))
    })?;
    if canonical_json(&names)? != bytes {
      return Err(RailError::message(format!(
        "local CAS native environment selector '{}' is not canonically encoded",
        path.display()
      )));
    }
    crate::compiler::native_cache::validate_environment_selector_names(names.iter().map(String::as_str))?;
    Ok(Some(names))
  }

  fn native_environment_selector_path(&self, base_action_key: &str) -> RailResult<PathBuf> {
    let key = validated_id_hex(base_action_key, crate::compiler::native_cache::BASE_ACTION_KEY_PREFIX)?;
    Ok(
      self
        .root
        .join(NATIVE_ENVIRONMENT_SELECTOR_DIRECTORY)
        .join(format!("{key}.json")),
    )
  }

  fn native_environment_selector_conflict_path(&self, base_action_key: &str) -> RailResult<PathBuf> {
    let key = validated_id_hex(base_action_key, crate::compiler::native_cache::BASE_ACTION_KEY_PREFIX)?;
    Ok(
      self
        .root
        .join(NATIVE_ENVIRONMENT_SELECTOR_DIRECTORY)
        .join(format!("{key}.conflict")),
    )
  }

  /// Prove that this local authority currently contains no native action state.
  /// A concurrent publisher after this linearization point may cause a
  /// conservative cold compile, but cannot make discovery-only keys reusable.
  pub(crate) fn native_authority_is_empty(&self) -> RailResult<bool> {
    let _lock = self.read_lock()?;
    if validate_native_ledger(&self.root)?.disabled {
      return Ok(false);
    }
    let directory = self.root.join(NATIVE_ACTION_STATE_DIRECTORY);
    validate_real_directory(&directory, "local CAS native action state")?;
    Ok(fs::read_dir(directory)?.next().is_none())
  }

  /// Create private same-filesystem native-result staging guarded from concurrent GC.
  pub(crate) fn native_result_staging(&self) -> RailResult<crate::compiler::native_cache::pack::NativeResultStaging> {
    let (directory, active) = self.create_guarded_staging("native-result-")?;
    Ok(crate::compiler::native_cache::pack::NativeResultStaging::guarded(
      directory, active,
    ))
  }

  /// Create one result directory beneath a command-owned staging lease.
  pub(crate) fn native_command_result_staging(
    &self,
    command_staging: &Path,
  ) -> RailResult<crate::compiler::native_cache::pack::NativeResultStaging> {
    let _lifecycle = self.read_lock()?;
    let staging_root = self.root.join("staging");
    validate_real_directory(&staging_root, "local CAS staging")?;
    let parent = command_staging
      .parent()
      .ok_or_else(|| RailError::message("native publication staging has no parent"))?;
    if crate::utils::canonicalize_existing(parent)? != crate::utils::canonicalize_existing(&staging_root)? {
      return Err(RailError::message(
        "native publication staging is outside the local CAS staging root",
      ));
    }
    let name = command_staging.file_name().and_then(OsStr::to_str).unwrap_or_default();
    if !name.starts_with("native-command-") {
      return Err(RailError::message("native publication staging has an invalid name"));
    }
    validate_real_directory(command_staging, "native publication staging")?;
    if !staging_entry_is_active(command_staging)? {
      return Err(RailError::message("native publication staging is not active"));
    }
    let incoming = command_staging.join("incoming");
    validate_real_directory(&incoming, "native publication incoming directory")?;
    let directory = tempfile::Builder::new().prefix("native-unit-").tempdir_in(incoming)?;
    Ok(crate::compiler::native_cache::pack::NativeResultStaging::command_scoped(directory))
  }

  /// Create one command-owned staging lease that protects all queued native results.
  pub(crate) fn native_publication_staging(&self) -> RailResult<(tempfile::TempDir, File)> {
    self.create_guarded_staging("native-command-")
  }

  /// Create one staging directory without exposing an unlocked directory to
  /// lifecycle cleanup. The shared lock covers only directory and lease
  /// creation; the per-entry lock protects the longer parallel staging work.
  fn create_guarded_staging(&self, prefix: &str) -> RailResult<(tempfile::TempDir, File)> {
    let _lifecycle = self.read_lock()?;
    let staging = self.root.join("staging");
    validate_real_directory(&staging, "local CAS staging")?;
    let directory = tempfile::Builder::new().prefix(prefix).tempdir_in(&staging)?;
    #[cfg(test)]
    pause_test_staging_before_active(directory.path())?;
    let active_path = directory.path().join("ACTIVE");
    let active = OpenOptions::new()
      .read(true)
      .write(true)
      .create_new(true)
      .open(active_path)?;
    active.lock()?;
    Ok((directory, active))
  }

  #[cfg(test)]
  pub(crate) fn status(&self) -> RailResult<LocalCasStatus> {
    status_at_with_max(&self.root, self.max_bytes)?.ok_or_else(|| RailError::message("local CAS disappeared"))
  }

  /// Return the private candidate location for one verified sysroot identity memo.
  #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
  pub(crate) fn sysroot_identity_memo_path(&self, lookup: &crate::source::ContentDigest) -> PathBuf {
    self
      .root
      .join(SYSROOT_IDENTITY_MEMO_DIRECTORY)
      .join(format!("{lookup}.json"))
  }

  pub(crate) fn open() -> RailResult<Self> {
    let base = cache_base()?;
    fs::create_dir_all(&base).map_err(|error| {
      RailError::message(format!(
        "failed to create local cache base '{}': {error}",
        base.display()
      ))
    })?;
    let base = fs::canonicalize(&base).map_err(|error| {
      RailError::message(format!(
        "failed to resolve local cache base '{}': {error}",
        base.display()
      ))
    })?;
    validate_real_directory(&base, "local cache base")?;
    let cargo_rail = create_real_directory(&base, "cargo-rail")?;
    let authority = selected_cache_authority(&cargo_rail, true)?;
    let lifecycle_lock = cargo_rail.join(format!("{}.lock", authority.root_name));
    let _lock = lock_local_cas(&lifecycle_lock, true, LockMode::Exclusive)?
      .ok_or_else(|| RailError::message("local CAS lifecycle lock was not created"))?;
    let root = create_real_directory(&cargo_rail, &authority.root_name)?;
    prove_local_cache_volume(&root)?;
    ensure_owner_marker(&root, &authority.trust_domain)?;
    create_real_directory(&root, "staging")?;
    for name in [
      "results",
      "pins",
      "leases",
      NATIVE_ACTION_STATE_DIRECTORY,
      NATIVE_ENVIRONMENT_SELECTOR_DIRECTORY,
      EVIDENCE_CANDIDATE_INDEX_DIRECTORY,
    ] {
      create_real_directory(&root, name)?;
    }
    validate_optional_real_directory(
      &root.join(LEGACY_NATIVE_ACTION_STATE_DIRECTORY),
      "legacy local CAS native action state",
    )?;
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    create_real_directory(&root, SYSROOT_IDENTITY_MEMO_DIRECTORY)?;
    validate_root_entries(&root)?;
    clear_staging(&root.join("staging"))?;
    reconcile_capacity_state(&root)?;
    reconcile_native_ledger(&root)?;
    Ok(Self {
      root,
      lifecycle_lock,
      max_bytes: cache_max_bytes()?,
    })
  }

  /// Open the CAS prepared by this Cargo session without repeating lifecycle mutation.
  ///
  /// Native compiler wrappers call this only after the parent wrote the session
  /// through `LocalCas::open`. Every lookup and restore still takes the shared
  /// lifecycle lock and verifies its exact objects at the final operation.
  pub(crate) fn open_initialized() -> RailResult<Self> {
    let base = cache_base()?;
    let base = fs::canonicalize(&base).map_err(|error| {
      RailError::message(format!(
        "failed to resolve initialized local cache base '{}': {error}",
        base.display()
      ))
    })?;
    let max_bytes = cache_max_bytes()?;
    Self::open_initialized_at(&base, max_bytes)
  }

  fn open_initialized_at(base: &Path, max_bytes: u64) -> RailResult<Self> {
    validate_real_directory(base, "local cache base")?;
    let cargo_rail = base.join("cargo-rail");
    validate_real_directory(&cargo_rail, "local CAS owner")?;
    let authority = selected_cache_authority(&cargo_rail, false)?;
    let lifecycle_lock = cargo_rail.join(format!("{}.lock", authority.root_name));
    let _lock = lock_local_cas(&lifecycle_lock, false, LockMode::Shared)?
      .ok_or_else(|| RailError::message("initialized local CAS lifecycle lock disappeared"))?;
    let root = cargo_rail.join(&authority.root_name);
    validate_real_directory(&root, "local CAS root")?;
    prove_local_cache_volume(&root)?;
    ensure_owner_marker_existing(&root, Some(&authority.trust_domain))?;
    for name in [
      "staging",
      "results",
      "pins",
      "leases",
      NATIVE_ACTION_STATE_DIRECTORY,
      NATIVE_ENVIRONMENT_SELECTOR_DIRECTORY,
      EVIDENCE_CANDIDATE_INDEX_DIRECTORY,
    ] {
      validate_real_directory(&root.join(name), "local CAS required directory")?;
    }
    validate_optional_real_directory(
      &root.join(LEGACY_NATIVE_ACTION_STATE_DIRECTORY),
      "legacy local CAS native action state",
    )?;
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    validate_real_directory(
      &root.join(SYSROOT_IDENTITY_MEMO_DIRECTORY),
      "local CAS sysroot memo directory",
    )?;
    validate_root_entries(&root)?;
    validate_capacity_state(&root)?;
    validate_native_ledger(&root)?;
    Ok(Self {
      root,
      lifecycle_lock,
      max_bytes,
    })
  }

  #[cfg(test)]
  pub(crate) fn open_at(base: &Path, max_bytes: u64) -> RailResult<Self> {
    fs::create_dir_all(base)?;
    let base = fs::canonicalize(base)?;
    let cargo_rail = create_real_directory(&base, "cargo-rail")?;
    let authority = SelectedCacheAuthority {
      root_name: CAS_ROOT_NAME.to_string(),
      trust_domain: load_default_trust_domain(&cargo_rail, true)?,
    };
    let lifecycle_lock = cargo_rail.join(format!("{}.lock", authority.root_name));
    let _lock = lock_local_cas(&lifecycle_lock, true, LockMode::Exclusive)?
      .ok_or_else(|| RailError::message("local CAS lifecycle lock was not created"))?;
    let root = create_real_directory(&cargo_rail, &authority.root_name)?;
    prove_local_cache_volume(&root)?;
    ensure_owner_marker(&root, &authority.trust_domain)?;
    create_real_directory(&root, "staging")?;
    for name in [
      "results",
      "pins",
      "leases",
      NATIVE_ACTION_STATE_DIRECTORY,
      NATIVE_ENVIRONMENT_SELECTOR_DIRECTORY,
      EVIDENCE_CANDIDATE_INDEX_DIRECTORY,
    ] {
      create_real_directory(&root, name)?;
    }
    validate_optional_real_directory(
      &root.join(LEGACY_NATIVE_ACTION_STATE_DIRECTORY),
      "legacy local CAS native action state",
    )?;
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    create_real_directory(&root, SYSROOT_IDENTITY_MEMO_DIRECTORY)?;
    validate_root_entries(&root)?;
    clear_staging(&root.join("staging"))?;
    reconcile_capacity_state(&root)?;
    reconcile_native_ledger(&root)?;
    Ok(Self {
      root,
      lifecycle_lock,
      max_bytes,
    })
  }

  #[cfg(any(target_os = "macos", test))]
  pub(super) fn restore(&self, action_key: &str, destination: &Path) -> CacheLookup {
    let mut stats = ReadStats::default();
    match self.restore_inner(action_key, destination, &mut stats) {
      Ok(hit) => CacheLookup::Hit(CacheHit {
        action_result: hit.action_result,
        result_digest: hit.object.result_digest,
        output_manifest: hit.manifest,
        compiler_units: hit.object.compiler_units.unwrap_or_default(),
        objects_verified: stats.objects,
        bytes_read: stats.bytes,
        bytes_restored: stats.restored,
      }),
      Err(fault) => CacheLookup::Miss(CacheMiss {
        kind: match fault.kind {
          FaultKind::Miss => CacheMissKind::Miss,
          FaultKind::Corrupt => CacheMissKind::Corrupt,
          FaultKind::Incompatible => CacheMissKind::Incompatible,
        },
        reason: fault.reason,
        objects_verified: stats.objects,
        bytes_read: stats.bytes,
      }),
    }
  }

  #[cfg(target_os = "macos")]
  pub(super) fn candidates(&self, lookup_key: &str) -> RailResult<Vec<CacheCandidate>> {
    let _lock = self.read_lock()?;
    validate_lookup_key(lookup_key)?;
    let pins_directory = self.root.join("pins");
    let mut entries = fs::read_dir(&pins_directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    if entries.len() > MAX_CANDIDATE_PINS {
      return Err(RailError::message(format!(
        "local CAS has more than {MAX_CANDIDATE_PINS} action pins; refusing an unbounded pre-context scan"
      )));
    }
    let mut candidates = Vec::new();
    for entry in entries {
      let path = entry.path();
      let metadata = fs::symlink_metadata(&path)?;
      if !metadata.is_file() || is_link_or_reparse(&metadata) || !has_single_link(&metadata) {
        return Err(RailError::message(format!(
          "local CAS pin '{}' is not a bounded regular file",
          path.display()
        )));
      }
      let file_name = entry
        .file_name()
        .into_string()
        .map_err(|_| RailError::message("local CAS pin has a non-UTF-8 name"))?;
      let key_hex = file_name
        .strip_suffix(".json")
        .ok_or_else(|| RailError::message(format!("local CAS pin '{file_name}' has an invalid name")))?;
      let mut stats = ReadStats::default();
      let pin: ActionPin = read_canonical_json(&path, MAX_OBJECT_METADATA_BYTES, &mut stats).map_err(fault_to_error)?;
      if pin.version != CAS_VERSION {
        return Err(RailError::message("local CAS candidate pin has an incompatible schema"));
      }
      if validated_action_key_hex(&pin.action_key)? != key_hex {
        return Err(RailError::message(
          "local CAS candidate pin filename does not match its action key",
        ));
      }
      validate_any_lookup_key(&pin.lookup_key)?;
      validated_id_hex(&pin.action_result, ACTION_RESULT_PREFIX)?;
      if pin.lookup_key != lookup_key {
        continue;
      }
      let verified = self
        .load_verified_result(&pin.action_key, &pin.action_result, &mut stats)
        .map_err(fault_to_error)?;
      if verified.object.lookup_key != pin.lookup_key {
        return Err(RailError::message(
          "local CAS candidate pin does not match its verified action result",
        ));
      }
      let StoredValidation::Hermetic(validation) = verified.validation else {
        return Err(RailError::message(
          "local CAS candidate has the wrong validation domain",
        ));
      };
      validation.validate_object()?;
      candidates.push(CacheCandidate {
        action_key: pin.action_key,
        validation: *validation,
      });
    }
    Ok(candidates)
  }

  #[cfg(any(unix, windows, test))]
  pub(crate) fn native_action(&self, action_key: &str) -> RailResult<NativeActionLookup<'_>> {
    self.native_action_for_authority(action_key, None)
  }

  /// Resolve an exact action while accepting remote-only authority solely from
  /// the command's already authenticated, deployment-pinned authority.
  pub(crate) fn native_action_for_authority(
    &self,
    action_key: &str,
    accepted_remote: Option<&crate::compiler::native_cache::RemoteAuthorityId>,
  ) -> RailResult<NativeActionLookup<'_>> {
    self.native_action_with_retry(action_key, accepted_remote, true)
  }

  #[cfg(any(unix, windows, test))]
  fn native_action_with_retry(
    &self,
    action_key: &str,
    accepted_remote: Option<&crate::compiler::native_cache::RemoteAuthorityId>,
    retry_after_race: bool,
  ) -> RailResult<NativeActionLookup<'_>> {
    let lock = self.read_lock()?;
    if validate_native_ledger(&self.root)?.disabled {
      return Ok(NativeActionLookup::Miss(NativeCacheMiss {
        reason: "native_authority_ledger_full".to_string(),
        bytes_read: 0,
      }));
    }
    let action_hex = validated_id_hex(action_key, crate::compiler::native_cache::ACTION_KEY_PREFIX)?;
    let path = self
      .root
      .join(NATIVE_ACTION_STATE_DIRECTORY)
      .join(format!("{action_hex}.json"));
    match fs::symlink_metadata(&path) {
      Ok(_) => {}
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
        return Ok(NativeActionLookup::Miss(NativeCacheMiss {
          reason: "action_not_found".to_string(),
          bytes_read: 0,
        }));
      }
      Err(error) => return Err(error.into()),
    }
    let mut stats = ReadStats::default();
    let state_bytes = match read_bounded_file(&path, MAX_OBJECT_METADATA_BYTES, &mut stats) {
      Ok(bytes) => bytes,
      Err(fault) => {
        let evidence = fault.reason.into_bytes();
        drop(lock);
        let quarantined =
          self.quarantine_native_action_if_invalid(action_key, NativeStateFault::Unreadable, &evidence)?;
        if !quarantined && retry_after_race {
          return self.native_action_with_retry(action_key, accepted_remote, false);
        }
        return Ok(NativeActionLookup::Miss(NativeCacheMiss {
          reason: "action_quarantined".to_string(),
          bytes_read: stats.bytes,
        }));
      }
    };
    let state = match decode_native_action_state(&state_bytes, action_key) {
      Ok(state) => state,
      Err(fault) => {
        drop(lock);
        let quarantined = self.quarantine_native_action_if_invalid(action_key, fault, &state_bytes)?;
        if !quarantined && retry_after_race {
          return self.native_action_with_retry(action_key, accepted_remote, false);
        }
        return Ok(NativeActionLookup::Miss(NativeCacheMiss {
          reason: "action_quarantined".to_string(),
          bytes_read: stats.bytes,
        }));
      }
    };
    let NativeActionStateKind::UniqueResult {
      result_key,
      action_result,
      origins,
    } = &state.state
    else {
      return Ok(NativeActionLookup::Miss(NativeCacheMiss {
        reason: if matches!(&state.state, NativeActionStateKind::ConflictedResults { .. }) {
          "action_conflicted"
        } else {
          "action_quarantined"
        }
        .to_string(),
        bytes_read: stats.bytes,
      }));
    };
    if !origins.local
      && origins.remote.as_deref() != accepted_remote.map(crate::compiler::native_cache::RemoteAuthorityId::as_str)
    {
      return Ok(NativeActionLookup::Miss(NativeCacheMiss {
        reason: "action_origin_unaccepted".to_string(),
        bytes_read: stats.bytes,
      }));
    }
    let verified = self
      .load_verified_result(action_key, action_result, &mut stats)
      .map_err(fault_to_error)?;
    if verified.object.lookup_key != action_key || verified.object.result_digest != *result_key {
      return Err(RailError::message(
        "local CAS native action state does not match its verified result",
      ));
    }
    let validation = match &verified.validation {
      StoredValidation::NativeCompiler(validation) => validation.as_ref().clone(),
      StoredValidation::Hermetic(_) => {
        return Err(RailError::message(
          "local CAS native action has the wrong validation domain",
        ));
      }
    };
    validation.validate_object()?;
    if validation.action_key() != action_key || validation.result_key() != result_key {
      return Err(RailError::message(
        "local CAS native descriptor does not match its action state",
      ));
    }
    Ok(NativeActionLookup::Hit(Box::new(NativeActionHit {
      validation,
      bytes_read: stats.bytes,
      cas: self,
      _lock: lock,
      verified,
    })))
  }

  /// Preserve evidence that an unreadable state may already have encoded a conflict.
  /// Returns `false` only when a concurrent writer installed a valid state first.
  #[cfg(any(unix, windows, test))]
  fn quarantine_native_action_if_invalid(
    &self,
    action_key: &str,
    observed_fault: NativeStateFault,
    observed_evidence: &[u8],
  ) -> RailResult<bool> {
    let _lock = self.lock()?;
    let action_hex = validated_id_hex(action_key, crate::compiler::native_cache::ACTION_KEY_PREFIX)?;
    let path = self
      .root
      .join(NATIVE_ACTION_STATE_DIRECTORY)
      .join(format!("{action_hex}.json"));
    let (fault, evidence) = match fs::symlink_metadata(&path) {
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => (observed_fault, observed_evidence.to_vec()),
      Err(error) => (NativeStateFault::Unreadable, error.to_string().into_bytes()),
      Ok(metadata)
        if metadata.is_file()
          && !is_link_or_reparse(&metadata)
          && has_single_link(&metadata)
          && metadata.len() <= MAX_OBJECT_METADATA_BYTES =>
      {
        match fs::read(&path) {
          Ok(bytes) => match decode_native_action_state(&bytes, action_key) {
            Ok(_) => return Ok(false),
            Err(fault) => (fault, bytes),
          },
          Err(error) => (NativeStateFault::Unreadable, error.to_string().into_bytes()),
        }
      }
      Ok(_) => (NativeStateFault::Unreadable, observed_evidence.to_vec()),
    };
    let quarantined = quarantined_native_action_state(action_key, fault, &evidence);
    self.publish_terminal_native_state(&path, &quarantined)?;
    Ok(true)
  }

  /// Load fully verified compiler evidence discovered by one non-authoritative configuration key.
  pub(crate) fn compiler_evidence_candidates(&self, candidate_key: &str) -> RailResult<Vec<CompilerEvidenceCandidate>> {
    let _lock = self.read_lock()?;
    let candidate_hex = validated_id_hex(candidate_key, EVIDENCE_CANDIDATE_KEY_PREFIX)?;
    let directory = self.root.join(EVIDENCE_CANDIDATE_INDEX_DIRECTORY).join(candidate_hex);
    let mut entries = match fs::read_dir(&directory) {
      Ok(entries) => entries.collect::<Result<Vec<_>, _>>()?,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
      Err(error) => return Err(error.into()),
    };
    validate_real_directory(&directory, "local CAS compiler-evidence candidate")?;
    entries.sort_by_key(|entry| entry.file_name());
    if entries.len() > MAX_CANDIDATE_PINS {
      return Err(RailError::message(format!(
        "local CAS compiler-evidence candidate has more than {MAX_CANDIDATE_PINS} actions; refusing an unbounded lookup"
      )));
    }

    let mut candidates = Vec::with_capacity(entries.len());
    for entry in entries {
      let path = entry.path();
      let metadata = fs::symlink_metadata(&path)?;
      if !metadata.is_file() || is_link_or_reparse(&metadata) || !has_single_link(&metadata) {
        return Err(RailError::message(format!(
          "local CAS compiler-evidence candidate entry '{}' is not a bounded regular file",
          path.display()
        )));
      }
      let file_name = entry
        .file_name()
        .into_string()
        .map_err(|_| RailError::message("local CAS compiler-evidence candidate entry has a non-UTF-8 name"))?;
      let action_hex = file_name.strip_suffix(".json").ok_or_else(|| {
        RailError::message(format!(
          "local CAS compiler-evidence candidate entry '{file_name}' has an invalid name"
        ))
      })?;
      let mut stats = ReadStats::default();
      let indexed: EvidenceCandidateIndexEntry =
        read_canonical_json(&path, MAX_OBJECT_METADATA_BYTES, &mut stats).map_err(fault_to_error)?;
      if indexed.version != EVIDENCE_CANDIDATE_INDEX_VERSION
        || indexed.candidate_key != candidate_key
        || validated_id_hex(&indexed.action_key, EVIDENCE_ACTION_KEY_PREFIX)? != action_hex
      {
        return Err(RailError::message(
          "local CAS compiler-evidence candidate entry does not match its directory and action key",
        ));
      }

      let pin_path = self.root.join("pins").join(&file_name);
      let pin_metadata = match fs::symlink_metadata(&pin_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
        Err(error) => return Err(error.into()),
      };
      if !pin_metadata.is_file() || is_link_or_reparse(&pin_metadata) || !has_single_link(&pin_metadata) {
        return Err(RailError::message(format!(
          "local CAS compiler-evidence pin '{}' is not a bounded regular file",
          pin_path.display()
        )));
      }
      let pin: ActionPin =
        read_canonical_json(&pin_path, MAX_OBJECT_METADATA_BYTES, &mut stats).map_err(fault_to_error)?;
      if pin.version != CAS_VERSION
        || pin.action_key != indexed.action_key
        || pin.lookup_key != candidate_key
        || validated_id_hex(&pin.action_result, ACTION_RESULT_PREFIX).is_err()
      {
        return Err(RailError::message(
          "local CAS compiler-evidence pin does not match its discovery entry",
        ));
      }
      let verified = self
        .load_verified_compiler_evidence(&pin.action_key, &pin.action_result, &mut stats)
        .map_err(fault_to_error)?;
      if verified.validation.candidate_key() != candidate_key {
        return Err(RailError::message(
          "local CAS compiler-evidence result does not match its discovery key",
        ));
      }
      candidates.push(CompilerEvidenceCandidate {
        validation: verified.validation,
        evidence: verified.evidence,
        created_unix_nanos: pin.created_unix_nanos,
      });
    }
    candidates.sort_unstable_by_key(|candidate| std::cmp::Reverse(candidate.created_unix_nanos));
    Ok(candidates)
  }

  pub(super) fn store(&self, request: StoreRequest<'_>) -> RailResult<StoreStats> {
    let StoreRequest {
      action_key,
      lookup_key,
      result_digest,
      manifest,
      validation,
      compiler_units,
      source_root,
    } = request;
    validate_action_key(action_key)?;
    validate_lookup_key(lookup_key)?;
    validation.validate_object()?;
    if validation.action_key != action_key || validation.lookup_key != lookup_key {
      return Err(RailError::message(
        "local cache validation manifest does not match the stored action",
      ));
    }
    self.store_validated(ValidatedStoreRequest {
      action_key,
      lookup_key,
      result_digest,
      manifest,
      validation: StoredValidationRef::Hermetic(validation),
      compiler_units,
      source_root,
      native_origins: None,
      move_preverified_blobs: false,
      before_authority: None,
    })
  }

  pub(crate) fn store_native(
    &self,
    prepared: PreparedNativeResult,
  ) -> RailResult<(NativeCompilerValidation, StoreStats)> {
    self.store_native_revalidated(prepared, |_| Ok(()))
  }

  /// Admit one verified native result only after its live action is revalidated
  /// at the final boundary before durable authority publication.
  pub(crate) fn store_native_revalidated<F>(
    &self,
    prepared: PreparedNativeResult,
    mut revalidate: F,
  ) -> RailResult<(NativeCompilerValidation, StoreStats)>
  where
    F: FnMut(&NativeCompilerValidation) -> RailResult<()>,
  {
    let staged = self.stage_native(prepared)?;
    revalidate(staged.validation())?;
    self.commit_staged_native(staged)
  }

  /// Prepare one native result completely without publishing payload or
  /// action authority. This is intentionally safe to overlap with rustc.
  pub(crate) fn stage_native(&self, prepared: PreparedNativeResult) -> RailResult<StagedNativeResult> {
    let (staging, _staging_lock, manifest, validation, origin, move_preverified_blobs) = prepared.into_parts();
    if validate_native_ledger(&self.root)?.disabled {
      return Err(RailError::with_help(
        "native cache authority is disabled because its terminal-state ledger is full",
        "run `cargo rail cache clean --scope local` to explicitly reset the complete authority root",
      ));
    }
    let source_root = staging.path();
    let action_key = validation.action_key().to_string();
    let result_digest = validation.result_key().to_string();
    crate::compiler::native_cache::validate_action_key(&action_key)?;
    crate::compiler::native_cache::validate_result_key(&result_digest)?;
    validation.validate_object()?;
    if !validation.is_authoritative() {
      return Err(RailError::message(
        "local CAS rejected a discovery-only native compiler session",
      ));
    }
    validate_native_output_manifest(&manifest, &validation).map_err(fault_to_error)?;
    let native_origins = match origin {
      PreparedNativeOrigin::Local => NativeResultOrigins {
        local: true,
        remote: None,
      },
      PreparedNativeOrigin::Remote(authority) => NativeResultOrigins {
        local: false,
        remote: Some(authority.as_str().to_string()),
      },
    };
    if !move_preverified_blobs {
      manifest.validate_unchanged(source_root)?;
    }
    let prepared = prepare_tree(&manifest, source_root).map_err(fault_to_error)?;
    let manifest_bytes = canonical_json(&manifest)?;
    let validation_bytes = canonical_json(&StoredValidationRef::NativeCompiler(&validation))?;
    let validation_id = validation_id(&validation_bytes);
    let object = ActionResultObject {
      version: CAS_VERSION,
      action_key: action_key.clone(),
      lookup_key: action_key,
      result_digest,
      output_manifest: Some(manifest.digest.clone()),
      output_tree: Some(prepared.root.clone()),
      validation: validation_id,
      compiler_units: Some(1),
      compiler_evidence: None,
    };
    let object_bytes = canonical_json(&object)?;
    let action_result = action_result_id(&object)?;
    let estimated = estimate_result_bytes(
      &prepared,
      manifest_bytes.len(),
      validation_bytes.len(),
      object_bytes.len(),
    )?;
    let staged = self
      .stage_bundle(BundlePublication {
        object: &object,
        object_bytes: &object_bytes,
        manifest: &manifest,
        manifest_bytes: &manifest_bytes,
        validation: StoredValidationRef::NativeCompiler(&validation),
        validation_bytes: &validation_bytes,
        prepared: &prepared,
        move_preverified_blobs,
      })
      .map_err(|error| RailError::message(format!("local CAS bundle preparation failed: {error}")))?;
    let incoming = estimated.max(staged.stats.bytes_written);
    if incoming > MAX_RESULT_BYTES || incoming > self.max_bytes {
      return Err(RailError::message(format!(
        "verified action result is {incoming} bytes, above the local CAS limit"
      )));
    }
    Ok(StagedNativeResult {
      validation,
      origins: native_origins,
      object,
      action_result,
      incoming,
      staged,
    })
  }

  fn commit_staged_native(&self, staged: StagedNativeResult) -> RailResult<(NativeCompilerValidation, StoreStats)> {
    let StagedNativeResult {
      validation,
      origins,
      object,
      action_result,
      incoming,
      staged,
    } = staged;
    let _lock = self.lock()?;
    if validate_native_ledger(&self.root)?.disabled {
      return Err(RailError::with_help(
        "native cache authority is disabled because its terminal-state ledger is full",
        "run `cargo rail cache clean --scope local` to explicitly reset the complete authority root",
      ));
    }
    self.reserve_result_capacity(incoming, Some(&action_result))?;
    let mut stats = self.publish_staged_bundle(&action_result, &object, staged, true)?;
    self.publish_native_action_state(
      validation.action_key(),
      validation.result_key(),
      &action_result,
      origins,
    )?;
    if stats.bytes_written != incoming {
      self.settle_result_capacity(incoming, stats.bytes_written)?;
    }
    stats.action_result = Some(action_result);
    Ok((validation, stats))
  }

  /// Commit one discovery-only cohort after every result has been staged and
  /// revalidated. Only the addressed actions and result payloads must remain
  /// absent under the exclusive lifecycle view, so bounded cohorts can overlap
  /// their durability work with the remaining rustc DAG.
  pub(crate) fn commit_new_native_batch(
    &self,
    staged: Vec<StagedNativeResult>,
  ) -> RailResult<Vec<(NativeCompilerValidation, StoreStats)>> {
    if staged.is_empty() {
      return Ok(Vec::new());
    }
    let mut actions = BTreeSet::new();
    let mut total = 0u64;
    for result in &staged {
      result.validation.validate_object()?;
      if !result.validation.is_authoritative()
        || !result.origins.local
        || result.origins.remote.is_some()
        || !actions.insert(result.validation.action_key().to_string())
        || result.incoming != result.staged.stats.bytes_written
      {
        return Err(RailError::message(
          "native discovery batch is not one exact local result per action",
        ));
      }
      total = total
        .checked_add(result.incoming)
        .ok_or_else(|| RailError::message("native discovery batch size overflow"))?;
    }
    if total > self.max_bytes {
      return Err(RailError::message(format!(
        "verified native batch is {total} bytes, above the local CAS limit"
      )));
    }

    let _lock = self.lock()?;
    if validate_native_ledger(&self.root)?.disabled {
      return Err(RailError::with_help(
        "native cache authority is disabled because its terminal-state ledger is full",
        "run `cargo rail cache clean --scope local` to explicitly reset the complete authority root",
      ));
    }
    let action_directory = self.root.join(NATIVE_ACTION_STATE_DIRECTORY);
    validate_real_directory(&action_directory, "local CAS native action state")?;
    for result in &staged {
      let action_hex = validated_action_key_hex(result.validation.action_key())?;
      let action_state = action_directory.join(format!("{action_hex}.json"));
      match fs::symlink_metadata(action_state) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
        Ok(_) => {
          return Err(RailError::message(
            "native discovery cohort encountered pre-existing action authority",
          ));
        }
      }
      let destination = result_path(&self.root, &result.action_result)?;
      match fs::symlink_metadata(destination) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
        Ok(_) => {
          return Err(RailError::message(
            "native discovery cohort encountered pre-existing result payload",
          ));
        }
      }
    }

    self.reserve_result_capacity(total, None)?;
    let mut published = Vec::with_capacity(staged.len());
    for result in staged {
      let StagedNativeResult {
        validation,
        origins,
        object,
        action_result,
        incoming: _,
        staged,
      } = result;
      let mut stats = self.publish_staged_bundle(&action_result, &object, staged, false)?;
      if stats.bytes_written == 0 {
        return Err(RailError::message(
          "native discovery batch result became visible concurrently",
        ));
      }
      stats.action_result = Some(action_result.clone());
      published.push((validation, origins, action_result, stats));
    }
    sync_directory_before_commit(&self.root.join("results"))?;

    let mut pending_states = Vec::with_capacity(published.len());
    for (validation, origins, action_result, _) in &published {
      let state = NativeActionState {
        version: NATIVE_ACTION_STATE_VERSION,
        action_key: validation.action_key().to_string(),
        state: NativeActionStateKind::UniqueResult {
          result_key: validation.result_key().to_string(),
          action_result: action_result.clone(),
          origins: origins.clone(),
        },
      };
      validate_native_action_state(&state, validation.action_key())?;
      let action_hex = validated_action_key_hex(validation.action_key())?;
      let destination = action_directory.join(format!("{action_hex}.json"));
      let mut temporary = tempfile::Builder::new()
        .prefix(".cargo-rail-native-action-")
        .suffix(".tmp")
        .tempfile_in(&action_directory)?;
      temporary.write_all(&canonical_json(&state)?)?;
      sync_before_commit(temporary.as_file())?;
      pending_states.push((temporary, destination));
    }
    for (temporary, destination) in pending_states {
      persist_noclobber_committed(temporary, &destination).map_err(|error| {
        RailError::message(format!(
          "failed to publish native discovery action '{}': {}",
          destination.display(),
          error.error
        ))
      })?;
    }
    sync_directory_before_commit(&action_directory)?;

    Ok(
      published
        .into_iter()
        .map(|(validation, _, _, stats)| (validation, stats))
        .collect(),
    )
  }

  /// Attach one already authenticated remote origin to verified semantic bytes
  /// without fetching or rewriting their immutable payload.
  pub(crate) fn attach_remote_origin(
    &self,
    action_key: &str,
    result_key: &str,
    authority: &crate::compiler::native_cache::RemoteAuthorityId,
  ) -> RailResult<bool> {
    crate::compiler::native_cache::validate_action_key(action_key)?;
    crate::compiler::native_cache::validate_result_key(result_key)?;
    let _lock = self.lock()?;
    if validate_native_ledger(&self.root)?.disabled {
      return Ok(false);
    }
    let action_hex = validated_action_key_hex(action_key)?;
    let path = self
      .root
      .join(NATIVE_ACTION_STATE_DIRECTORY)
      .join(format!("{action_hex}.json"));
    let metadata = match fs::symlink_metadata(&path) {
      Ok(metadata) => metadata,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
      Err(error) => return Err(error.into()),
    };
    if !metadata.is_file()
      || is_link_or_reparse(&metadata)
      || !has_single_link(&metadata)
      || metadata.len() > MAX_OBJECT_METADATA_BYTES
    {
      return Ok(false);
    }
    let bytes = fs::read(&path)?;
    let state = match decode_native_action_state(&bytes, action_key) {
      Ok(state) => state,
      Err(_) => return Ok(false),
    };
    let NativeActionStateKind::UniqueResult {
      result_key: stored_result,
      action_result,
      mut origins,
    } = state.state
    else {
      return Ok(false);
    };
    if stored_result != result_key {
      return Ok(false);
    }
    let mut stats = ReadStats::default();
    let verified = match self.load_verified_result(action_key, &action_result, &mut stats) {
      Ok(verified) => verified,
      Err(_) => return Ok(false),
    };
    if verified.object.result_digest != result_key {
      return Ok(false);
    }
    origins.remote = Some(authority.as_str().to_string());
    let updated = NativeActionState {
      version: NATIVE_ACTION_STATE_VERSION,
      action_key: action_key.to_string(),
      state: NativeActionStateKind::UniqueResult {
        result_key: result_key.to_string(),
        action_result,
        origins,
      },
    };
    write_file_atomic_committed(&path, &canonical_json(&updated)?)?;
    Ok(true)
  }

  /// Revalidate a bounded event handle before the parent attempts remote
  /// publication. The caller receives no path and must drop this read view
  /// before performing any remote I/O.
  pub(crate) fn native_result_needs_remote_publication(
    &self,
    action_key: &str,
    result_key: &str,
    authority: &crate::compiler::native_cache::RemoteAuthorityId,
  ) -> RailResult<bool> {
    crate::compiler::native_cache::validate_action_key(action_key)?;
    crate::compiler::native_cache::validate_result_key(result_key)?;
    let _lock = self.read_lock()?;
    if validate_native_ledger(&self.root)?.disabled {
      return Ok(false);
    }
    let action_hex = validated_action_key_hex(action_key)?;
    let path = self
      .root
      .join(NATIVE_ACTION_STATE_DIRECTORY)
      .join(format!("{action_hex}.json"));
    let mut stats = ReadStats::default();
    let state: NativeActionState = match read_canonical_json(&path, MAX_OBJECT_METADATA_BYTES, &mut stats) {
      Ok(state) => state,
      Err(_) => return Ok(false),
    };
    validate_native_action_state(&state, action_key)?;
    let NativeActionStateKind::UniqueResult {
      result_key: stored_result,
      action_result,
      origins,
    } = state.state
    else {
      return Ok(false);
    };
    if stored_result != result_key || !origins.local || origins.remote.as_deref() == Some(authority.as_str()) {
      return Ok(false);
    }
    let verified = match self.load_verified_result(action_key, &action_result, &mut stats) {
      Ok(verified) => verified,
      Err(_) => return Ok(false),
    };
    Ok(
      verified.object.result_digest == result_key && matches!(verified.validation, StoredValidation::NativeCompiler(_)),
    )
  }

  /// Persist authenticated remote nondeterminism through the same terminal
  /// action-state transition used by local admission.
  pub(crate) fn record_remote_conflict(
    &self,
    action_key: &str,
    first_result: &str,
    second_result: &str,
  ) -> RailResult<()> {
    crate::compiler::native_cache::validate_action_key(action_key)?;
    crate::compiler::native_cache::validate_result_key(first_result)?;
    crate::compiler::native_cache::validate_result_key(second_result)?;
    if first_result == second_result {
      return Err(RailError::message(
        "remote conflict evidence repeats one result identity",
      ));
    }
    let _lock = self.lock()?;
    if validate_native_ledger(&self.root)?.disabled {
      return Err(RailError::message("native cache terminal-state ledger is disabled"));
    }
    let action_hex = validated_action_key_hex(action_key)?;
    let path = self
      .root
      .join(NATIVE_ACTION_STATE_DIRECTORY)
      .join(format!("{action_hex}.json"));
    let existing = match fs::symlink_metadata(&path) {
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
      Err(error) => return Err(error.into()),
      Ok(metadata)
        if metadata.is_file()
          && !is_link_or_reparse(&metadata)
          && has_single_link(&metadata)
          && metadata.len() <= MAX_OBJECT_METADATA_BYTES =>
      {
        let bytes = fs::read(&path)?;
        match decode_native_action_state(&bytes, action_key) {
          Ok(state) => Some(state),
          Err(fault) => {
            let quarantined = quarantined_native_action_state(action_key, fault, &bytes);
            self.publish_terminal_native_state(&path, &quarantined)?;
            return Ok(());
          }
        }
      }
      Ok(_) => {
        let quarantined = quarantined_native_action_state(action_key, NativeStateFault::Unreadable, &[]);
        self.publish_terminal_native_state(&path, &quarantined)?;
        return Ok(());
      }
    };
    if existing.as_ref().is_some_and(|state| {
      matches!(
        state.state,
        NativeActionStateKind::ConflictedResults { .. } | NativeActionStateKind::Quarantined { .. }
      )
    }) {
      return Ok(());
    }
    let mut results = vec![first_result.to_string(), second_result.to_string()];
    if let Some(NativeActionState {
      state: NativeActionStateKind::UniqueResult { result_key, .. },
      ..
    }) = existing
    {
      results.push(result_key);
    }
    results.sort_unstable();
    results.dedup();
    let conflicted = NativeActionState {
      version: NATIVE_ACTION_STATE_VERSION,
      action_key: action_key.to_string(),
      state: NativeActionStateKind::ConflictedResults {
        first_result_key: results[0].clone(),
        second_result_key: results[1].clone(),
      },
    };
    self.publish_terminal_native_state(&path, &conflicted)
  }

  /// Publish one deterministic compiler-evidence object through the shared local lifecycle.
  pub(crate) fn store_compiler_evidence(&self, request: CompilerEvidenceStoreRequest<'_>) -> RailResult<StoreStats> {
    let _lock = self.lock()?;
    let CompilerEvidenceStoreRequest { validation, evidence } = request;
    validation.validate_object()?;
    validate_evidence_action_key(validation.action_key())?;
    validate_evidence_candidate_key(validation.candidate_key())?;
    let evidence_id = evidence.identity()?;
    validate_evidence_object(&evidence_id)?;
    let result_digest = validation.result_digest(&evidence_id);
    let validation_bytes = canonical_json(validation)?;
    let validation_id = validation_id(&validation_bytes);
    let evidence_bytes = canonical_json(evidence)?;
    if evidence_bytes.len() as u64 > MAX_OBJECT_METADATA_BYTES {
      return Err(RailError::message(
        "compiler evidence exceeds the local CAS metadata-object bound",
      ));
    }
    let object = ActionResultObject {
      version: CAS_VERSION,
      action_key: validation.action_key().to_string(),
      lookup_key: validation.candidate_key().to_string(),
      result_digest,
      output_manifest: None,
      output_tree: None,
      validation: validation_id,
      compiler_units: None,
      compiler_evidence: Some(evidence_id),
    };
    let object_bytes = canonical_json(&object)?;
    let action_result = action_result_id(&object)?;
    let result_hex = validated_id_hex(&action_result, ACTION_RESULT_PREFIX)?;
    let destination = self.root.join("results").join(result_hex);
    let incoming = match fs::symlink_metadata(&destination) {
      Ok(metadata) if metadata.is_dir() && !is_link_or_reparse(&metadata) => 0,
      Ok(_) => {
        return Err(RailError::message(
          "local CAS compiler-evidence result path is not a real directory",
        ));
      }
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => object_bytes
        .len()
        .checked_add(validation_bytes.len())
        .and_then(|bytes| bytes.checked_add(evidence_bytes.len()))
        .map(|bytes| bytes as u64)
        .ok_or_else(|| RailError::message("local CAS compiler-evidence result size overflow"))?,
      Err(error) => return Err(error.into()),
    };
    if incoming > MAX_RESULT_BYTES || incoming > self.max_bytes {
      return Err(RailError::message(format!(
        "verified compiler evidence is {incoming} bytes, above the local CAS limit"
      )));
    }
    self.reserve_result_capacity(incoming, Some(&action_result))?;
    let lease = self.create_lease(&action_result)?;
    let mut stats = self.publish_compiler_evidence_bundle(CompilerEvidencePublication {
      action_result: &action_result,
      object: &object,
      object_bytes: &object_bytes,
      validation,
      validation_bytes: &validation_bytes,
      evidence,
      evidence_bytes: &evidence_bytes,
    })?;
    self.publish_pin(validation.action_key(), validation.candidate_key(), &action_result)?;
    self.publish_compiler_evidence_candidate_index(validation.action_key(), validation.candidate_key())?;
    drop(lease);
    self.settle_result_capacity(incoming, stats.bytes_written)?;
    stats.action_result = Some(action_result);
    Ok(stats)
  }

  fn store_validated(&self, request: ValidatedStoreRequest<'_>) -> RailResult<StoreStats> {
    let ValidatedStoreRequest {
      action_key,
      lookup_key,
      result_digest,
      manifest,
      validation,
      compiler_units,
      source_root,
      native_origins,
      move_preverified_blobs,
      mut before_authority,
    } = request;
    validate_manifest(manifest).map_err(fault_to_error)?;
    if !move_preverified_blobs {
      manifest.validate_unchanged(source_root)?;
    }
    let prepared = prepare_tree(manifest, source_root).map_err(fault_to_error)?;
    let manifest_bytes = canonical_json(manifest)?;
    let manifest_id = manifest.digest.clone();
    let validation_bytes = canonical_json(&validation)?;
    let validation_id = validation_id(&validation_bytes);
    let object = ActionResultObject {
      version: CAS_VERSION,
      action_key: action_key.to_string(),
      lookup_key: lookup_key.to_string(),
      result_digest: result_digest.to_string(),
      output_manifest: Some(manifest_id),
      output_tree: Some(prepared.root.clone()),
      validation: validation_id,
      compiler_units: Some(compiler_units),
      compiler_evidence: None,
    };
    let object_bytes = canonical_json(&object)?;
    let action_result = action_result_id(&object)?;
    let estimated = estimate_result_bytes(
      &prepared,
      manifest_bytes.len(),
      validation_bytes.len(),
      object_bytes.len(),
    )?;
    let staged = self
      .stage_bundle(BundlePublication {
        object: &object,
        object_bytes: &object_bytes,
        manifest,
        manifest_bytes: &manifest_bytes,
        validation,
        validation_bytes: &validation_bytes,
        prepared: &prepared,
        move_preverified_blobs,
      })
      .map_err(|error| RailError::message(format!("local CAS bundle preparation failed: {error}")))?;
    let incoming = estimated.max(staged.stats.bytes_written);
    if incoming > MAX_RESULT_BYTES || incoming > self.max_bytes {
      return Err(RailError::message(format!(
        "verified action result is {incoming} bytes, above the local CAS limit"
      )));
    }
    if let Some(revalidate) = &mut before_authority {
      revalidate()?;
    }
    let _lock = self.lock()?;
    self
      .reserve_result_capacity(incoming, Some(&action_result))
      .map_err(|error| RailError::message(format!("local CAS pre-publication capacity check failed: {error}")))?;
    // The exclusive lifecycle lock remains held through payload publication,
    // authority commit, and capacity settlement. A result lease inside the
    // same interval adds no protection and would force two durable writes per
    // compiler action.
    let mut stats = self
      .publish_staged_bundle(&action_result, &object, staged, true)
      .map_err(|error| RailError::message(format!("local CAS bundle publication failed: {error}")))?;
    match validation {
      StoredValidationRef::NativeCompiler(validation) => self
        .publish_native_action_state(
          action_key,
          validation.result_key(),
          &action_result,
          native_origins.ok_or_else(|| RailError::message("native CAS admission omitted its result origin"))?,
        )
        .map_err(|error| RailError::message(format!("local CAS native action publication failed: {error}")))?,
      StoredValidationRef::Hermetic(_) => self
        .publish_pin(action_key, lookup_key, &action_result)
        .map_err(|error| RailError::message(format!("local CAS pin publication failed: {error}")))?,
    }
    if stats.bytes_written != incoming {
      self
        .settle_result_capacity(incoming, stats.bytes_written)
        .map_err(|error| RailError::message(format!("local CAS capacity settlement failed: {error}")))?;
    }
    stats.action_result = Some(action_result);
    Ok(stats)
  }

  #[cfg(any(target_os = "macos", test))]
  fn restore_inner(
    &self,
    action_key: &str,
    destination: &Path,
    stats: &mut ReadStats,
  ) -> Result<VerifiedResult, Fault> {
    let _lock = self
      .read_lock()
      .map_err(|error| Fault::corrupt(format!("lifecycle_lock_unavailable: {error}")))?;
    let key_hex = validated_action_key_hex(action_key).map_err(|error| Fault::corrupt(error.to_string()))?;
    let pin_path = self.root.join("pins").join(format!("{key_hex}.json"));
    let pin_metadata = match fs::symlink_metadata(&pin_path) {
      Ok(metadata) => metadata,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
        return Err(Fault::miss("action_not_found"));
      }
      Err(error) => return Err(Fault::corrupt(format!("pin_unreadable: {error}"))),
    };
    if !pin_metadata.is_file() || is_link_or_reparse(&pin_metadata) || !has_single_link(&pin_metadata) {
      return Err(Fault::corrupt("pin_not_regular_file"));
    }
    let pin: ActionPin = read_canonical_json(&pin_path, MAX_OBJECT_METADATA_BYTES, stats)?;
    if pin.version != CAS_VERSION {
      return Err(Fault::incompatible("pin_schema_version"));
    }
    if pin.action_key != action_key {
      return Err(Fault::corrupt("pin_action_key_mismatch"));
    }
    validate_any_lookup_key(&pin.lookup_key).map_err(|_| Fault::corrupt("pin_lookup_identity"))?;
    validated_id_hex(&pin.action_result, ACTION_RESULT_PREFIX)
      .map_err(|_| Fault::corrupt("pin_action_result_identity"))?;
    let _lease = self
      .create_lease(&pin.action_result)
      .map_err(|error| Fault::corrupt(format!("lease_unavailable: {error}")))?;
    let verified = self.load_verified_result(action_key, &pin.action_result, stats)?;
    if verified.object.lookup_key != pin.lookup_key {
      return Err(Fault::corrupt("pin_lookup_binding_mismatch"));
    }
    self.materialize(&verified, destination, stats)?;
    let _ = OpenOptions::new()
      .read(true)
      .write(true)
      .open(&pin_path)
      .and_then(|file| file.set_modified(SystemTime::now()));
    Ok(verified)
  }
}

struct VerifiedResult {
  #[cfg(any(unix, windows, test))]
  action_result: String,
  #[cfg(any(unix, windows, test))]
  object: ActionResultObject,
  #[cfg(any(unix, windows, test))]
  manifest: OutputManifest,
  #[cfg(any(unix, windows, test))]
  validation: StoredValidation,
  #[cfg(any(unix, windows, test))]
  trees: BTreeMap<String, TreeObject>,
  #[cfg(any(unix, windows, test))]
  bundle: PathBuf,
}

struct VerifiedCompilerEvidence {
  validation: CompilerEvidenceValidation,
  evidence: CompilerEvidenceObject,
}

fn cache_base() -> RailResult<PathBuf> {
  if let Some(base) = std::env::var_os(CACHE_BASE_ENV) {
    if base.is_empty() {
      return Err(RailError::message(format!("{CACHE_BASE_ENV} must not be empty")));
    }
    return Ok(PathBuf::from(base));
  }
  if let Some(cargo_home) = std::env::var_os("CARGO_HOME")
    && !cargo_home.is_empty()
  {
    return Ok(PathBuf::from(cargo_home));
  }
  let home = std::env::var_os("HOME")
    .filter(|home| !home.is_empty())
    .ok_or_else(|| RailError::message("local CAS needs CARGO_RAIL_CACHE_DIR, CARGO_HOME, or HOME"))?;
  Ok(PathBuf::from(home).join(".cargo"))
}

fn selected_cache_authority(owner: &Path, create_default: bool) -> RailResult<SelectedCacheAuthority> {
  if let Some(value) = std::env::var_os(CACHE_TRUST_DOMAIN_ENV) {
    let trust_domain = value
      .to_str()
      .ok_or_else(|| RailError::message(format!("{CACHE_TRUST_DOMAIN_ENV} is not valid UTF-8")))?;
    validate_trust_domain(trust_domain)?;
    return Ok(SelectedCacheAuthority {
      root_name: format!("{CAS_ROOT_NAME}-{trust_domain}"),
      trust_domain: trust_domain.to_string(),
    });
  }
  let trust_domain = load_default_trust_domain(owner, create_default)?;
  Ok(SelectedCacheAuthority {
    root_name: CAS_ROOT_NAME.to_string(),
    trust_domain,
  })
}

fn load_default_trust_domain(owner: &Path, create: bool) -> RailResult<String> {
  let path = owner.join(DEFAULT_TRUST_DOMAIN_FILE);
  match fs::symlink_metadata(&path) {
    Ok(_) => return read_trust_domain_file(&path),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {}
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
      return Err(RailError::with_help(
        format!("local CAS trust-domain marker '{}' is missing", path.display()),
        "run an ordinary cacheable cargo-rail action to create a new isolated authority root",
      ));
    }
    Err(error) => return Err(error.into()),
  }
  let mut temporary = tempfile::NamedTempFile::new_in(owner)?;
  let seed = format!(
    "{}\0{}\0{}\0{}",
    owner.display(),
    temporary.path().display(),
    std::process::id(),
    unix_nanos()
  );
  let trust_domain = crate::source::ContentDigest::sha256(seed.as_bytes()).to_string();
  temporary.write_all(format!("{trust_domain}\n").as_bytes())?;
  sync_before_commit(temporary.as_file())?;
  match persist_noclobber_committed(temporary, &path) {
    Ok(_) => sync_directory_before_commit(owner)?,
    Err(error)
      if fs::symlink_metadata(&path)
        .is_ok_and(|metadata| metadata.is_file() && !is_link_or_reparse(&metadata) && has_single_link(&metadata)) =>
    {
      let _ = error;
    }
    Err(error) => {
      return Err(RailError::message(format!(
        "failed to create local CAS trust-domain marker '{}': {}",
        path.display(),
        error.error
      )));
    }
  }
  read_trust_domain_file(&path)
}

fn read_trust_domain_file(path: &Path) -> RailResult<String> {
  let metadata = fs::symlink_metadata(path)?;
  if !metadata.is_file() || is_link_or_reparse(&metadata) || !has_single_link(&metadata) || metadata.len() != 65 {
    return Err(RailError::message(format!(
      "local CAS trust-domain marker '{}' is not a private canonical file",
      path.display()
    )));
  }
  let bytes = fs::read(path)?;
  let value = bytes
    .strip_suffix(b"\n")
    .and_then(|value| std::str::from_utf8(value).ok())
    .ok_or_else(|| RailError::message("local CAS trust-domain marker is malformed"))?;
  validate_trust_domain(value)?;
  Ok(value.to_string())
}

fn validate_trust_domain(value: &str) -> RailResult<()> {
  if value.len() == 64
    && value
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
  {
    Ok(())
  } else {
    Err(RailError::message(format!(
      "{CACHE_TRUST_DOMAIN_ENV} must be a 64-character lowercase hexadecimal opaque ID"
    )))
  }
}

/// Resolve the selected user-wide CAS without creating cache state.
pub(super) fn configured_root() -> RailResult<Option<PathBuf>> {
  let base = cache_base()?;
  let base = match fs::canonicalize(&base) {
    Ok(base) => base,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error.into()),
  };
  validate_real_directory(&base, "local cache base")?;
  let owner = base.join("cargo-rail");
  match fs::symlink_metadata(&owner) {
    Ok(_) => validate_real_directory(&owner, "local CAS owner")?,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error.into()),
  }
  let owner = fs::canonicalize(owner)?;
  let authority = match selected_cache_authority(&owner, false) {
    Ok(authority) => authority,
    Err(_)
      if std::env::var_os(CACHE_TRUST_DOMAIN_ENV).is_none()
        && fs::symlink_metadata(owner.join(CAS_ROOT_NAME))
          .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
    {
      return Ok(None);
    }
    Err(error) => return Err(error),
  };
  let root = owner.join(authority.root_name);
  match fs::symlink_metadata(&root) {
    Ok(_) => {
      ensure_owner_marker_existing(&root, Some(&authority.trust_domain))?;
      Ok(Some(root))
    }
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
    Err(error) => Err(error.into()),
  }
}

pub(super) fn configured_legacy_root() -> RailResult<Option<PathBuf>> {
  let base = match fs::canonicalize(cache_base()?) {
    Ok(base) => base,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error.into()),
  };
  validate_real_directory(&base, "local cache base")?;
  let owner = base.join("cargo-rail");
  match fs::symlink_metadata(&owner) {
    Ok(_) => validate_real_directory(&owner, "local CAS owner")?,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error.into()),
  }
  let root = fs::canonicalize(owner)?.join(LEGACY_CAS_ROOT_NAME);
  match fs::symlink_metadata(&root) {
    Ok(_) => Ok(Some(root)),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
    Err(error) => Err(error.into()),
  }
}

fn cache_max_bytes() -> RailResult<u64> {
  let Some(value) = std::env::var_os(CACHE_MAX_BYTES_ENV) else {
    return Ok(DEFAULT_CACHE_MAX_BYTES);
  };
  let value = value
    .to_str()
    .ok_or_else(|| RailError::message(format!("{CACHE_MAX_BYTES_ENV} is not valid UTF-8")))?;
  let parsed = value
    .parse::<u64>()
    .map_err(|error| RailError::message(format!("invalid {CACHE_MAX_BYTES_ENV} value '{value}': {error}")))?;
  if parsed == 0 {
    return Err(RailError::message(format!("{CACHE_MAX_BYTES_ENV} must be positive")));
  }
  Ok(parsed)
}

fn create_real_directory(parent: &Path, name: &str) -> RailResult<PathBuf> {
  let path = parent.join(name);
  match fs::create_dir(&path) {
    Ok(()) => {}
    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
    Err(error) => {
      return Err(RailError::message(format!(
        "failed to create local CAS directory '{}': {error}",
        path.display()
      )));
    }
  }
  validate_real_directory(&path, "local CAS path")?;
  make_directory_private(&path)?;
  Ok(path)
}

#[cfg(unix)]
fn make_directory_private(path: &Path) -> RailResult<()> {
  use std::os::unix::fs::PermissionsExt as _;

  fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
  Ok(())
}

#[cfg(not(unix))]
fn make_directory_private(_path: &Path) -> RailResult<()> {
  Ok(())
}

fn validate_real_directory(path: &Path, description: &str) -> RailResult<()> {
  let metadata = fs::symlink_metadata(path)
    .map_err(|error| RailError::message(format!("failed to inspect {description} '{}': {error}", path.display())))?;
  if metadata.is_dir() && !is_link_or_reparse(&metadata) {
    Ok(())
  } else {
    Err(RailError::with_help(
      format!("{description} '{}' is not a real directory", path.display()),
      "remove the hostile path; cargo-rail will not follow cache symlinks",
    ))
  }
}

#[cfg(windows)]
fn prove_local_cache_volume(path: &Path) -> RailResult<()> {
  use cargo_rail_windows_fs::{observe_file, open_for_observation, prove_local_ntfs};

  const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0010;
  let opened = open_for_observation(path).map_err(|error| {
    RailError::message(format!(
      "failed to open local CAS root '{}' for capability proof: {error}",
      path.display()
    ))
  })?;
  let before = observe_file(&opened).map_err(|error| {
    RailError::message(format!(
      "failed to observe local CAS root '{}': {error}",
      path.display()
    ))
  })?;
  if before.file_attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
    return Err(RailError::message(format!(
      "local CAS root '{}' changed type during capability proof",
      path.display()
    )));
  }
  prove_local_ntfs(&opened, before.volume_serial_number).map_err(|error| {
    RailError::message(format!(
      "local CAS root '{}' is not on a proven local NTFS volume: {error}",
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
      "local CAS root '{}' changed while its capability was proven: {error}",
      path.display()
    ))
  })?;
  if current != before {
    return Err(RailError::message(format!(
      "local CAS root '{}' changed while its capability was proven",
      path.display()
    )));
  }
  Ok(())
}

#[cfg(not(windows))]
fn prove_local_cache_volume(_path: &Path) -> RailResult<()> {
  Ok(())
}

fn validate_optional_real_directory(path: &Path, description: &str) -> RailResult<()> {
  match fs::symlink_metadata(path) {
    Ok(_) => validate_real_directory(path, description),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
    Err(error) => Err(error.into()),
  }
}

fn ensure_owner_marker(root: &Path, trust_domain: &str) -> RailResult<()> {
  let marker = root.join("OWNER");
  match fs::symlink_metadata(&marker) {
    Ok(_) => return ensure_owner_marker_existing(root, Some(trust_domain)),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
    Err(error) => return Err(error.into()),
  }
  if fs::read_dir(root)?.next().transpose()?.is_some() {
    // Another first writer may have published the marker after our initial
    // check. Accept only that exact ownership transition; never adopt an
    // unrelated nonempty directory.
    if fs::symlink_metadata(&marker).is_ok() {
      return ensure_owner_marker_existing(root, Some(trust_domain));
    }
    return Err(RailError::with_help(
      format!(
        "local CAS root '{}' is nonempty but has no cargo-rail ownership marker",
        root.display()
      ),
      "choose an empty cache directory or remove the hostile pre-positioned path",
    ));
  }
  let parent = root
    .parent()
    .ok_or_else(|| RailError::message("local CAS root has no parent directory"))?;
  let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
  temporary.write_all(&owner_marker_bytes(trust_domain)?)?;
  sync_before_commit(temporary.as_file())?;
  match persist_noclobber_committed(temporary, &marker) {
    Ok(_) => {
      sync_directory_before_commit(root)?;
    }
    Err(_)
      if fs::symlink_metadata(&marker).is_ok_and(|metadata| metadata.is_file() && !is_link_or_reparse(&metadata)) => {}
    Err(error) => {
      return Err(RailError::message(format!(
        "failed to create local CAS ownership marker '{}': {}",
        marker.display(),
        error.error
      )));
    }
  }
  ensure_owner_marker_existing(root, Some(trust_domain))
}

fn validate_root_entries(root: &Path) -> RailResult<()> {
  let allowed = BTreeSet::from([
    CAPACITY_STATE_FILE,
    NATIVE_LEDGER_STATE_FILE,
    "OWNER",
    EVIDENCE_CANDIDATE_INDEX_DIRECTORY,
    "leases",
    NATIVE_ACTION_STATE_DIRECTORY,
    NATIVE_ENVIRONMENT_SELECTOR_DIRECTORY,
    LEGACY_NATIVE_ACTION_STATE_DIRECTORY,
    "pins",
    "results",
    "staging",
  ]);
  #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
  let allowed = {
    let mut allowed = allowed;
    allowed.insert(SYSROOT_IDENTITY_MEMO_DIRECTORY);
    allowed
  };
  for entry in fs::read_dir(root)? {
    let entry = entry?;
    let name = entry
      .file_name()
      .into_string()
      .map_err(|_| RailError::message("local CAS root contains a non-UTF-8 entry"))?;
    if !allowed.contains(name.as_str()) {
      return Err(RailError::with_help(
        format!("local CAS root '{}' contains unexpected entry '{name}'", root.display()),
        "remove the hostile entry or choose a different CARGO_RAIL_CACHE_DIR",
      ));
    }
  }
  Ok(())
}

fn validate_action_key(action_key: &str) -> RailResult<()> {
  validated_id_hex(action_key, ACTION_KEY_PREFIX).map(|_| ())
}

fn validate_lookup_key(lookup_key: &str) -> RailResult<()> {
  validated_id_hex(lookup_key, LOOKUP_PREFIX).map(|_| ())
}

fn encode_native_environment_selector(names: &[String]) -> RailResult<Vec<u8>> {
  crate::compiler::native_cache::validate_environment_selector_names(names.iter().map(String::as_str))?;
  let bytes = canonical_json(&names)?;
  if bytes.len() as u64 > MAX_NATIVE_ENVIRONMENT_SELECTOR_BYTES {
    return Err(RailError::message(format!(
      "native environment selector exceeds its {MAX_NATIVE_ENVIRONMENT_SELECTOR_BYTES}-byte bound"
    )));
  }
  Ok(bytes)
}

fn validated_action_key_hex(action_key: &str) -> RailResult<&str> {
  if action_key.starts_with(ACTION_KEY_PREFIX) {
    validated_id_hex(action_key, ACTION_KEY_PREFIX)
  } else if action_key.starts_with(EVIDENCE_ACTION_KEY_PREFIX) {
    validated_id_hex(action_key, EVIDENCE_ACTION_KEY_PREFIX)
  } else {
    validated_id_hex(action_key, crate::compiler::native_cache::ACTION_KEY_PREFIX)
  }
}

fn validate_any_lookup_key(lookup_key: &str) -> RailResult<()> {
  if lookup_key.starts_with(LOOKUP_PREFIX) {
    validate_lookup_key(lookup_key)
  } else if lookup_key.starts_with(EVIDENCE_CANDIDATE_KEY_PREFIX) {
    validate_evidence_candidate_key(lookup_key)
  } else {
    crate::compiler::native_cache::validate_action_key(lookup_key)
  }
}

fn validate_native_action_state(state: &NativeActionState, expected_action: &str) -> RailResult<()> {
  if state.version != NATIVE_ACTION_STATE_VERSION || state.action_key != expected_action {
    return Err(RailError::message(
      "local CAS native action state has an incompatible identity",
    ));
  }
  crate::compiler::native_cache::validate_action_key(&state.action_key)?;
  match &state.state {
    NativeActionStateKind::UniqueResult {
      result_key,
      action_result,
      origins,
    } => {
      crate::compiler::native_cache::validate_result_key(result_key)?;
      validated_id_hex(action_result, ACTION_RESULT_PREFIX)?;
      if !origins.local && origins.remote.is_none() {
        return Err(RailError::message("local CAS native action has no authority origin"));
      }
      if origins.remote.as_deref().is_some_and(|origin| {
        origin.strip_prefix("remote-authority-v1-sha256-").is_none_or(|digest| {
          digest.len() != 64
            || !digest
              .bytes()
              .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
      }) {
        return Err(RailError::message(
          "local CAS native action has an invalid remote origin",
        ));
      }
    }
    NativeActionStateKind::ConflictedResults {
      first_result_key,
      second_result_key,
    } => {
      crate::compiler::native_cache::validate_result_key(first_result_key)?;
      crate::compiler::native_cache::validate_result_key(second_result_key)?;
      if first_result_key >= second_result_key {
        return Err(RailError::message(
          "local CAS native conflict evidence is not canonical",
        ));
      }
    }
    NativeActionStateKind::Quarantined { evidence_digest, .. } => {
      validate_content_digest(evidence_digest).map_err(fault_to_error)?;
    }
  }
  Ok(())
}

fn quarantined_native_action_state(action_key: &str, fault: NativeStateFault, evidence: &[u8]) -> NativeActionState {
  NativeActionState {
    version: NATIVE_ACTION_STATE_VERSION,
    action_key: action_key.to_string(),
    state: NativeActionStateKind::Quarantined {
      fault,
      evidence_digest: format!("sha256:{}", crate::source::ContentDigest::sha256(evidence)),
    },
  }
}

fn decode_native_action_state(bytes: &[u8], expected_action: &str) -> Result<NativeActionState, NativeStateFault> {
  let state = serde_json::from_slice::<NativeActionState>(bytes).map_err(|_| NativeStateFault::Malformed)?;
  if state.version != NATIVE_ACTION_STATE_VERSION || state.action_key != expected_action {
    return Err(NativeStateFault::Incompatible);
  }
  validate_native_action_state(&state, expected_action).map_err(|_| NativeStateFault::Malformed)?;
  if canonical_json(&state).map_err(|_| NativeStateFault::Malformed)? != bytes {
    return Err(NativeStateFault::Malformed);
  }
  Ok(state)
}

pub(super) fn validate_fast_identity(action_key: &str, lookup_key: &str) -> RailResult<()> {
  validate_action_key(action_key)?;
  validate_lookup_key(lookup_key)
}

fn validated_id_hex<'a>(identity: &'a str, prefix: &str) -> RailResult<&'a str> {
  let hex = identity
    .strip_prefix(prefix)
    .ok_or_else(|| RailError::message(format!("identity '{identity}' has the wrong domain or version")))?;
  if hex.len() != 64
    || !hex
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
  {
    return Err(RailError::message(format!(
      "identity '{identity}' is not canonical SHA-256"
    )));
  }
  Ok(hex)
}

fn canonical_json<T: Serialize>(value: &T) -> RailResult<Vec<u8>> {
  serde_json::to_vec(value).map_err(Into::into)
}

fn framed_identity(domain: &[u8], frames: &[(&[u8], &[u8])]) -> String {
  let mut hasher = Sha256::new();
  hasher.update(domain);
  for (tag, value) in frames {
    hasher.update((tag.len() as u64).to_le_bytes());
    hasher.update(tag);
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
  }
  crate::instrumentation::record_hash_operation();
  let input_bytes = domain.len()
    + frames
      .iter()
      .map(|(tag, value)| 16usize.saturating_add(tag.len()).saturating_add(value.len()))
      .sum::<usize>();
  crate::instrumentation::record_hash_input_bytes(input_bytes);
  sha256_hex(hasher)
}

fn blob_id(content_digest: &str, bytes: u64) -> Result<String, Fault> {
  let content = content_digest
    .strip_prefix("sha256:")
    .ok_or_else(|| Fault::corrupt("file_content_digest_domain"))?;
  if content.len() != 64
    || !content
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
  {
    return Err(Fault::corrupt("file_content_digest_encoding"));
  }
  Ok(format!(
    "{BLOB_PREFIX}{}",
    framed_identity(
      b"cargo-rail-cas-blob\0",
      &[
        (b"version", CAS_VERSION.to_le_bytes().as_slice()),
        (b"bytes", bytes.to_le_bytes().as_slice()),
        (b"content-sha256", content.as_bytes()),
      ],
    )
  ))
}

fn tree_id(tree: &TreeObject) -> RailResult<String> {
  let version = tree.version.to_le_bytes();
  let entries = tree
    .entries
    .iter()
    .map(canonical_json)
    .collect::<RailResult<Vec<_>>>()?;
  let mut frames = Vec::<(&[u8], &[u8])>::with_capacity(entries.len() + 1);
  frames.push((b"version", &version));
  frames.extend(entries.iter().map(|entry| (b"entry".as_slice(), entry.as_slice())));
  Ok(format!(
    "{TREE_PREFIX}{}",
    framed_identity(b"cargo-rail-cas-tree\0", &frames)
  ))
}

fn action_result_id(object: &ActionResultObject) -> RailResult<String> {
  let version = object.version.to_le_bytes();
  match (
    object.output_manifest.as_deref(),
    object.output_tree.as_deref(),
    object.compiler_units,
    object.compiler_evidence.as_deref(),
  ) {
    (Some(output_manifest), Some(output_tree), Some(compiler_units), None) => {
      let compiler_units = (compiler_units as u64).to_le_bytes();
      Ok(format!(
        "{ACTION_RESULT_PREFIX}{}",
        framed_identity(
          b"cargo-rail-cas-action-result\0",
          &[
            (b"version", &version),
            (b"action-key", object.action_key.as_bytes()),
            (b"lookup-key", object.lookup_key.as_bytes()),
            (b"result-digest", object.result_digest.as_bytes()),
            (b"output-manifest", output_manifest.as_bytes()),
            (b"output-tree", output_tree.as_bytes()),
            (b"validation", object.validation.as_bytes()),
            (b"compiler-units", &compiler_units),
          ],
        )
      ))
    }
    (None, None, None, Some(evidence)) => Ok(format!(
      "{ACTION_RESULT_PREFIX}{}",
      framed_identity(
        b"cargo-rail-cas-compiler-evidence-result\0",
        &[
          (b"version", &version),
          (b"action-key", object.action_key.as_bytes()),
          (b"lookup-key", object.lookup_key.as_bytes()),
          (b"result-digest", object.result_digest.as_bytes()),
          (b"validation", object.validation.as_bytes()),
          (b"compiler-evidence", evidence.as_bytes()),
        ],
      )
    )),
    _ => Err(RailError::message(
      "local CAS action result contains an invalid typed payload",
    )),
  }
}

fn output_result_payload(object: &ActionResultObject) -> Result<(&str, &str, usize), Fault> {
  match (
    object.output_manifest.as_deref(),
    object.output_tree.as_deref(),
    object.compiler_units,
    object.compiler_evidence.as_deref(),
  ) {
    (Some(manifest), Some(tree), Some(compiler_units), None) => Ok((manifest, tree, compiler_units)),
    _ => Err(Fault::corrupt("action_result_output_payload_shape")),
  }
}

fn compiler_evidence_payload(object: &ActionResultObject) -> Result<&str, Fault> {
  match (
    object.output_manifest.as_deref(),
    object.output_tree.as_deref(),
    object.compiler_units,
    object.compiler_evidence.as_deref(),
  ) {
    (None, None, None, Some(evidence)) => Ok(evidence),
    _ => Err(Fault::corrupt("action_result_compiler_evidence_payload_shape")),
  }
}

fn validation_id(bytes: &[u8]) -> String {
  let version = CAS_VERSION.to_le_bytes();
  format!(
    "{VALIDATION_PREFIX}{}",
    framed_identity(
      b"cargo-rail-cas-validation\0",
      &[(b"version", &version), (b"manifest", bytes)],
    )
  )
}

fn fault_to_error(fault: Fault) -> RailError {
  RailError::message(format!(
    "local CAS {}: {}",
    match fault.kind {
      FaultKind::Miss => "miss",
      FaultKind::Corrupt => "corruption",
      FaultKind::Incompatible => "incompatibility",
    },
    fault.reason
  ))
}

fn validate_manifest(manifest: &OutputManifest) -> Result<(), Fault> {
  if manifest.version != super::OUTPUT_MANIFEST_VERSION {
    return Err(Fault::incompatible("output_manifest_schema_version"));
  }
  if manifest.entries.len() > MAX_ENTRIES {
    return Err(Fault::corrupt("output_manifest_entry_limit"));
  }
  let expected_digest = super::output_manifest_digest(&manifest.entries)
    .map_err(|error| Fault::corrupt(format!("output_manifest_identity: {error}")))?;
  if expected_digest != manifest.digest {
    return Err(Fault::corrupt("output_manifest_digest_mismatch"));
  }
  validated_id_hex(&manifest.digest, MANIFEST_PREFIX)
    .map_err(|_| Fault::corrupt("output_manifest_identity_encoding"))?;

  let mut files = 0usize;
  let mut directories = 0usize;
  let mut symlinks = 0usize;
  let mut bytes = 0u64;
  let mut previous = None::<&str>;
  let mut kinds = BTreeMap::<String, &'static str>::new();
  for entry in &manifest.entries {
    validate_logical_path(&entry.path)?;
    if previous.is_some_and(|previous| previous >= entry.path.as_str()) {
      return Err(Fault::corrupt("output_manifest_paths_not_strictly_sorted"));
    }
    previous = Some(&entry.path);
    if !entry.path.starts_with("target/")
      && entry.path != "target"
      && !entry.path.starts_with("build/")
      && entry.path != "build"
    {
      return Err(Fault::corrupt("output_manifest_path_outside_declared_roots"));
    }
    let kind = match &entry.kind {
      OutputEntryKind::Directory { mode } => {
        validate_mode(*mode, true)?;
        directories += 1;
        "directory"
      }
      OutputEntryKind::File {
        digest,
        mode,
        bytes: length,
      } => {
        validate_content_digest(digest)?;
        validate_mode(*mode, false)?;
        files += 1;
        bytes = bytes
          .checked_add(*length)
          .ok_or_else(|| Fault::corrupt("output_manifest_byte_overflow"))?;
        "file"
      }
      OutputEntryKind::Symlink { target } => {
        validate_symlink(&entry.path, target)?;
        symlinks += 1;
        "symlink"
      }
    };
    kinds.insert(entry.path.clone(), kind);
  }
  for entry in &manifest.entries {
    if let Some(parent) = Path::new(&entry.path).parent()
      && !parent.as_os_str().is_empty()
    {
      let parent = crate::utils::path_to_git_format(parent);
      if kinds.get(&parent) != Some(&"directory") {
        return Err(Fault::corrupt("output_manifest_parent_not_directory"));
      }
    }
  }
  if files != manifest.files
    || directories != manifest.directories
    || symlinks != manifest.symlinks
    || bytes != manifest.bytes
  {
    return Err(Fault::corrupt("output_manifest_summary_mismatch"));
  }
  if manifest.bytes > MAX_RESULT_BYTES {
    return Err(Fault::corrupt("output_manifest_byte_limit"));
  }
  Ok(())
}

fn validate_native_output_manifest(
  manifest: &OutputManifest,
  validation: &NativeCompilerValidation,
) -> Result<(), Fault> {
  let mut expected_files = validation
    .cas_output_bindings()
    .map(|(path, digest, bytes, mode)| (path, (digest, Some(bytes), mode)))
    .chain(
      validation
        .cas_stream_bindings()
        .into_iter()
        .map(|(path, digest, bytes, mode)| (path, (digest, Some(bytes), mode))),
    )
    .collect::<BTreeMap<_, _>>();
  let expected_directories = BTreeSet::from(["target", "target/outputs", "target/streams"]);
  let mut actual_directories = BTreeSet::new();
  for entry in &manifest.entries {
    match &entry.kind {
      OutputEntryKind::Directory { mode } if *mode == 0o755 => {
        actual_directories.insert(entry.path.as_str());
      }
      OutputEntryKind::File { digest, mode, bytes } => {
        let Some((expected_digest, expected_bytes, expected_mode)) = expected_files.remove(entry.path.as_str()) else {
          return Err(Fault::corrupt("native_output_manifest_file_set"));
        };
        if digest != expected_digest
          || expected_bytes.is_some_and(|expected| expected != *bytes)
          || *mode != expected_mode
        {
          return Err(Fault::corrupt("native_output_manifest_file_binding"));
        }
      }
      _ => return Err(Fault::corrupt("native_output_manifest_entry_class")),
    }
  }
  if !expected_files.is_empty() || actual_directories != expected_directories {
    return Err(Fault::corrupt("native_output_manifest_membership"));
  }
  Ok(())
}

fn validate_logical_path(path: &str) -> Result<(), Fault> {
  if path.is_empty() || path.len() > MAX_PATH_BYTES || path.contains('\0') {
    return Err(Fault::corrupt("unsafe_output_path"));
  }
  let parsed = crate::source::RepositoryPath::new(Path::new(path)).map_err(|_| Fault::corrupt("unsafe_output_path"))?;
  if parsed.as_str() != path {
    return Err(Fault::corrupt("noncanonical_output_path"));
  }
  for component in path.split('/') {
    validate_name(component)?;
  }
  Ok(())
}

fn validate_name(name: &str) -> Result<(), Fault> {
  if name.is_empty()
    || !name.is_ascii()
    || name.len() > MAX_NAME_BYTES
    || name == "."
    || name == ".."
    || name.contains(['/', '\\', '\0', ':'])
    || name.ends_with(['.', ' '])
    || name.bytes().any(|byte| byte < 0x20 || byte == 0x7f)
  {
    return Err(Fault::corrupt("unsafe_output_name"));
  }
  let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
  let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
    || stem
      .strip_prefix("COM")
      .or_else(|| stem.strip_prefix("LPT"))
      .is_some_and(|suffix| matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"));
  if reserved {
    return Err(Fault::corrupt("platform_reserved_output_name"));
  }
  Ok(())
}

fn validate_content_digest(digest: &str) -> Result<(), Fault> {
  let hex = digest
    .strip_prefix("sha256:")
    .ok_or_else(|| Fault::corrupt("file_content_digest_domain"))?;
  if hex.len() != 64
    || !hex
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
  {
    return Err(Fault::corrupt("file_content_digest_encoding"));
  }
  Ok(())
}

fn validate_mode(mode: u32, directory: bool) -> Result<(), Fault> {
  let allowed = if directory {
    mode == 0o755
  } else {
    mode == 0o755 || valid_regular_file_mode(mode)
  };
  if allowed {
    Ok(())
  } else {
    Err(Fault::incompatible("unsupported_output_mode"))
  }
}

#[cfg(unix)]
const fn valid_regular_file_mode(mode: u32) -> bool {
  mode & !0o666 == 0 && mode & 0o400 != 0
}

#[cfg(not(unix))]
const fn valid_regular_file_mode(mode: u32) -> bool {
  matches!(mode, 0o444 | 0o644)
}

fn validate_symlink(path: &str, target: &str) -> Result<(), Fault> {
  if target.is_empty() || target.contains(['\0', '\\']) || target.len() > MAX_PATH_BYTES {
    return Err(Fault::corrupt("unsafe_symlink_target"));
  }
  let path = crate::source::RepositoryPath::new(Path::new(path)).map_err(|_| Fault::corrupt("unsafe_symlink_path"))?;
  if super::symlink_target_escapes(&path, target) {
    return Err(Fault::corrupt("symlink_target_escape"));
  }
  Ok(())
}

fn prepare_tree(manifest: &OutputManifest, source_root: &Path) -> Result<PreparedTree, Fault> {
  let mut root = BuildDirectory::default();
  let mut directories = manifest
    .entries
    .iter()
    .filter_map(|entry| match entry.kind {
      OutputEntryKind::Directory { mode } => Some((entry.path.as_str(), mode)),
      _ => None,
    })
    .collect::<Vec<_>>();
  directories.sort_by_key(|(path, _)| path.split('/').count());
  for (path, mode) in directories {
    let components = path.split('/').collect::<Vec<_>>();
    insert_directory(&mut root, &components, mode)?;
  }
  for entry in &manifest.entries {
    let components = entry.path.split('/').collect::<Vec<_>>();
    match &entry.kind {
      OutputEntryKind::Directory { .. } => {}
      OutputEntryKind::File { digest, mode, bytes } => {
        insert_leaf(
          &mut root,
          &components,
          BuildNode::File {
            source: source_root.join(&entry.path),
            content_digest: digest.clone(),
            bytes: *bytes,
            mode: *mode,
          },
        )?;
      }
      OutputEntryKind::Symlink { target } => {
        let source = source_root.join(&entry.path);
        let directory = fs::metadata(&source).is_ok_and(|metadata| metadata.is_dir());
        insert_leaf(
          &mut root,
          &components,
          BuildNode::Symlink {
            target: target.clone(),
            directory,
          },
        )?;
      }
    }
  }
  let mut trees = BTreeMap::new();
  let mut blobs = BTreeMap::new();
  let root = finalize_tree(root, &mut trees, &mut blobs, 0)?;
  Ok(PreparedTree { root, trees, blobs })
}

fn insert_directory(directory: &mut BuildDirectory, components: &[&str], mode: u32) -> Result<(), Fault> {
  let (name, parents) = components
    .split_last()
    .ok_or_else(|| Fault::corrupt("empty_output_path"))?;
  let parent = directory_at_mut(directory, parents)?;
  match parent.children.get(*name) {
    Some(BuildNode::Directory { mode: existing, .. }) if *existing == mode => Ok(()),
    Some(_) => Err(Fault::corrupt("duplicate_or_colliding_output_path")),
    None => {
      parent.children.insert(
        (*name).to_string(),
        BuildNode::Directory {
          mode,
          contents: BuildDirectory::default(),
        },
      );
      Ok(())
    }
  }
}

fn insert_leaf(directory: &mut BuildDirectory, components: &[&str], node: BuildNode) -> Result<(), Fault> {
  let (name, parents) = components
    .split_last()
    .ok_or_else(|| Fault::corrupt("empty_output_path"))?;
  let parent = directory_at_mut(directory, parents)?;
  if parent.children.insert((*name).to_string(), node).is_some() {
    return Err(Fault::corrupt("duplicate_or_colliding_output_path"));
  }
  Ok(())
}

fn directory_at_mut<'a>(
  mut directory: &'a mut BuildDirectory,
  components: &[&str],
) -> Result<&'a mut BuildDirectory, Fault> {
  for component in components {
    directory = match directory.children.get_mut(*component) {
      Some(BuildNode::Directory { contents, .. }) => contents,
      _ => return Err(Fault::corrupt("missing_output_parent_directory")),
    };
  }
  Ok(directory)
}

fn finalize_tree(
  directory: BuildDirectory,
  trees: &mut BTreeMap<String, Vec<u8>>,
  blobs: &mut BTreeMap<String, PreparedBlob>,
  depth: usize,
) -> Result<String, Fault> {
  if depth > MAX_TREE_DEPTH {
    return Err(Fault::corrupt("output_tree_depth_limit"));
  }
  validate_platform_collisions(directory.children.keys().map(String::as_str))?;
  let mut entries = Vec::with_capacity(directory.children.len());
  for (name, node) in directory.children {
    let kind = match node {
      BuildNode::File {
        source,
        content_digest,
        bytes,
        mode,
      } => {
        let blob = blob_id(&content_digest, bytes)?;
        if let Some(existing) = blobs.get(&blob) {
          if existing.content_digest != content_digest || existing.bytes != bytes {
            return Err(Fault::corrupt("blob_identity_collision"));
          }
        } else {
          blobs.insert(
            blob.clone(),
            PreparedBlob {
              source,
              content_digest: content_digest.clone(),
              bytes,
            },
          );
        }
        TreeEntryKind::File {
          blob,
          content_digest,
          bytes,
          mode,
        }
      }
      BuildNode::Directory { mode, contents } => TreeEntryKind::Directory {
        tree: finalize_tree(contents, trees, blobs, depth + 1)?,
        mode,
      },
      BuildNode::Symlink { target, directory } => TreeEntryKind::Symlink { target, directory },
    };
    entries.push(TreeEntry { name, kind });
  }
  let object = TreeObject {
    version: CAS_VERSION,
    entries,
  };
  let identity = tree_id(&object).map_err(|error| Fault::corrupt(format!("tree_identity: {error}")))?;
  let bytes = canonical_json(&object).map_err(|error| Fault::corrupt(format!("tree_encoding: {error}")))?;
  if let Some(existing) = trees.insert(identity.clone(), bytes.clone())
    && existing != bytes
  {
    return Err(Fault::corrupt("tree_identity_collision"));
  }
  Ok(identity)
}

fn validate_platform_collisions<'a>(names: impl Iterator<Item = &'a str>) -> Result<(), Fault> {
  let mut portable = BTreeSet::new();
  for name in names {
    validate_name(name)?;
    let folded = name.to_ascii_lowercase();
    if !portable.insert(folded) {
      return Err(Fault::corrupt("platform_colliding_output_names"));
    }
  }
  Ok(())
}

fn estimate_result_bytes(
  prepared: &PreparedTree,
  manifest: usize,
  validation: usize,
  action_result: usize,
) -> RailResult<u64> {
  let blob_bytes = prepared.blobs.values().try_fold(0u64, |total, blob| {
    total
      .checked_add(blob.bytes)
      .ok_or_else(|| RailError::message("local CAS result size overflow"))
  })?;
  let tree_bytes = prepared.trees.values().try_fold(0u64, |total, tree| {
    total
      .checked_add(tree.len() as u64)
      .ok_or_else(|| RailError::message("local CAS result size overflow"))
  })?;
  blob_bytes
    .checked_add(tree_bytes)
    .and_then(|total| total.checked_add(manifest as u64))
    .and_then(|total| total.checked_add(validation as u64))
    .and_then(|total| total.checked_add(action_result as u64))
    .ok_or_else(|| RailError::message("local CAS result size overflow"))
}

impl LocalCas {
  fn load_verified_result(
    &self,
    action_key: &str,
    action_result: &str,
    stats: &mut ReadStats,
  ) -> Result<VerifiedResult, Fault> {
    let result_hex = validated_id_hex(action_result, ACTION_RESULT_PREFIX)
      .map_err(|_| Fault::corrupt("action_result_identity_encoding"))?;
    let bundle = self.root.join("results").join(result_hex);
    validate_bundle_directory(&bundle)?;
    validate_bundle_root_entries(&bundle)?;

    let object_path = bundle.join("action-result.json");
    let object: ActionResultObject = read_canonical_json(&object_path, MAX_OBJECT_METADATA_BYTES, stats)?;
    stats.objects = stats.objects.saturating_add(1);
    if object.version != CAS_VERSION {
      return Err(Fault::incompatible("action_result_schema_version"));
    }
    if object.action_key != action_key {
      return Err(Fault::corrupt("action_result_action_key_mismatch"));
    }
    validate_any_lookup_key(&object.lookup_key).map_err(|_| Fault::corrupt("action_result_lookup_identity"))?;
    if action_result_id(&object).map_err(|error| Fault::corrupt(format!("action_result_identity: {error}")))?
      != action_result
    {
      return Err(Fault::corrupt("action_result_digest_mismatch"));
    }
    let (output_manifest, output_tree, _) = output_result_payload(&object)?;
    validated_id_hex(output_manifest, MANIFEST_PREFIX)
      .map_err(|_| Fault::corrupt("action_result_manifest_identity"))?;
    validated_id_hex(output_tree, TREE_PREFIX).map_err(|_| Fault::corrupt("action_result_tree_identity"))?;
    let validation_hex = validated_id_hex(&object.validation, VALIDATION_PREFIX)
      .map_err(|_| Fault::corrupt("action_result_validation_identity"))?;

    let manifest_path = bundle.join("manifests").join(format!(
      "{}.json",
      validated_id_hex(output_manifest, MANIFEST_PREFIX)
        .map_err(|_| { Fault::corrupt("action_result_manifest_identity") })?
    ));
    let manifest: OutputManifest = read_canonical_json(&manifest_path, MAX_OBJECT_METADATA_BYTES, stats)?;
    stats.objects = stats.objects.saturating_add(1);
    validate_manifest(&manifest)?;
    if manifest.digest != output_manifest {
      return Err(Fault::corrupt("action_result_manifest_mismatch"));
    }
    let validation_path = bundle.join("validations").join(format!("{validation_hex}.json"));
    let validation: StoredValidation = read_canonical_json(&validation_path, MAX_OBJECT_METADATA_BYTES, stats)?;
    stats.objects = stats.objects.saturating_add(1);
    let validation_bytes =
      canonical_json(&validation).map_err(|error| Fault::corrupt(format!("validation_encoding: {error}")))?;
    if validation_id(&validation_bytes) != object.validation {
      return Err(Fault::corrupt("validation_digest_mismatch"));
    }
    if validation.action_key() != object.action_key || validation.lookup_key() != object.lookup_key {
      return Err(Fault::corrupt("validation_action_binding_mismatch"));
    }
    validation
      .validate_object()
      .map_err(|error| Fault::corrupt(format!("validation_object: {error}")))?;
    match &validation {
      StoredValidation::NativeCompiler(validation) => validate_native_output_manifest(&manifest, validation)?,
      StoredValidation::Hermetic(_) => {}
    }
    if validation.result_digest(manifest.digest()) != object.result_digest {
      return Err(Fault::corrupt("action_result_result_digest_mismatch"));
    }

    let mut trees = BTreeMap::new();
    let mut loading = BTreeSet::new();
    let mut tree_entries = 0usize;
    load_tree_recursive(
      &bundle,
      output_tree,
      0,
      &mut tree_entries,
      &mut loading,
      &mut trees,
      stats,
    )?;
    let mut flattened = BTreeMap::new();
    let mut blobs = BTreeSet::new();
    flatten_tree(output_tree, "", &trees, &mut flattened, &mut blobs, 0)?;
    let flattened = flattened
      .into_iter()
      .map(|(path, kind)| OutputEntry { path, kind })
      .collect::<Vec<_>>();
    if flattened != manifest.entries {
      return Err(Fault::corrupt("output_tree_manifest_mismatch"));
    }
    validate_object_directory(
      &bundle.join("trees"),
      &trees.keys().cloned().collect(),
      TREE_PREFIX,
      "json",
    )?;
    validate_object_directory(&bundle.join("blobs"), &blobs, BLOB_PREFIX, "blob")?;
    validate_object_directory(
      &bundle.join("manifests"),
      &BTreeSet::from([output_manifest.to_string()]),
      MANIFEST_PREFIX,
      "json",
    )?;
    validate_object_directory(
      &bundle.join("validations"),
      &BTreeSet::from([object.validation.clone()]),
      VALIDATION_PREFIX,
      "json",
    )?;
    for blob in &blobs {
      let path = bundle.join("blobs").join(format!(
        "{}.blob",
        validated_id_hex(blob, BLOB_PREFIX).map_err(|_| Fault::corrupt("blob_identity"))?
      ));
      let metadata = fs::symlink_metadata(&path).map_err(|error| Fault::corrupt(format!("blob_missing: {error}")))?;
      if !metadata.is_file()
        || is_link_or_reparse(&metadata)
        || !has_single_link(&metadata)
        || metadata.len() > MAX_RESULT_BYTES
      {
        return Err(Fault::corrupt("blob_not_bounded_regular_file"));
      }
    }
    Ok(VerifiedResult {
      #[cfg(any(unix, windows, test))]
      action_result: action_result.to_string(),
      #[cfg(any(unix, windows, test))]
      object,
      #[cfg(any(unix, windows, test))]
      manifest,
      #[cfg(any(unix, windows, test))]
      validation,
      #[cfg(any(unix, windows, test))]
      trees,
      #[cfg(any(unix, windows, test))]
      bundle,
    })
  }

  fn load_verified_compiler_evidence(
    &self,
    action_key: &str,
    action_result: &str,
    stats: &mut ReadStats,
  ) -> Result<VerifiedCompilerEvidence, Fault> {
    validate_evidence_action_key(action_key).map_err(|_| Fault::corrupt("compiler_evidence_action_identity"))?;
    let result_hex = validated_id_hex(action_result, ACTION_RESULT_PREFIX)
      .map_err(|_| Fault::corrupt("action_result_identity_encoding"))?;
    let bundle = self.root.join("results").join(result_hex);
    validate_bundle_directory(&bundle)?;
    validate_compiler_evidence_bundle_root_entries(&bundle)?;

    let object: ActionResultObject =
      read_canonical_json(&bundle.join("action-result.json"), MAX_OBJECT_METADATA_BYTES, stats)?;
    stats.objects = stats.objects.saturating_add(1);
    if object.version != CAS_VERSION {
      return Err(Fault::incompatible("action_result_schema_version"));
    }
    if object.action_key != action_key {
      return Err(Fault::corrupt("action_result_action_key_mismatch"));
    }
    if action_result_id(&object).map_err(|error| Fault::corrupt(format!("action_result_identity: {error}")))?
      != action_result
    {
      return Err(Fault::corrupt("action_result_digest_mismatch"));
    }
    validate_evidence_candidate_key(&object.lookup_key)
      .map_err(|_| Fault::corrupt("compiler_evidence_candidate_identity"))?;
    let evidence_id = compiler_evidence_payload(&object)?;
    let evidence_hex = validated_id_hex(evidence_id, EVIDENCE_OBJECT_PREFIX)
      .map_err(|_| Fault::corrupt("compiler_evidence_object_identity"))?;
    let validation_hex = validated_id_hex(&object.validation, VALIDATION_PREFIX)
      .map_err(|_| Fault::corrupt("action_result_validation_identity"))?;

    let validation: CompilerEvidenceValidation = read_canonical_json(
      &bundle.join("validations").join(format!("{validation_hex}.json")),
      MAX_OBJECT_METADATA_BYTES,
      stats,
    )?;
    stats.objects = stats.objects.saturating_add(1);
    let validation_bytes =
      canonical_json(&validation).map_err(|error| Fault::corrupt(format!("validation_encoding: {error}")))?;
    if validation_id(&validation_bytes) != object.validation
      || validation.action_key() != object.action_key
      || validation.candidate_key() != object.lookup_key
    {
      return Err(Fault::corrupt("compiler_evidence_validation_binding_mismatch"));
    }
    validation
      .validate_object()
      .map_err(|error| Fault::corrupt(format!("compiler_evidence_validation: {error}")))?;

    let evidence: CompilerEvidenceObject = read_canonical_json(
      &bundle.join("evidence").join(format!("{evidence_hex}.json")),
      MAX_OBJECT_METADATA_BYTES,
      stats,
    )?;
    stats.objects = stats.objects.saturating_add(1);
    if evidence
      .identity()
      .map_err(|error| Fault::corrupt(format!("compiler_evidence_identity: {error}")))?
      != evidence_id
    {
      return Err(Fault::corrupt("compiler_evidence_digest_mismatch"));
    }
    if validation.result_digest(evidence_id) != object.result_digest {
      return Err(Fault::corrupt("compiler_evidence_result_digest_mismatch"));
    }
    validate_object_directory(
      &bundle.join("validations"),
      &BTreeSet::from([object.validation.clone()]),
      VALIDATION_PREFIX,
      "json",
    )?;
    validate_object_directory(
      &bundle.join("evidence"),
      &BTreeSet::from([evidence_id.to_string()]),
      EVIDENCE_OBJECT_PREFIX,
      "json",
    )?;
    Ok(VerifiedCompilerEvidence { validation, evidence })
  }

  #[cfg(any(unix, windows, test))]
  fn materialize(&self, verified: &VerifiedResult, destination: &Path, stats: &mut ReadStats) -> Result<(), Fault> {
    let (_, output_tree, _) = output_result_payload(&verified.object)?;
    let parent = validate_materialization_destination(destination)?;
    let temporary = tempfile::Builder::new()
      .prefix("restore-")
      .tempdir_in(parent)
      .map_err(|error| Fault::corrupt(format!("materialization_staging_unavailable: {error}")))?;
    materialize_from_staging(verified, output_tree, destination, temporary.path(), parent, stats)
  }

  #[cfg(any(unix, windows, test))]
  fn materialize_registered(
    &self,
    verified: &VerifiedResult,
    destination: &Path,
    staging: &Path,
    stats: &mut ReadStats,
  ) -> Result<(), Fault> {
    let (_, output_tree, _) = output_result_payload(&verified.object)?;
    let parent = validate_materialization_destination(destination)?;
    if staging == destination || staging.parent() != Some(parent) {
      return Err(Fault::corrupt("materialization_staging_outside_registered_parent"));
    }
    match fs::symlink_metadata(staging) {
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
      Ok(_) => return Err(Fault::corrupt("materialization_staging_prepositioned")),
      Err(error) => return Err(Fault::corrupt(format!("materialization_staging_unreadable: {error}"))),
    }
    fs::create_dir(staging).map_err(|error| Fault::corrupt(format!("materialization_staging_create: {error}")))?;
    materialize_from_staging(verified, output_tree, destination, staging, parent, stats)?;
    fs::remove_dir(staging).map_err(|error| Fault::corrupt(format!("materialization_staging_cleanup: {error}")))
  }
}

#[cfg(any(unix, windows, test))]
fn validate_materialization_destination(destination: &Path) -> Result<&Path, Fault> {
  let parent = destination
    .parent()
    .ok_or_else(|| Fault::corrupt("materialization_root_has_no_parent"))?;
  validate_real_directory_fault(parent, "materialization parent")?;
  match fs::symlink_metadata(destination) {
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(parent),
    Ok(_) => Err(Fault::corrupt("materialization_destination_prepositioned")),
    Err(error) => Err(Fault::corrupt(format!(
      "materialization_destination_unreadable: {error}"
    ))),
  }
}

#[cfg(any(unix, windows, test))]
fn materialize_from_staging(
  verified: &VerifiedResult,
  output_tree: &str,
  destination: &Path,
  staging: &Path,
  parent: &Path,
  stats: &mut ReadStats,
) -> Result<(), Fault> {
  let payload = staging.join("output");
  fs::create_dir(&payload).map_err(|error| Fault::corrupt(format!("materialization_root_create: {error}")))?;
  // A native result is a regenerable Cargo output backed by an already
  // durable CAS bundle. Readers need atomic visibility and verified bytes,
  // not a storage barrier for the private restored copy. Hermetic action
  // restores retain their stronger durable-materialization behavior.
  let durable = matches!(&verified.validation, StoredValidation::Hermetic(_));
  materialize_tree(
    &verified.bundle,
    output_tree,
    &verified.trees,
    &payload,
    stats,
    0,
    durable,
  )?;
  if durable {
    verified
      .manifest
      .validate_unchanged(&payload)
      .map_err(|error| Fault::corrupt(format!("materialized_manifest_validation: {error}")))?;
    sync_output_tree(&payload).map_err(|error| Fault::corrupt(format!("materialized_tree_sync: {error}")))?;
  }
  fs::rename(&payload, destination)
    .map_err(|error| Fault::corrupt(format!("materialization_atomic_publish: {error}")))?;
  if durable {
    sync_directory(parent).map_err(|error| Fault::corrupt(format!("materialization_parent_sync: {error}")))?;
  }
  Ok(())
}

fn read_canonical_json<T>(path: &Path, limit: u64, stats: &mut ReadStats) -> Result<T, Fault>
where
  T: for<'de> Deserialize<'de> + Serialize,
{
  let bytes = read_bounded_file(path, limit, stats)?;
  let decoded =
    serde_json::from_slice::<T>(&bytes).map_err(|error| Fault::corrupt(format!("object_decode: {error}")))?;
  let canonical = canonical_json(&decoded).map_err(|error| Fault::corrupt(format!("object_encode: {error}")))?;
  if canonical != bytes {
    return Err(Fault::corrupt("object_encoding_not_canonical"));
  }
  Ok(decoded)
}

fn read_bounded_file(path: &Path, limit: u64, stats: &mut ReadStats) -> Result<Vec<u8>, Fault> {
  let metadata = fs::symlink_metadata(path).map_err(|error| Fault::corrupt(format!("object_missing: {error}")))?;
  if !metadata.is_file() || is_link_or_reparse(&metadata) || !has_single_link(&metadata) {
    return Err(Fault::corrupt("object_not_regular_file"));
  }
  if metadata.len() > limit {
    return Err(Fault::corrupt("object_size_limit"));
  }
  let total = stats
    .bytes
    .checked_add(metadata.len())
    .ok_or_else(|| Fault::corrupt("result_byte_overflow"))?;
  if total > MAX_LOOKUP_BYTES {
    return Err(Fault::corrupt("result_byte_limit"));
  }
  let mut file = File::open(path).map_err(|error| Fault::corrupt(format!("object_open: {error}")))?;
  let opened = file
    .metadata()
    .map_err(|error| Fault::corrupt(format!("object_metadata: {error}")))?;
  if !opened.is_file() || !has_single_link(&opened) || opened.len() != metadata.len() {
    return Err(Fault::corrupt("object_changed_before_read"));
  }
  let mut bytes = Vec::with_capacity(metadata.len() as usize);
  (&mut file)
    .take(metadata.len().saturating_add(1))
    .read_to_end(&mut bytes)
    .map_err(|error| Fault::corrupt(format!("object_read: {error}")))?;
  if bytes.len() as u64 != metadata.len() {
    return Err(Fault::corrupt("object_truncated_during_read"));
  }
  stats.bytes = total;
  crate::instrumentation::record_cas_read(bytes.len() as u64);
  Ok(bytes)
}

fn validate_bundle_directory(path: &Path) -> Result<(), Fault> {
  validate_real_directory_fault(path, "action result bundle")
}

fn validate_real_directory_fault(path: &Path, description: &str) -> Result<(), Fault> {
  let metadata =
    fs::symlink_metadata(path).map_err(|error| Fault::corrupt(format!("{description}_missing: {error}")))?;
  if metadata.is_dir() && !is_link_or_reparse(&metadata) {
    Ok(())
  } else {
    Err(Fault::corrupt(format!("{description}_not_real_directory")))
  }
}

fn validate_bundle_root_entries(bundle: &Path) -> Result<(), Fault> {
  let expected = ["action-result.json", "blobs", "manifests", "trees", "validations"]
    .into_iter()
    .map(str::to_string)
    .collect::<BTreeSet<_>>();
  let mut actual = BTreeSet::new();
  for entry in fs::read_dir(bundle).map_err(|error| Fault::corrupt(format!("bundle_read: {error}")))? {
    let entry = entry.map_err(|error| Fault::corrupt(format!("bundle_entry: {error}")))?;
    let name = entry
      .file_name()
      .into_string()
      .map_err(|_| Fault::corrupt("bundle_non_utf8_entry"))?;
    actual.insert(name);
    if actual.len() > expected.len() {
      return Err(Fault::corrupt("bundle_entries_mismatch"));
    }
  }
  if actual != expected {
    return Err(Fault::corrupt("bundle_entries_mismatch"));
  }
  for directory in ["blobs", "manifests", "trees", "validations"] {
    validate_real_directory_fault(&bundle.join(directory), "bundle object directory")?;
  }
  Ok(())
}

fn validate_compiler_evidence_bundle_root_entries(bundle: &Path) -> Result<(), Fault> {
  let expected = ["action-result.json", "evidence", "validations"]
    .into_iter()
    .map(str::to_string)
    .collect::<BTreeSet<_>>();
  let mut actual = BTreeSet::new();
  for entry in fs::read_dir(bundle).map_err(|error| Fault::corrupt(format!("bundle_read: {error}")))? {
    let entry = entry.map_err(|error| Fault::corrupt(format!("bundle_entry: {error}")))?;
    let name = entry
      .file_name()
      .into_string()
      .map_err(|_| Fault::corrupt("bundle_non_utf8_entry"))?;
    actual.insert(name);
    if actual.len() > expected.len() {
      return Err(Fault::corrupt("bundle_entries_mismatch"));
    }
  }
  if actual != expected {
    return Err(Fault::corrupt("bundle_entries_mismatch"));
  }
  for directory in ["evidence", "validations"] {
    validate_real_directory_fault(&bundle.join(directory), "bundle object directory")?;
  }
  Ok(())
}

fn load_tree_recursive(
  bundle: &Path,
  identity: &str,
  depth: usize,
  total_entries: &mut usize,
  loading: &mut BTreeSet<String>,
  loaded: &mut BTreeMap<String, TreeObject>,
  stats: &mut ReadStats,
) -> Result<(), Fault> {
  if depth > MAX_TREE_DEPTH {
    return Err(Fault::corrupt("tree_depth_limit"));
  }
  if loaded.contains_key(identity) {
    return Ok(());
  }
  if !loading.insert(identity.to_string()) {
    return Err(Fault::corrupt("tree_cycle"));
  }
  let hex = validated_id_hex(identity, TREE_PREFIX).map_err(|_| Fault::corrupt("tree_identity_encoding"))?;
  let path = bundle.join("trees").join(format!("{hex}.json"));
  let tree: TreeObject = read_canonical_json(&path, MAX_OBJECT_METADATA_BYTES, stats)?;
  stats.objects = stats.objects.saturating_add(1);
  if tree.version != CAS_VERSION {
    return Err(Fault::incompatible("tree_schema_version"));
  }
  if tree.entries.len() > MAX_ENTRIES {
    return Err(Fault::corrupt("tree_entry_limit"));
  }
  *total_entries = total_entries
    .checked_add(tree.entries.len())
    .ok_or_else(|| Fault::corrupt("tree_entry_overflow"))?;
  if *total_entries > MAX_ENTRIES {
    return Err(Fault::corrupt("tree_entry_limit"));
  }
  let actual = tree_id(&tree).map_err(|error| Fault::corrupt(format!("tree_identity: {error}")))?;
  if actual != identity {
    return Err(Fault::corrupt("tree_digest_mismatch"));
  }
  let mut previous = None::<&str>;
  validate_platform_collisions(tree.entries.iter().map(|entry| entry.name.as_str()))?;
  for entry in &tree.entries {
    if previous.is_some_and(|previous| previous >= entry.name.as_str()) {
      return Err(Fault::corrupt("tree_entries_not_strictly_sorted"));
    }
    previous = Some(&entry.name);
    match &entry.kind {
      TreeEntryKind::File {
        blob,
        content_digest,
        bytes,
        mode,
      } => {
        validate_mode(*mode, false)?;
        validate_content_digest(content_digest)?;
        if blob_id(content_digest, *bytes)? != *blob {
          return Err(Fault::corrupt("blob_reference_mismatch"));
        }
      }
      TreeEntryKind::Directory { tree, mode } => {
        validate_mode(*mode, true)?;
        load_tree_recursive(bundle, tree, depth + 1, total_entries, loading, loaded, stats)?;
      }
      TreeEntryKind::Symlink { target, .. } => {
        if target.is_empty() || target.contains(['\0', '\\']) || target.len() > MAX_PATH_BYTES {
          return Err(Fault::corrupt("unsafe_symlink_target"));
        }
      }
    }
  }
  loading.remove(identity);
  loaded.insert(identity.to_string(), tree);
  Ok(())
}

fn flatten_tree(
  identity: &str,
  prefix: &str,
  trees: &BTreeMap<String, TreeObject>,
  output: &mut BTreeMap<String, OutputEntryKind>,
  blobs: &mut BTreeSet<String>,
  depth: usize,
) -> Result<(), Fault> {
  if depth > MAX_TREE_DEPTH {
    return Err(Fault::corrupt("tree_depth_limit"));
  }
  let tree = trees
    .get(identity)
    .ok_or_else(|| Fault::corrupt("tree_reference_missing"))?;
  for entry in &tree.entries {
    let path = if prefix.is_empty() {
      entry.name.clone()
    } else {
      format!("{prefix}/{}", entry.name)
    };
    validate_logical_path(&path)?;
    let kind = match &entry.kind {
      TreeEntryKind::File {
        blob,
        content_digest,
        bytes,
        mode,
      } => {
        blobs.insert(blob.clone());
        OutputEntryKind::File {
          digest: content_digest.clone(),
          mode: *mode,
          bytes: *bytes,
        }
      }
      TreeEntryKind::Directory { tree, mode } => {
        flatten_tree(tree, &path, trees, output, blobs, depth + 1)?;
        OutputEntryKind::Directory { mode: *mode }
      }
      TreeEntryKind::Symlink { target, .. } => {
        validate_symlink(&path, target)?;
        OutputEntryKind::Symlink { target: target.clone() }
      }
    };
    if output.insert(path, kind).is_some() {
      return Err(Fault::corrupt("duplicate_tree_path"));
    }
    if output.len() > MAX_ENTRIES {
      return Err(Fault::corrupt("tree_entry_limit"));
    }
  }
  Ok(())
}

fn validate_object_directory(
  directory: &Path,
  expected: &BTreeSet<String>,
  prefix: &str,
  extension: &str,
) -> Result<(), Fault> {
  let expected = expected
    .iter()
    .map(|identity| {
      validated_id_hex(identity, prefix)
        .map(|hex| format!("{hex}.{extension}"))
        .map_err(|_| Fault::corrupt("object_identity_encoding"))
    })
    .collect::<Result<BTreeSet<_>, _>>()?;
  let mut actual = BTreeSet::new();
  for entry in fs::read_dir(directory).map_err(|error| Fault::corrupt(format!("object_directory_read: {error}")))? {
    let entry = entry.map_err(|error| Fault::corrupt(format!("object_directory_entry: {error}")))?;
    let metadata =
      fs::symlink_metadata(entry.path()).map_err(|error| Fault::corrupt(format!("object_metadata: {error}")))?;
    if !metadata.is_file() || is_link_or_reparse(&metadata) || !has_single_link(&metadata) {
      return Err(Fault::corrupt("object_directory_contains_non_file"));
    }
    actual.insert(
      entry
        .file_name()
        .into_string()
        .map_err(|_| Fault::corrupt("object_directory_non_utf8_name"))?,
    );
    if actual.len() > MAX_ENTRIES {
      return Err(Fault::corrupt("object_directory_entry_limit"));
    }
  }
  if actual != expected {
    return Err(Fault::corrupt("object_directory_membership_mismatch"));
  }
  Ok(())
}

#[cfg(any(unix, windows, test))]
fn materialize_tree(
  bundle: &Path,
  identity: &str,
  trees: &BTreeMap<String, TreeObject>,
  destination: &Path,
  stats: &mut ReadStats,
  depth: usize,
  durable: bool,
) -> Result<(), Fault> {
  if depth > MAX_TREE_DEPTH {
    return Err(Fault::corrupt("tree_depth_limit"));
  }
  let tree = trees
    .get(identity)
    .ok_or_else(|| Fault::corrupt("tree_reference_missing"))?;
  validate_real_directory_fault(destination, "materialization directory")?;
  for entry in &tree.entries {
    let path = destination.join(&entry.name);
    match &entry.kind {
      TreeEntryKind::File {
        blob,
        content_digest,
        bytes,
        mode,
      } => {
        materialize_blob(bundle, blob, content_digest, *bytes, *mode, &path, stats, durable)?;
      }
      TreeEntryKind::Directory { tree, mode } => {
        fs::create_dir(&path).map_err(|error| Fault::corrupt(format!("directory_materialization: {error}")))?;
        materialize_tree(bundle, tree, trees, &path, stats, depth + 1, durable)?;
        set_exact_mode(&path, *mode)?;
      }
      TreeEntryKind::Symlink { target, directory } => {
        let target_path = Path::new(target);
        create_materialized_symlink(target_path, &path, *directory)?;
      }
    }
  }
  Ok(())
}

#[cfg(any(unix, windows, test))]
// Each argument is an independent verified blob boundary; grouping them would
// add a construction type without removing state or knowledge.
#[allow(clippy::too_many_arguments)]
fn materialize_blob(
  bundle: &Path,
  identity: &str,
  content_digest: &str,
  expected_bytes: u64,
  mode: u32,
  destination: &Path,
  stats: &mut ReadStats,
  durable: bool,
) -> Result<(), Fault> {
  let hex = validated_id_hex(identity, BLOB_PREFIX).map_err(|_| Fault::corrupt("blob_identity_encoding"))?;
  let source = bundle.join("blobs").join(format!("{hex}.blob"));
  let metadata = fs::symlink_metadata(&source).map_err(|error| Fault::corrupt(format!("blob_missing: {error}")))?;
  if !metadata.is_file()
    || is_link_or_reparse(&metadata)
    || !has_single_link(&metadata)
    || metadata.len() != expected_bytes
  {
    return Err(Fault::corrupt("blob_metadata_mismatch"));
  }
  let maximum_read = expected_bytes.saturating_add(1);
  if stats
    .bytes
    .checked_add(maximum_read)
    .is_none_or(|total| total > MAX_LOOKUP_BYTES)
  {
    return Err(Fault::corrupt("result_byte_limit"));
  }
  let mut input = File::open(&source).map_err(|error| Fault::corrupt(format!("blob_open: {error}")))?;
  let opened = input
    .metadata()
    .map_err(|error| Fault::corrupt(format!("blob_opened_metadata: {error}")))?;
  if !opened.is_file() || !has_single_link(&opened) || opened.len() != expected_bytes {
    return Err(Fault::corrupt("blob_changed_before_read"));
  }
  let mut output = OpenOptions::new()
    .write(true)
    .create_new(true)
    .open(destination)
    .map_err(|error| Fault::corrupt(format!("blob_destination_create: {error}")))?;
  let mut digest = Sha256::new();
  let mut copied = 0u64;
  let mut buffer = [0_u8; IO_BUFFER_BYTES];
  loop {
    let remaining = maximum_read.saturating_sub(copied);
    if remaining == 0 {
      break;
    }
    let read_capacity = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
    let read = input
      .read(&mut buffer[..read_capacity])
      .map_err(|error| Fault::corrupt(format!("blob_read: {error}")))?;
    if read == 0 {
      break;
    }
    output
      .write_all(&buffer[..read])
      .map_err(|error| Fault::corrupt(format!("blob_write: {error}")))?;
    digest.update(&buffer[..read]);
    copied = copied.saturating_add(read as u64);
  }
  if durable {
    output
      .sync_all()
      .map_err(|error| Fault::corrupt(format!("blob_sync: {error}")))?;
  }
  let actual_digest = format!("sha256:{}", sha256_hex(digest));
  stats.bytes = stats.bytes.saturating_add(copied);
  crate::instrumentation::record_cas_read(copied);
  if copied != expected_bytes || actual_digest != content_digest || blob_id(&actual_digest, copied)? != identity {
    return Err(Fault::corrupt("blob_digest_mismatch"));
  }
  stats.objects = stats.objects.saturating_add(1);
  stats.restored = stats.restored.saturating_add(copied);
  crate::instrumentation::record_cas_restore(copied);
  crate::instrumentation::record_hash(copied as usize);
  crate::instrumentation::record_hashed_file_bytes_read(copied as usize);
  set_exact_mode(destination, mode)
}

#[cfg(unix)]
fn set_exact_mode(path: &Path, mode: u32) -> Result<(), Fault> {
  use std::os::unix::fs::PermissionsExt as _;

  fs::set_permissions(path, fs::Permissions::from_mode(mode))
    .map_err(|error| Fault::corrupt(format!("output_mode: {error}")))
}

#[cfg(not(unix))]
fn set_exact_mode(path: &Path, mode: u32) -> Result<(), Fault> {
  let mut permissions = fs::metadata(path)
    .map_err(|error| Fault::corrupt(format!("output_mode_metadata: {error}")))?
    .permissions();
  permissions.set_readonly(mode & 0o200 == 0);
  fs::set_permissions(path, permissions).map_err(|error| Fault::corrupt(format!("output_mode: {error}")))
}

#[cfg(unix)]
fn create_materialized_symlink(target: &Path, destination: &Path, _directory: bool) -> Result<(), Fault> {
  std::os::unix::fs::symlink(target, destination)
    .map_err(|error| Fault::corrupt(format!("symlink_materialization: {error}")))
}

#[cfg(windows)]
fn create_materialized_symlink(target: &Path, destination: &Path, directory: bool) -> Result<(), Fault> {
  let result = if directory {
    std::os::windows::fs::symlink_dir(target, destination)
  } else {
    std::os::windows::fs::symlink_file(target, destination)
  };
  result.map_err(|error| Fault::corrupt(format!("symlink_materialization: {error}")))
}

#[cfg(not(any(unix, windows)))]
fn create_materialized_symlink(_target: &Path, _destination: &Path, _directory: bool) -> Result<(), Fault> {
  Err(Fault::incompatible("symlink_materialization_unsupported"))
}

#[cfg(any(unix, windows, test))]
fn sync_output_tree(root: &Path) -> RailResult<()> {
  let mut directories = Vec::new();
  let mut pending = vec![root.to_path_buf()];
  while let Some(path) = pending.pop() {
    let metadata = fs::symlink_metadata(&path)?;
    if is_link_or_reparse(&metadata) {
      continue;
    }
    if metadata.is_dir() {
      directories.push(path.clone());
      for entry in fs::read_dir(path)? {
        pending.push(entry?.path());
      }
    }
  }
  directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
  for directory in directories {
    sync_directory(&directory)?;
  }
  Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> RailResult<()> {
  File::open(path)?.sync_all()?;
  Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> RailResult<()> {
  Ok(())
}

impl LocalCas {
  fn stage_bundle(&self, publication: BundlePublication<'_>) -> RailResult<StagedBundle> {
    let BundlePublication {
      object,
      object_bytes,
      manifest,
      manifest_bytes,
      validation,
      validation_bytes,
      prepared,
      move_preverified_blobs,
    } = publication;
    let (temporary, active) = self.create_guarded_staging("result-")?;
    let payload = temporary.path().join("payload");
    fs::create_dir(&payload)?;
    make_directory_private(&payload)?;
    for name in ["blobs", "manifests", "trees", "validations"] {
      let directory = payload.join(name);
      fs::create_dir(&directory)?;
      make_directory_private(&directory)?;
    }
    let mut stats = StoreStats::default();
    write_new_before_commit(&payload.join("action-result.json"), object_bytes)?;
    stats.objects_written = stats.objects_written.saturating_add(1);
    stats.bytes_written = stats.bytes_written.saturating_add(object_bytes.len() as u64);
    #[cfg(test)]
    pause_test_publication_after_first_object(&payload)?;

    let manifest_hex = validated_id_hex(manifest.digest(), MANIFEST_PREFIX)?;
    write_new_before_commit(
      &payload.join("manifests").join(format!("{manifest_hex}.json")),
      manifest_bytes,
    )?;
    stats.objects_written = stats.objects_written.saturating_add(1);
    stats.bytes_written = stats.bytes_written.saturating_add(manifest_bytes.len() as u64);

    let validation_identity = validation_id(validation_bytes);
    if validation_identity != object.validation || canonical_json(&validation)? != validation_bytes {
      return Err(RailError::message(
        "local CAS validation object changed before publication",
      ));
    }
    let validation_hex = validated_id_hex(&validation_identity, VALIDATION_PREFIX)?;
    write_new_before_commit(
      &payload.join("validations").join(format!("{validation_hex}.json")),
      validation_bytes,
    )?;
    stats.objects_written = stats.objects_written.saturating_add(1);
    stats.bytes_written = stats.bytes_written.saturating_add(validation_bytes.len() as u64);

    for (identity, bytes) in &prepared.trees {
      let hex = validated_id_hex(identity, TREE_PREFIX)?;
      write_new_before_commit(&payload.join("trees").join(format!("{hex}.json")), bytes)?;
      stats.objects_written = stats.objects_written.saturating_add(1);
      stats.bytes_written = stats.bytes_written.saturating_add(bytes.len() as u64);
    }
    for (identity, blob) in &prepared.blobs {
      let hex = validated_id_hex(identity, BLOB_PREFIX)?;
      let destination = payload.join("blobs").join(format!("{hex}.blob"));
      let written = if move_preverified_blobs {
        move_blob_verified(blob, identity, &destination)?
      } else {
        copy_blob_verified(blob, identity, &destination)?
      };
      stats.objects_written = stats.objects_written.saturating_add(1);
      stats.bytes_written = stats.bytes_written.saturating_add(written);
    }
    sync_directory_before_commit(&payload.join("blobs"))?;
    sync_directory_before_commit(&payload.join("manifests"))?;
    sync_directory_before_commit(&payload.join("trees"))?;
    sync_directory_before_commit(&payload.join("validations"))?;
    sync_directory_before_commit(&payload)?;
    Ok(StagedBundle {
      _temporary: temporary,
      _active: active,
      payload,
      stats,
    })
  }

  fn publish_staged_bundle(
    &self,
    action_result: &str,
    object: &ActionResultObject,
    staged: StagedBundle,
    commit_durability: bool,
  ) -> RailResult<StoreStats> {
    let result_hex = validated_id_hex(action_result, ACTION_RESULT_PREFIX)?;
    let destination = self.root.join("results").join(result_hex);
    match fs::symlink_metadata(&destination) {
      Ok(metadata) if metadata.is_dir() && !is_link_or_reparse(&metadata) => {
        let mut read = ReadStats::default();
        self
          .load_verified_result(&object.action_key, action_result, &mut read)
          .map_err(fault_to_error)?;
        return Ok(StoreStats::default());
      }
      Ok(_) => {
        return Err(RailError::with_help(
          format!("local CAS result '{}' is not a real directory", destination.display()),
          "run `cargo rail cache clean --scope local`; cargo-rail will not replace a hostile object",
        ));
      }
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
      Err(error) => return Err(error.into()),
    }
    let StagedBundle {
      _temporary,
      _active,
      payload,
      stats,
    } = staged;
    match rename_committed(&payload, &destination, false) {
      Ok(()) => {
        if commit_durability {
          sync_directory_before_commit(&self.root.join("results"))?;
        }
        crate::instrumentation::record_cas_write(stats.bytes_written, stats.objects_written);
        Ok(stats)
      }
      Err(_)
        if fs::symlink_metadata(&destination)
          .is_ok_and(|metadata| metadata.is_dir() && !is_link_or_reparse(&metadata)) =>
      {
        let mut read = ReadStats::default();
        self
          .load_verified_result(&object.action_key, action_result, &mut read)
          .map_err(fault_to_error)?;
        Ok(StoreStats::default())
      }
      Err(error) => Err(RailError::message(format!(
        "failed to atomically publish local CAS result '{}': {error}",
        destination.display()
      ))),
    }
  }

  fn publish_compiler_evidence_bundle(&self, publication: CompilerEvidencePublication<'_>) -> RailResult<StoreStats> {
    let CompilerEvidencePublication {
      action_result,
      object,
      object_bytes,
      validation,
      validation_bytes,
      evidence,
      evidence_bytes,
    } = publication;
    let result_hex = validated_id_hex(action_result, ACTION_RESULT_PREFIX)?;
    let destination = self.root.join("results").join(result_hex);
    match fs::symlink_metadata(&destination) {
      Ok(metadata) if metadata.is_dir() && !is_link_or_reparse(&metadata) => {
        let mut read = ReadStats::default();
        self
          .load_verified_compiler_evidence(&object.action_key, action_result, &mut read)
          .map_err(fault_to_error)?;
        return Ok(StoreStats::default());
      }
      Ok(_) => {
        return Err(RailError::with_help(
          format!(
            "local CAS compiler-evidence result '{}' is not a real directory",
            destination.display()
          ),
          "run `cargo rail cache clean --scope local`; cargo-rail will not replace a hostile object",
        ));
      }
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
      Err(error) => return Err(error.into()),
    }

    if validation.action_key() != object.action_key
      || validation.candidate_key() != object.lookup_key
      || validation_id(validation_bytes) != object.validation
      || evidence.identity()? != object.compiler_evidence.as_deref().unwrap_or_default()
    {
      return Err(RailError::message(
        "compiler evidence changed before local CAS publication",
      ));
    }
    let temporary = tempfile::Builder::new()
      .prefix("compiler-evidence-")
      .tempdir_in(self.root.join("staging"))?;
    let payload = temporary.path().join("payload");
    fs::create_dir(&payload)?;
    make_directory_private(&payload)?;
    for name in ["evidence", "validations"] {
      let directory = payload.join(name);
      fs::create_dir(&directory)?;
      make_directory_private(&directory)?;
    }

    let mut stats = StoreStats::default();
    write_new_synced(&payload.join("action-result.json"), object_bytes)?;
    stats.objects_written = 1;
    stats.bytes_written = object_bytes.len() as u64;
    #[cfg(test)]
    pause_test_publication_after_first_object(&payload)?;

    let validation_hex = validated_id_hex(&object.validation, VALIDATION_PREFIX)?;
    write_new_synced(
      &payload.join("validations").join(format!("{validation_hex}.json")),
      validation_bytes,
    )?;
    stats.objects_written = stats.objects_written.saturating_add(1);
    stats.bytes_written = stats.bytes_written.saturating_add(validation_bytes.len() as u64);

    let evidence_identity = evidence.identity()?;
    let evidence_hex = validated_id_hex(&evidence_identity, EVIDENCE_OBJECT_PREFIX)?;
    if canonical_json(evidence)? != evidence_bytes {
      return Err(RailError::message(
        "compiler evidence encoding changed before local CAS publication",
      ));
    }
    write_new_synced(
      &payload.join("evidence").join(format!("{evidence_hex}.json")),
      evidence_bytes,
    )?;
    stats.objects_written = stats.objects_written.saturating_add(1);
    stats.bytes_written = stats.bytes_written.saturating_add(evidence_bytes.len() as u64);
    sync_directory(&payload.join("evidence"))?;
    sync_directory(&payload.join("validations"))?;
    sync_directory(&payload)?;

    match rename_committed(&payload, &destination, false) {
      Ok(()) => {
        sync_directory(&self.root.join("results"))?;
        crate::instrumentation::record_cas_write(stats.bytes_written, stats.objects_written);
        Ok(stats)
      }
      Err(_)
        if fs::symlink_metadata(&destination)
          .is_ok_and(|metadata| metadata.is_dir() && !is_link_or_reparse(&metadata)) =>
      {
        let mut read = ReadStats::default();
        self
          .load_verified_compiler_evidence(&object.action_key, action_result, &mut read)
          .map_err(fault_to_error)?;
        Ok(StoreStats::default())
      }
      Err(error) => Err(RailError::message(format!(
        "failed to atomically publish local CAS compiler evidence '{}': {error}",
        destination.display()
      ))),
    }
  }

  fn publish_compiler_evidence_candidate_index(&self, action_key: &str, candidate_key: &str) -> RailResult<()> {
    validate_evidence_action_key(action_key)?;
    let candidate_hex = validated_id_hex(candidate_key, EVIDENCE_CANDIDATE_KEY_PREFIX)?;
    let action_hex = validated_id_hex(action_key, EVIDENCE_ACTION_KEY_PREFIX)?;
    let index_root = self.root.join(EVIDENCE_CANDIDATE_INDEX_DIRECTORY);
    let directory = create_real_directory(&index_root, candidate_hex)?;
    let indexed = EvidenceCandidateIndexEntry {
      version: EVIDENCE_CANDIDATE_INDEX_VERSION,
      action_key: action_key.to_string(),
      candidate_key: candidate_key.to_string(),
    };
    let bytes = canonical_json(&indexed)?;
    let destination = directory.join(format!("{action_hex}.json"));
    let mut temporary = tempfile::NamedTempFile::new_in(self.root.join("staging"))?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    match persist_noclobber_committed(temporary, &destination) {
      Ok(_) => {
        sync_directory(&directory)?;
        sync_directory(&index_root)
      }
      Err(_)
        if fs::symlink_metadata(&destination)
          .is_ok_and(|metadata| metadata.is_file() && !is_link_or_reparse(&metadata) && has_single_link(&metadata)) =>
      {
        let mut stats = ReadStats::default();
        let existing: EvidenceCandidateIndexEntry =
          read_canonical_json(&destination, MAX_OBJECT_METADATA_BYTES, &mut stats).map_err(fault_to_error)?;
        if existing != indexed {
          return Err(RailError::message(
            "existing local CAS compiler-evidence candidate has a different action binding",
          ));
        }
        Ok(())
      }
      Err(error) => Err(RailError::message(format!(
        "failed to publish local CAS compiler-evidence candidate '{}': {}",
        destination.display(),
        error.error
      ))),
    }
  }

  fn remove_compiler_evidence_candidate_index(&self, action_key: &str, candidate_key: &str) -> RailResult<u64> {
    validate_evidence_action_key(action_key)?;
    let candidate_hex = validated_id_hex(candidate_key, EVIDENCE_CANDIDATE_KEY_PREFIX)?;
    let action_hex = validated_id_hex(action_key, EVIDENCE_ACTION_KEY_PREFIX)?;
    let directory = self.root.join(EVIDENCE_CANDIDATE_INDEX_DIRECTORY).join(candidate_hex);
    let path = directory.join(format!("{action_hex}.json"));
    let metadata = match fs::symlink_metadata(&path) {
      Ok(metadata) => metadata,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
      Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || is_link_or_reparse(&metadata) || !has_single_link(&metadata) {
      return Err(RailError::message(format!(
        "local CAS compiler-evidence candidate entry '{}' is not a regular file",
        path.display()
      )));
    }
    let mut stats = ReadStats::default();
    let indexed: EvidenceCandidateIndexEntry =
      read_canonical_json(&path, MAX_OBJECT_METADATA_BYTES, &mut stats).map_err(fault_to_error)?;
    if indexed.version != EVIDENCE_CANDIDATE_INDEX_VERSION
      || indexed.action_key != action_key
      || indexed.candidate_key != candidate_key
    {
      return Err(RailError::message(
        "local CAS compiler-evidence candidate entry does not match its authoritative pin",
      ));
    }
    fs::remove_file(&path)?;
    sync_directory(&directory)?;
    Ok(metadata.len())
  }

  fn publish_pin(&self, action_key: &str, lookup_key: &str, action_result: &str) -> RailResult<()> {
    let pin = ActionPin {
      version: CAS_VERSION,
      action_key: action_key.to_string(),
      action_result: action_result.to_string(),
      lookup_key: lookup_key.to_string(),
      created_unix_nanos: unix_nanos(),
    };
    let bytes = canonical_json(&pin)?;
    let key_hex = validated_action_key_hex(action_key)?;
    let destination = self.root.join("pins").join(format!("{key_hex}.json"));
    let mut temporary = tempfile::NamedTempFile::new_in(self.root.join("staging"))?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    match persist_noclobber_committed(temporary, &destination) {
      Ok(_) => {
        sync_directory(&self.root.join("pins"))?;
        Ok(())
      }
      Err(_)
        if fs::symlink_metadata(&destination)
          .is_ok_and(|metadata| metadata.is_file() && !is_link_or_reparse(&metadata)) =>
      {
        let mut stats = ReadStats::default();
        let existing: ActionPin =
          read_canonical_json(&destination, MAX_OBJECT_METADATA_BYTES, &mut stats).map_err(fault_to_error)?;
        if existing.version != CAS_VERSION {
          return Err(RailError::message(
            "existing local CAS action pin has an incompatible schema",
          ));
        }
        if existing.action_key != action_key
          || existing.action_result != action_result
          || existing.lookup_key != lookup_key
        {
          return Err(RailError::with_help(
            format!("action key '{action_key}' produced two different verified results"),
            "the action is nondeterministic or the cache is corrupt; run `cargo rail cache clean --scope local` before retrying",
          ));
        }
        Ok(())
      }
      Err(error) => Err(RailError::message(format!(
        "failed to atomically publish local CAS pin '{}': {}",
        destination.display(),
        error.error
      ))),
    }
  }

  fn publish_native_action_state(
    &self,
    action_key: &str,
    result_key: &str,
    action_result: &str,
    admitted_origins: NativeResultOrigins,
  ) -> RailResult<()> {
    crate::compiler::native_cache::validate_action_key(action_key)?;
    crate::compiler::native_cache::validate_result_key(result_key)?;
    validated_id_hex(action_result, ACTION_RESULT_PREFIX)?;
    let action_hex = validated_action_key_hex(action_key)?;
    let destination = self
      .root
      .join(NATIVE_ACTION_STATE_DIRECTORY)
      .join(format!("{action_hex}.json"));
    let state = match fs::symlink_metadata(&destination) {
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => NativeActionState {
        version: NATIVE_ACTION_STATE_VERSION,
        action_key: action_key.to_string(),
        state: NativeActionStateKind::UniqueResult {
          result_key: result_key.to_string(),
          action_result: action_result.to_string(),
          origins: admitted_origins,
        },
      },
      Err(error) => return Err(error.into()),
      Ok(metadata) => {
        if !metadata.is_file()
          || is_link_or_reparse(&metadata)
          || !has_single_link(&metadata)
          || metadata.len() > MAX_OBJECT_METADATA_BYTES
        {
          let quarantined = quarantined_native_action_state(action_key, NativeStateFault::Unreadable, &[]);
          self.publish_terminal_native_state(&destination, &quarantined)?;
          return Err(RailError::message(
            "local CAS native action state was quarantined because it was unreadable",
          ));
        }
        let bytes = fs::read(&destination)?;
        let existing = match decode_native_action_state(&bytes, action_key) {
          Ok(existing) => existing,
          Err(fault) => {
            let quarantined = quarantined_native_action_state(action_key, fault, &bytes);
            self.publish_terminal_native_state(&destination, &quarantined)?;
            return Err(RailError::message(
              "local CAS native action state was quarantined because it was malformed",
            ));
          }
        };
        match existing.state {
          NativeActionStateKind::UniqueResult {
            result_key: existing_result,
            origins,
            ..
          } if existing_result == result_key => NativeActionState {
            version: NATIVE_ACTION_STATE_VERSION,
            action_key: action_key.to_string(),
            state: NativeActionStateKind::UniqueResult {
              result_key: result_key.to_string(),
              action_result: action_result.to_string(),
              origins: NativeResultOrigins {
                local: origins.local || admitted_origins.local,
                remote: admitted_origins.remote.or(origins.remote),
              },
            },
          },
          NativeActionStateKind::UniqueResult {
            result_key: existing_result,
            ..
          } => {
            let (first_result_key, second_result_key) = if existing_result.as_str() < result_key {
              (existing_result, result_key.to_string())
            } else {
              (result_key.to_string(), existing_result)
            };
            let conflicted = NativeActionState {
              version: NATIVE_ACTION_STATE_VERSION,
              action_key: action_key.to_string(),
              state: NativeActionStateKind::ConflictedResults {
                first_result_key,
                second_result_key,
              },
            };
            self.publish_terminal_native_state(&destination, &conflicted)?;
            return Err(RailError::with_help(
              format!("native action '{action_key}' produced two different verified results"),
              "the action is nondeterministic; this cache authority root will never restore that action",
            ));
          }
          NativeActionStateKind::ConflictedResults { .. } => {
            return Err(RailError::message("local CAS native action is durably conflicted"));
          }
          NativeActionStateKind::Quarantined { .. } => {
            return Err(RailError::message("local CAS native action is durably quarantined"));
          }
        }
      }
    };
    validate_native_action_state(&state, action_key)?;
    write_file_atomic_committed(&destination, &canonical_json(&state)?)
  }

  fn publish_terminal_native_state(&self, destination: &Path, state: &NativeActionState) -> RailResult<()> {
    if !matches!(
      state.state,
      NativeActionStateKind::ConflictedResults { .. } | NativeActionStateKind::Quarantined { .. }
    ) {
      return Err(RailError::message(
        "native terminal-state publication received a unique state",
      ));
    }
    let bytes = canonical_json(state)?;
    let mut ledger = validate_native_ledger(&self.root)?;
    ledger.terminal_states = ledger
      .terminal_states
      .checked_add(1)
      .ok_or_else(|| RailError::message("native terminal-state count overflow"))?;
    ledger.terminal_bytes = ledger
      .terminal_bytes
      .checked_add(bytes.len() as u64)
      .ok_or_else(|| RailError::message("native terminal-state byte count overflow"))?;
    if ledger.terminal_states > MAX_NATIVE_TERMINAL_STATES || ledger.terminal_bytes > MAX_NATIVE_TERMINAL_BYTES {
      ledger.disabled = true;
      write_native_ledger(&self.root, &ledger)?;
      return Err(RailError::with_help(
        "native cache authority was disabled because its terminal-state ledger is full",
        "run `cargo rail cache clean --scope local` to explicitly reset the complete authority root",
      ));
    }
    // Reserve evidence before replacing action authority. A crash may
    // conservatively overcharge, but cannot expose an uncharged terminal state.
    write_native_ledger(&self.root, &ledger)?;
    write_file_atomic_committed(destination, &bytes)
  }

  fn create_lease(&self, action_result: &str) -> RailResult<LeaseGuard> {
    validated_id_hex(action_result, ACTION_RESULT_PREFIX)?;
    let record = LeaseRecord {
      version: CAS_VERSION,
      action_result: action_result.to_string(),
      created_unix_seconds: unix_seconds(),
    };
    let bytes = canonical_json(&record)?;
    let mut temporary = tempfile::NamedTempFile::new_in(self.root.join("staging"))?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    let random = temporary
      .path()
      .file_name()
      .and_then(OsStr::to_str)
      .ok_or_else(|| RailError::message("temporary lease name is not valid UTF-8"))?;
    let destination = self.root.join("leases").join(format!("{random}.json"));
    persist_noclobber_committed(temporary, &destination).map_err(|error| {
      RailError::message(format!(
        "failed to atomically publish local CAS lease '{}': {}",
        destination.display(),
        error.error
      ))
    })?;
    sync_directory(&self.root.join("leases"))?;
    Ok(LeaseGuard { path: destination })
  }
}

fn write_new_synced(path: &Path, bytes: &[u8]) -> RailResult<()> {
  let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
  file.write_all(bytes)?;
  file.sync_all()?;
  Ok(())
}

/// Write private result bytes without issuing an independent device-cache
/// drain for every CAS object.
///
/// On Apple platforms `File::sync_all` maps to `F_FULLFSYNC`, which drains the
/// whole device queue. A plain `fsync` still establishes the write-before-rename
/// ordering this regenerable CAS needs without imposing that device-wide drain.
fn write_new_before_commit(path: &Path, bytes: &[u8]) -> RailResult<()> {
  let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
  file.write_all(bytes)?;
  sync_before_commit(&file)
}

struct CommittedPersistError {
  error: std::io::Error,
}

#[cfg(windows)]
fn persist_noclobber_committed(
  temporary: tempfile::NamedTempFile,
  destination: &Path,
) -> Result<File, CommittedPersistError> {
  let (file, temporary_path) = temporary
    .keep()
    .map_err(|error| CommittedPersistError { error: error.error })?;
  if let Err(error) = cargo_rail_windows_fs::rename_write_through(&temporary_path, destination, false) {
    drop(file);
    let error = match fs::remove_file(&temporary_path) {
      Ok(()) => error,
      Err(cleanup) => std::io::Error::new(
        error.kind(),
        format!(
          "{error}; failed to remove retained temporary file '{}': {cleanup}",
          temporary_path.display()
        ),
      ),
    };
    return Err(CommittedPersistError { error });
  }
  Ok(file)
}

#[cfg(not(windows))]
fn persist_noclobber_committed(
  temporary: tempfile::NamedTempFile,
  destination: &Path,
) -> Result<File, CommittedPersistError> {
  temporary
    .persist_noclobber(destination)
    .map_err(|error| CommittedPersistError { error: error.error })
}

#[cfg(windows)]
fn rename_committed(source: &Path, destination: &Path, replace: bool) -> std::io::Result<()> {
  cargo_rail_windows_fs::rename_write_through(source, destination, replace)
}

#[cfg(not(windows))]
fn rename_committed(source: &Path, destination: &Path, _replace: bool) -> std::io::Result<()> {
  fs::rename(source, destination)
}

#[cfg(target_os = "macos")]
fn sync_before_commit(file: &File) -> RailResult<()> {
  rustix::fs::fsync(file).map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
  Ok(())
}

#[cfg(not(target_os = "macos"))]
fn sync_before_commit(file: &File) -> RailResult<()> {
  file.sync_all()?;
  Ok(())
}

#[cfg(unix)]
fn sync_directory_before_commit(path: &Path) -> RailResult<()> {
  sync_before_commit(&File::open(path)?)
}

#[cfg(not(unix))]
fn sync_directory_before_commit(_path: &Path) -> RailResult<()> {
  Ok(())
}

#[cfg(test)]
fn pause_test_publication_after_first_object(_payload: &Path) -> RailResult<()> {
  const FAIL_ENV: &str = "CARGO_RAIL_TEST_CAS_FAIL_AFTER_FIRST_OBJECT";
  const PAUSE_ENV: &str = "CARGO_RAIL_TEST_CAS_PAUSE_AFTER_FIRST_OBJECT";
  if std::env::var_os(FAIL_ENV).is_some() {
    #[cfg(windows)]
    const OUT_OF_SPACE: i32 = 112;
    #[cfg(not(windows))]
    const OUT_OF_SPACE: i32 = 28;
    return Err(std::io::Error::from_raw_os_error(OUT_OF_SPACE).into());
  }
  let Some(control) = std::env::var_os(PAUSE_ENV) else {
    return Ok(());
  };
  let control = PathBuf::from(control);
  fs::create_dir_all(&control)?;
  write_new_synced(&control.join("ready"), b"ready\n")?;
  let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
  while !control.join("continue").is_file() {
    if std::time::Instant::now() >= deadline {
      return Err(RailError::message("test publication pause timed out"));
    }
    std::thread::sleep(std::time::Duration::from_millis(10));
  }
  Ok(())
}

#[cfg(test)]
fn pause_test_staging_before_active(_directory: &Path) -> RailResult<()> {
  const PAUSE_ENV: &str = "CARGO_RAIL_TEST_CAS_PAUSE_BEFORE_ACTIVE";
  let Some(control) = std::env::var_os(PAUSE_ENV) else {
    return Ok(());
  };
  let control = PathBuf::from(control);
  fs::create_dir_all(&control)?;
  write_new_synced(&control.join("ready"), b"ready\n")?;
  let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
  while !control.join("continue").is_file() {
    if std::time::Instant::now() >= deadline {
      return Err(RailError::message("test staging-creation pause timed out"));
    }
    std::thread::sleep(std::time::Duration::from_millis(10));
  }
  Ok(())
}

fn copy_blob_verified(blob: &PreparedBlob, identity: &str, destination: &Path) -> RailResult<u64> {
  let metadata = fs::symlink_metadata(&blob.source)?;
  if !metadata.is_file() || is_link_or_reparse(&metadata) || !has_single_link(&metadata) || metadata.len() != blob.bytes
  {
    return Err(RailError::message(format!(
      "declared output '{}' changed before local CAS publication",
      blob.source.display()
    )));
  }
  let mut input = File::open(&blob.source)?;
  let opened = input.metadata()?;
  if !opened.is_file() || !has_single_link(&opened) || opened.len() != blob.bytes {
    return Err(RailError::message(format!(
      "declared output '{}' changed before local CAS publication",
      blob.source.display()
    )));
  }
  let mut output = OpenOptions::new().write(true).create_new(true).open(destination)?;
  let mut hasher = Sha256::new();
  let mut copied = 0u64;
  let maximum_read = blob.bytes.saturating_add(1);
  let mut buffer = [0_u8; IO_BUFFER_BYTES];
  loop {
    let remaining = maximum_read.saturating_sub(copied);
    if remaining == 0 {
      break;
    }
    let read_capacity = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
    let read = input.read(&mut buffer[..read_capacity])?;
    if read == 0 {
      break;
    }
    output.write_all(&buffer[..read])?;
    hasher.update(&buffer[..read]);
    copied = copied.saturating_add(read as u64);
  }
  sync_before_commit(&output)?;
  let digest = format!("sha256:{}", sha256_hex(hasher));
  if copied != blob.bytes
    || digest != blob.content_digest
    || blob_id(&digest, copied).map_err(fault_to_error)? != identity
  {
    return Err(RailError::message(format!(
      "declared output '{}' changed while the local CAS copied it",
      blob.source.display()
    )));
  }
  crate::instrumentation::record_hash(copied as usize);
  crate::instrumentation::record_hashed_file_bytes_read(copied as usize);
  Ok(copied)
}

fn move_blob_verified(blob: &PreparedBlob, identity: &str, destination: &Path) -> RailResult<u64> {
  let metadata = fs::symlink_metadata(&blob.source)?;
  if !metadata.is_file() || is_link_or_reparse(&metadata) || !has_single_link(&metadata) || metadata.len() != blob.bytes
  {
    return Err(RailError::message(format!(
      "verified staged output '{}' changed before local CAS publication",
      blob.source.display()
    )));
  }
  let mut input = File::open(&blob.source)?;
  if !crate::utils::private_file_matches_path(&input, &blob.source, blob.bytes)? {
    return Err(RailError::message(format!(
      "verified staged output '{}' changed while it was opened",
      blob.source.display()
    )));
  }
  let mut hasher = Sha256::new();
  let mut copied = 0u64;
  let mut buffer = [0_u8; IO_BUFFER_BYTES];
  loop {
    let read = input.read(&mut buffer)?;
    if read == 0 {
      break;
    }
    hasher.update(&buffer[..read]);
    copied = copied.saturating_add(read as u64);
    if copied > blob.bytes {
      break;
    }
  }
  let digest = format!("sha256:{}", sha256_hex(hasher));
  if copied != blob.bytes
    || digest != blob.content_digest
    || blob_id(&digest, copied).map_err(fault_to_error)? != identity
    || !crate::utils::private_file_matches_path(&input, &blob.source, blob.bytes)?
  {
    return Err(RailError::message(format!(
      "verified staged output '{}' changed before zero-copy admission",
      blob.source.display()
    )));
  }
  // Command-scoped wrapper staging is intentionally not durable: a crash may
  // lose a candidate, but must never expose authority over unsynced bytes. The
  // admission worker owns the one required durability boundary immediately
  // before moving the verified payload into the published result bundle.
  sync_before_commit(&input)?;
  drop(input);
  fs::rename(&blob.source, destination)?;
  crate::instrumentation::record_hash(copied as usize);
  crate::instrumentation::record_hashed_file_bytes_read(copied as usize);
  Ok(copied)
}

fn unix_seconds() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or(0, |duration| duration.as_secs())
}

fn sha256_hex(hasher: Sha256) -> String {
  crate::source::ContentDigest::from_sha256_bytes(hasher.finalize().into()).to_string()
}

fn unix_nanos() -> u128 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or(0, |duration| duration.as_nanos())
}

enum GcAuthorityKind {
  Pin { lookup_key: String },
  NativeAction,
}

struct GcAuthority {
  path: PathBuf,
  key: String,
  result: String,
  last_used: u128,
  kind: GcAuthorityKind,
}

impl LocalCas {
  fn reserve_result_capacity(&self, incoming: u64, protected_result: Option<&str>) -> RailResult<()> {
    let mut current = validate_capacity_state(&self.root)?.result_bytes;
    if current.saturating_add(incoming) > self.max_bytes {
      let target = self.max_bytes.saturating_sub(incoming);
      self.garbage_collect(target, protected_result)?;
      current = validate_capacity_state(&self.root)?.result_bytes;
    }
    let reserved = current
      .checked_add(incoming)
      .ok_or_else(|| RailError::message("local CAS size overflow"))?;
    if reserved > self.max_bytes {
      return Err(RailError::with_help(
        format!(
          "local CAS needs {incoming} bytes but its {}-byte bound cannot be satisfied",
          self.max_bytes
        ),
        format!("raise {CACHE_MAX_BYTES_ENV} or run `cargo rail cache clean --scope local`"),
      ));
    }
    write_capacity_state(&self.root, reserved)?;
    Ok(())
  }

  fn settle_result_capacity(&self, reserved: u64, written: u64) -> RailResult<()> {
    if written > reserved {
      return Err(RailError::message(
        "local CAS publication wrote more bytes than it reserved",
      ));
    }
    let state = validate_capacity_state(&self.root)?;
    let result_bytes = state
      .result_bytes
      .checked_sub(reserved)
      .and_then(|bytes| bytes.checked_add(written))
      .ok_or_else(|| RailError::message("local CAS capacity settlement underflow"))?;
    write_capacity_state(&self.root, result_bytes)
  }

  fn garbage_collect(&self, target_bytes: u64, protected_result: Option<&str>) -> RailResult<()> {
    let now = unix_seconds();
    let mut leased = BTreeSet::new();
    let leases = self.root.join("leases");
    let mut lease_paths = fs::read_dir(&leases)?.collect::<Result<Vec<_>, _>>()?;
    lease_paths.sort_by_key(|entry| entry.file_name());
    for entry in lease_paths {
      let path = entry.path();
      let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
        Err(error) => return Err(error.into()),
      };
      if !metadata.is_file() || is_link_or_reparse(&metadata) || !has_single_link(&metadata) {
        return Err(RailError::message(format!(
          "local CAS lease '{}' is not a regular file",
          path.display()
        )));
      }
      let mut stats = ReadStats::default();
      let lease: LeaseRecord =
        read_canonical_json(&path, MAX_OBJECT_METADATA_BYTES, &mut stats).map_err(fault_to_error)?;
      if lease.version != CAS_VERSION {
        return Err(RailError::message("local CAS lease has an incompatible schema"));
      }
      validated_id_hex(&lease.action_result, ACTION_RESULT_PREFIX)?;
      if now.saturating_sub(lease.created_unix_seconds) >= STALE_LEASE_SECONDS {
        fs::remove_file(path)?;
      } else {
        leased.insert(lease.action_result);
      }
    }
    if let Some(protected) = protected_result {
      leased.insert(protected.to_string());
    }

    let pins_directory = self.root.join("pins");
    let mut authorities = Vec::new();
    let mut pin_entries = fs::read_dir(&pins_directory)?.collect::<Result<Vec<_>, _>>()?;
    pin_entries.sort_by_key(|entry| entry.file_name());
    for entry in pin_entries {
      let path = entry.path();
      let metadata = fs::symlink_metadata(&path)?;
      if !metadata.is_file() || is_link_or_reparse(&metadata) || !has_single_link(&metadata) {
        return Err(RailError::message(format!(
          "local CAS pin '{}' is not a regular file",
          path.display()
        )));
      }
      let file_name = entry
        .file_name()
        .into_string()
        .map_err(|_| RailError::message("local CAS pin has a non-UTF-8 name"))?;
      let key_hex = file_name
        .strip_suffix(".json")
        .ok_or_else(|| RailError::message(format!("local CAS pin '{file_name}' has an invalid name")))?;
      let mut stats = ReadStats::default();
      let pin: ActionPin = read_canonical_json(&path, MAX_OBJECT_METADATA_BYTES, &mut stats).map_err(fault_to_error)?;
      if pin.version != CAS_VERSION {
        return Err(RailError::message("local CAS pin has an incompatible schema"));
      }
      if validated_action_key_hex(&pin.action_key)? != key_hex {
        return Err(RailError::message(
          "local CAS pin filename does not match its action key",
        ));
      }
      validated_id_hex(&pin.action_result, ACTION_RESULT_PREFIX)?;
      validate_any_lookup_key(&pin.lookup_key)?;
      authorities.push(GcAuthority {
        path,
        key: pin.action_key,
        result: pin.action_result,
        last_used: last_used_unix_nanos(&metadata, pin.created_unix_nanos),
        kind: GcAuthorityKind::Pin {
          lookup_key: pin.lookup_key,
        },
      });
    }

    let native_actions_directory = self.root.join(NATIVE_ACTION_STATE_DIRECTORY);
    let mut native_entries = fs::read_dir(&native_actions_directory)?.collect::<Result<Vec<_>, _>>()?;
    native_entries.sort_by_key(|entry| entry.file_name());
    for entry in native_entries {
      let path = entry.path();
      let metadata = fs::symlink_metadata(&path)?;
      if !metadata.is_file() || is_link_or_reparse(&metadata) || !has_single_link(&metadata) {
        return Err(RailError::message(format!(
          "local CAS native action state '{}' is not a regular file",
          path.display()
        )));
      }
      let file_name = entry
        .file_name()
        .into_string()
        .map_err(|_| RailError::message("local CAS native action state has a non-UTF-8 name"))?;
      let key_hex = file_name.strip_suffix(".json").ok_or_else(|| {
        RailError::message(format!(
          "local CAS native action state '{file_name}' has an invalid name"
        ))
      })?;
      let action_key = format!("{}{key_hex}", crate::compiler::native_cache::ACTION_KEY_PREFIX);
      crate::compiler::native_cache::validate_action_key(&action_key)?;
      let state = if metadata.len() <= MAX_OBJECT_METADATA_BYTES {
        let bytes = fs::read(&path)?;
        match decode_native_action_state(&bytes, &action_key) {
          Ok(state) => state,
          Err(fault) => {
            let state = quarantined_native_action_state(&action_key, fault, &bytes);
            self.publish_terminal_native_state(&path, &state)?;
            state
          }
        }
      } else {
        let state = quarantined_native_action_state(&action_key, NativeStateFault::Unreadable, &[]);
        self.publish_terminal_native_state(&path, &state)?;
        state
      };
      if let NativeActionStateKind::UniqueResult { action_result, .. } = state.state {
        authorities.push(GcAuthority {
          path,
          key: action_key,
          result: action_result,
          last_used: last_used_unix_nanos(&metadata, 0),
          kind: GcAuthorityKind::NativeAction,
        });
      }
    }
    sync_directory(&native_actions_directory)?;
    authorities.sort_by(|left, right| (left.last_used, &left.key).cmp(&(right.last_used, &right.key)));

    let results_directory = self.root.join("results");
    let mut result_sizes = BTreeMap::<String, u64>::new();
    let mut result_entries = fs::read_dir(&results_directory)?.collect::<Result<Vec<_>, _>>()?;
    result_entries.sort_by_key(|entry| entry.file_name());
    for entry in result_entries {
      let name = entry
        .file_name()
        .into_string()
        .map_err(|_| RailError::message("local CAS result has a non-UTF-8 name"))?;
      if name.len() != 64
        || !name
          .bytes()
          .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
      {
        return Err(RailError::message(format!(
          "local CAS result directory '{name}' is not canonical"
        )));
      }
      validate_real_directory(&entry.path(), "local CAS result")?;
      result_sizes.insert(
        format!("{ACTION_RESULT_PREFIX}{name}"),
        checked_tree_bytes(&entry.path())?,
      );
    }

    let mut references = BTreeMap::<String, usize>::new();
    for authority in &authorities {
      *references.entry(authority.result.clone()).or_default() += 1;
    }
    for (result, size) in result_sizes.clone() {
      if !references.contains_key(&result) && !leased.contains(&result) {
        let path = result_path(&self.root, &result)?;
        safe_remove_tree(&path)?;
        result_sizes.remove(&result);
        let _ = size;
      }
    }

    let mut current = result_sizes.values().try_fold(0u64, |total, size| {
      total
        .checked_add(*size)
        .ok_or_else(|| RailError::message("local CAS result size overflow"))
    })?;
    for authority in authorities {
      if current <= target_bytes {
        break;
      }
      if leased.contains(&authority.result) {
        continue;
      }
      fs::remove_file(&authority.path)?;
      match &authority.kind {
        GcAuthorityKind::Pin { lookup_key } if lookup_key.starts_with(EVIDENCE_CANDIDATE_KEY_PREFIX) => {
          current = current.saturating_sub(self.remove_compiler_evidence_candidate_index(&authority.key, lookup_key)?);
          sync_directory(&pins_directory)?;
        }
        GcAuthorityKind::Pin { .. } => sync_directory(&pins_directory)?,
        GcAuthorityKind::NativeAction => sync_directory(&native_actions_directory)?,
      }
      if let Some(count) = references.get_mut(&authority.result) {
        *count = count.saturating_sub(1);
        if *count == 0 {
          references.remove(&authority.result);
          if let Some(size) = result_sizes.remove(&authority.result) {
            safe_remove_tree(&result_path(&self.root, &authority.result)?)?;
            current = current.saturating_sub(size);
          }
        }
      }
    }
    sync_directory(&pins_directory)?;
    sync_directory(&native_actions_directory)?;
    sync_directory(&results_directory)?;
    write_capacity_state(&self.root, current)?;
    Ok(())
  }
}

fn result_path(root: &Path, identity: &str) -> RailResult<PathBuf> {
  Ok(
    root
      .join("results")
      .join(validated_id_hex(identity, ACTION_RESULT_PREFIX)?),
  )
}

fn checked_tree_bytes(root: &Path) -> RailResult<u64> {
  checked_tree_file_stats(root).map(|(_, bytes)| bytes)
}

fn reconcile_capacity_state(root: &Path) -> RailResult<()> {
  write_capacity_state(root, checked_tree_bytes(&root.join("results"))?)
}

fn validate_capacity_state(root: &Path) -> RailResult<CapacityState> {
  let path = root.join(CAPACITY_STATE_FILE);
  let mut stats = ReadStats::default();
  let state: CapacityState =
    read_canonical_json(&path, MAX_OBJECT_METADATA_BYTES, &mut stats).map_err(fault_to_error)?;
  if state.version != CAS_VERSION {
    return Err(RailError::message(
      "local CAS capacity state has an incompatible schema",
    ));
  }
  Ok(state)
}

fn write_capacity_state(root: &Path, result_bytes: u64) -> RailResult<()> {
  write_file_atomic_committed(
    &root.join(CAPACITY_STATE_FILE),
    &canonical_json(&CapacityState {
      version: CAS_VERSION,
      result_bytes,
    })?,
  )
}

#[cfg(target_os = "macos")]
fn write_file_atomic_committed(path: &Path, contents: &[u8]) -> RailResult<()> {
  let parent = path
    .parent()
    .filter(|parent| !parent.as_os_str().is_empty())
    .unwrap_or(Path::new("."));
  let mut temporary = tempfile::Builder::new()
    .prefix(".cargo-rail-")
    .suffix(".tmp")
    .tempfile_in(parent)
    .map_err(|error| {
      RailError::message(format!(
        "failed to create temporary local CAS file in {}: {error}",
        parent.display()
      ))
    })?;
  if let Ok(metadata) = fs::symlink_metadata(path)
    && metadata.file_type().is_file()
  {
    temporary.as_file().set_permissions(metadata.permissions())?;
  }
  temporary.write_all(contents)?;
  sync_before_commit(temporary.as_file())?;
  temporary.persist(path).map_err(|error| {
    RailError::message(format!(
      "failed to atomically replace local CAS file {}: {}",
      path.display(),
      error.error
    ))
  })?;
  sync_directory_before_commit(parent)
}

#[cfg(windows)]
fn write_file_atomic_committed(path: &Path, contents: &[u8]) -> RailResult<()> {
  let parent = path
    .parent()
    .filter(|parent| !parent.as_os_str().is_empty())
    .unwrap_or(Path::new("."));
  let mut temporary = tempfile::Builder::new()
    .prefix(".cargo-rail-")
    .suffix(".tmp")
    .tempfile_in(parent)
    .map_err(|error| {
      RailError::message(format!(
        "failed to create temporary local CAS file in {}: {error}",
        parent.display()
      ))
    })?;
  if let Ok(metadata) = fs::symlink_metadata(path)
    && metadata.file_type().is_file()
  {
    temporary.as_file().set_permissions(metadata.permissions())?;
  }
  temporary.write_all(contents)?;
  sync_before_commit(temporary.as_file())?;
  let (file, temporary_path) = temporary.keep().map_err(|error| {
    RailError::message(format!(
      "failed to retain temporary local CAS file for {}: {}",
      path.display(),
      error.error
    ))
  })?;
  if let Err(error) = cargo_rail_windows_fs::rename_write_through(&temporary_path, path, true) {
    drop(file);
    let cleanup = fs::remove_file(&temporary_path);
    return Err(RailError::message(match cleanup {
      Ok(()) => format!(
        "failed to atomically replace local CAS file {}: {error}",
        path.display()
      ),
      Err(cleanup) => format!(
        "failed to atomically replace local CAS file {}: {error}; failed to remove retained temporary file '{}': {cleanup}",
        path.display(),
        temporary_path.display()
      ),
    }));
  }
  drop(file);
  Ok(())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn write_file_atomic_committed(path: &Path, contents: &[u8]) -> RailResult<()> {
  crate::utils::write_file_atomic(path, contents)
}

fn reconcile_native_ledger(root: &Path) -> RailResult<()> {
  let mut terminal_states = 0_u64;
  let mut terminal_bytes = 0_u64;
  for entry in bounded_directory_entries(
    &root.join(NATIVE_ACTION_STATE_DIRECTORY),
    "local CAS native action-state ledger",
  )? {
    let path = entry.path();
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_file() || is_link_or_reparse(&metadata) || !has_single_link(&metadata) {
      return Err(RailError::message(format!(
        "local CAS native action state '{}' is not a private regular file",
        path.display()
      )));
    }
    let file_name = entry
      .file_name()
      .into_string()
      .map_err(|_| RailError::message("local CAS native action state has a non-UTF-8 name"))?;
    let key = file_name
      .strip_suffix(".json")
      .ok_or_else(|| RailError::message("local CAS native action state has a noncanonical name"))?;
    let action_key = format!("{}{key}", crate::compiler::native_cache::ACTION_KEY_PREFIX);
    crate::compiler::native_cache::validate_action_key(&action_key)?;
    let terminal = if metadata.len() > MAX_OBJECT_METADATA_BYTES {
      true
    } else {
      let bytes = fs::read(&path)?;
      decode_native_action_state(&bytes, &action_key).map_or(true, |state| {
        matches!(
          state.state,
          NativeActionStateKind::ConflictedResults { .. } | NativeActionStateKind::Quarantined { .. }
        )
      })
    };
    if terminal {
      terminal_states = terminal_states.saturating_add(1);
      terminal_bytes = terminal_bytes.saturating_add(metadata.len());
    }
  }
  write_native_ledger(
    root,
    &NativeLedgerState {
      version: NATIVE_ACTION_STATE_VERSION,
      terminal_states,
      terminal_bytes,
      disabled: terminal_states > MAX_NATIVE_TERMINAL_STATES || terminal_bytes > MAX_NATIVE_TERMINAL_BYTES,
    },
  )
}

fn validate_native_ledger(root: &Path) -> RailResult<NativeLedgerState> {
  let path = root.join(NATIVE_LEDGER_STATE_FILE);
  let mut stats = ReadStats::default();
  let state: NativeLedgerState =
    read_canonical_json(&path, MAX_OBJECT_METADATA_BYTES, &mut stats).map_err(fault_to_error)?;
  if state.version == 1 && !root.join(NATIVE_ACTION_STATE_DIRECTORY).exists() {
    return Ok(NativeLedgerState {
      version: NATIVE_ACTION_STATE_VERSION,
      terminal_states: 0,
      terminal_bytes: 0,
      disabled: false,
    });
  }
  if state.version != NATIVE_ACTION_STATE_VERSION
    || (!state.disabled
      && (state.terminal_states > MAX_NATIVE_TERMINAL_STATES || state.terminal_bytes > MAX_NATIVE_TERMINAL_BYTES))
  {
    return Err(RailError::message(
      "local CAS native terminal-state ledger is incompatible",
    ));
  }
  Ok(state)
}

fn write_native_ledger(root: &Path, state: &NativeLedgerState) -> RailResult<()> {
  write_file_atomic_committed(&root.join(NATIVE_LEDGER_STATE_FILE), &canonical_json(state)?)
}

fn checked_tree_file_stats(root: &Path) -> RailResult<(u64, u64)> {
  let mut files = 0u64;
  let mut bytes = 0u64;
  let mut entries = 0usize;
  let mut pending = vec![root.to_path_buf()];
  while let Some(path) = pending.pop() {
    entries = entries.saturating_add(1);
    if entries > MAX_ENTRIES {
      return Err(RailError::message(format!(
        "local CAS tree '{}' exceeds its {MAX_ENTRIES}-entry scan bound",
        root.display()
      )));
    }
    let metadata = match fs::symlink_metadata(&path) {
      Ok(metadata) => metadata,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
      Err(error) => return Err(error.into()),
    };
    if is_link_or_reparse(&metadata) {
      return Err(RailError::with_help(
        format!("local CAS contains a symlink at '{}'", path.display()),
        "run `cargo rail cache clean --scope local`; cargo-rail will not follow cache symlinks",
      ));
    }
    if metadata.is_dir() {
      let entries = match fs::read_dir(&path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
        Err(error) => return Err(error.into()),
      };
      let mut children = Vec::new();
      for entry in entries {
        match entry {
          Ok(entry) => children.push(entry),
          Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
          Err(error) => return Err(error.into()),
        }
      }
      children.sort_by_key(|entry| entry.file_name());
      pending.extend(children.into_iter().map(|entry| entry.path()));
    } else if metadata.is_file() {
      files = files.saturating_add(1);
      bytes = bytes
        .checked_add(metadata.len())
        .ok_or_else(|| RailError::message("local CAS size overflow"))?;
    } else {
      return Err(RailError::message(format!(
        "local CAS contains unsupported file type '{}'",
        path.display()
      )));
    }
  }
  Ok((files, bytes))
}

fn lifecycle_lock_path(root: &Path) -> RailResult<PathBuf> {
  let parent = root
    .parent()
    .filter(|parent| parent.file_name() == Some(OsStr::new("cargo-rail")))
    .ok_or_else(|| RailError::message("local CAS root has no canonical cargo-rail owner directory"))?;
  let root_name = root
    .file_name()
    .and_then(OsStr::to_str)
    .ok_or_else(|| RailError::message("local CAS root name is not valid UTF-8"))?;
  Ok(parent.join(format!("{root_name}.lock")))
}

fn lock_local_cas(path: &Path, create: bool, mode: LockMode) -> RailResult<Option<LocalCasLifecycleLock>> {
  let parent = path
    .parent()
    .ok_or_else(|| RailError::message("local CAS lifecycle lock has no parent"))?;
  validate_real_directory(parent, "local CAS owner")?;
  let file = match crate::utils::open_cache_lock_file(path, create) {
    Ok(file) => file,
    Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error.into()),
  };
  if !crate::utils::private_file_matches_path(&file, path, 0)? {
    return Err(RailError::with_help(
      format!(
        "local CAS lifecycle lock '{}' is not a private regular file",
        path.display()
      ),
      "remove the hostile lock path; cargo-rail will not follow or share cache lock files",
    ));
  }
  match mode {
    LockMode::Shared => file.lock_shared()?,
    LockMode::Exclusive => file.lock()?,
  }
  if !crate::utils::private_file_matches_path(&file, path, 0)? {
    return Err(RailError::message(format!(
      "local CAS lifecycle lock '{}' changed while it was acquired",
      path.display()
    )));
  }
  Ok(Some(LocalCasLifecycleLock { _file: file }))
}

fn bounded_directory_entries(directory: &Path, description: &str) -> RailResult<Vec<fs::DirEntry>> {
  let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
  if entries.len() > MAX_ENTRIES {
    return Err(RailError::message(format!(
      "{description} exceeds its {MAX_ENTRIES}-entry scan bound"
    )));
  }
  entries.sort_by_key(|entry| entry.file_name());
  Ok(entries)
}

fn bounded_optional_directory_entries(directory: &Path, description: &str) -> RailResult<Vec<fs::DirEntry>> {
  match fs::symlink_metadata(directory) {
    Ok(_) => bounded_directory_entries(directory, description),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
    Err(error) => Err(error.into()),
  }
}

fn optional_tree_file_stats(root: &Path) -> RailResult<(u64, u64)> {
  match fs::symlink_metadata(root) {
    Ok(_) => checked_tree_file_stats(root),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((0, 0)),
    Err(error) => Err(error.into()),
  }
}

fn safe_remove_tree(root: &Path) -> RailResult<()> {
  let metadata = fs::symlink_metadata(root)?;
  if !metadata.is_dir() || is_link_or_reparse(&metadata) {
    return Err(RailError::message(format!(
      "refusing to recursively remove non-directory local CAS path '{}'",
      root.display()
    )));
  }
  // `remove_dir_all` uses the platform's non-following recursive removal. Keep
  // ownership validation here and leave race-resistant traversal to `std`.
  fs::remove_dir_all(root).map_err(|error| {
    RailError::message(format!(
      "failed to remove local CAS directory '{}': {error}",
      root.display()
    ))
  })
}

fn clear_staging(staging: &Path) -> RailResult<()> {
  for entry in bounded_directory_entries(staging, "local CAS staging")? {
    let path = entry.path();
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.is_dir() && !is_link_or_reparse(&metadata) {
      if staging_entry_is_active(&path)? {
        continue;
      }
      safe_remove_tree(&path)?;
    } else if metadata.is_dir() || is_directory_reparse(&metadata) {
      fs::remove_dir(&path)?;
    } else {
      fs::remove_file(&path)?;
    }
  }
  // Staging contains no cache authority. Persist cleanup without forcing an
  // Apple device-wide drain; a crash can only leave regenerable residue.
  sync_directory_before_commit(staging)
}

fn staging_entry_is_active(path: &Path) -> RailResult<bool> {
  let active = path.join("ACTIVE");
  let metadata = match fs::symlink_metadata(&active) {
    Ok(metadata) => metadata,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
    Err(error) => return Err(error.into()),
  };
  if !metadata.is_file() || is_link_or_reparse(&metadata) || !has_single_link(&metadata) || metadata.len() != 0 {
    return Err(RailError::message(format!(
      "local CAS staging lease '{}' is not a private empty file",
      active.display()
    )));
  }
  let file = OpenOptions::new().read(true).write(true).open(&active)?;
  match file.try_lock() {
    Ok(()) => Ok(false),
    Err(fs::TryLockError::WouldBlock) => Ok(true),
    Err(fs::TryLockError::Error(error)) => Err(error.into()),
  }
}

pub(super) fn existing_root_at(root: &Path) -> RailResult<Option<PathBuf>> {
  let valid_name = root.file_name().and_then(OsStr::to_str).is_some_and(|name| {
    name == CAS_ROOT_NAME
      || name
        .strip_prefix(&format!("{CAS_ROOT_NAME}-"))
        .is_some_and(|trust_domain| validate_trust_domain(trust_domain).is_ok())
  });
  if !root.is_absolute() || !valid_name || root.parent().and_then(Path::file_name) != Some(OsStr::new("cargo-rail")) {
    return Err(RailError::message(format!(
      "local CAS reference '{}' is not a cargo-rail-owned cache path",
      root.display()
    )));
  }
  match fs::symlink_metadata(root) {
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error.into()),
    Ok(_) => validate_real_directory(root, "local CAS root")?,
  }
  let canonical = fs::canonicalize(root)?;
  if canonical != root {
    return Err(RailError::message(format!(
      "local CAS reference '{}' is not canonical",
      root.display()
    )));
  }
  ensure_owner_marker_existing(root, None)?;
  validate_root_entries(root)?;
  for name in [
    "results",
    "pins",
    "leases",
    "staging",
    NATIVE_ACTION_STATE_DIRECTORY,
    NATIVE_ENVIRONMENT_SELECTOR_DIRECTORY,
    EVIDENCE_CANDIDATE_INDEX_DIRECTORY,
  ] {
    validate_optional_real_directory(&root.join(name), "local CAS domain")?;
  }
  validate_optional_real_directory(
    &root.join(LEGACY_NATIVE_ACTION_STATE_DIRECTORY),
    "legacy local CAS native action state",
  )?;
  #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
  validate_optional_real_directory(&root.join(SYSROOT_IDENTITY_MEMO_DIRECTORY), "local CAS domain")?;
  validate_capacity_state(root)?;
  validate_native_ledger(root)?;
  Ok(Some(root.to_path_buf()))
}

pub(super) fn status_at(root: &Path) -> RailResult<Option<LocalCasStatus>> {
  status_at_with_max(root, cache_max_bytes()?)
}

fn status_at_with_max(root: &Path, max_bytes: u64) -> RailResult<Option<LocalCasStatus>> {
  let lifecycle_lock = lifecycle_lock_path(root)?;
  let lock = lock_local_cas(&lifecycle_lock, false, LockMode::Exclusive)?;
  if lock.is_none() && existing_root_at(root)?.is_some() {
    return Err(RailError::with_help(
      "local CAS predates coordinated lifecycle snapshots",
      "run an ordinary cargo-rail cacheable action once to establish the lifecycle lock",
    ));
  }
  let _lock = lock;
  let Some(root) = existing_root_at(root)? else {
    return Ok(None);
  };
  let now = unix_seconds();
  let mut referenced_results = BTreeSet::new();
  let mut oldest_used = None::<u128>;
  let mut newest_used = None::<u128>;

  let pin_entries = bounded_optional_directory_entries(&root.join("pins"), "local CAS pins")?;
  for entry in &pin_entries {
    let path = entry.path();
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_file() || is_link_or_reparse(&metadata) || !has_single_link(&metadata) {
      return Err(RailError::message(format!(
        "local CAS pin '{}' is not a bounded regular file",
        path.display()
      )));
    }
    let mut stats = ReadStats::default();
    let pin: ActionPin = read_canonical_json(&path, MAX_OBJECT_METADATA_BYTES, &mut stats).map_err(fault_to_error)?;
    if pin.version != CAS_VERSION {
      return Err(RailError::message("local CAS pin has an incompatible schema"));
    }
    validated_action_key_hex(&pin.action_key)?;
    validated_id_hex(&pin.action_result, ACTION_RESULT_PREFIX)?;
    validate_any_lookup_key(&pin.lookup_key)?;
    referenced_results.insert(pin.action_result);
    let last_used = last_used_unix_nanos(&metadata, pin.created_unix_nanos);
    oldest_used = Some(oldest_used.map_or(last_used, |current| current.min(last_used)));
    newest_used = Some(newest_used.map_or(last_used, |current| current.max(last_used)));
  }

  let native_entries =
    bounded_optional_directory_entries(&root.join(NATIVE_ACTION_STATE_DIRECTORY), "local CAS native actions")?;
  let mut native_unique = 0_u64;
  let mut native_conflicted = 0_u64;
  let mut native_quarantined = 0_u64;
  let mut native_local_origins = 0_u64;
  let mut native_remote_origins = 0_u64;
  for entry in &native_entries {
    let path = entry.path();
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_file() || is_link_or_reparse(&metadata) || !has_single_link(&metadata) {
      return Err(RailError::message(format!(
        "local CAS native action state '{}' is not a bounded regular file",
        path.display()
      )));
    }
    let name = entry
      .file_name()
      .into_string()
      .map_err(|_| RailError::message("local CAS native action state has a non-UTF-8 name"))?;
    let key = name
      .strip_suffix(".json")
      .ok_or_else(|| RailError::message("local CAS native action state has a noncanonical name"))?;
    let action_key = format!("{}{key}", crate::compiler::native_cache::ACTION_KEY_PREFIX);
    let bytes = fs::read(&path)?;
    let state = decode_native_action_state(&bytes, &action_key)
      .map_err(|_| RailError::message("local CAS native action state is malformed"))?;
    match state.state {
      NativeActionStateKind::UniqueResult {
        action_result, origins, ..
      } => {
        native_unique = native_unique.saturating_add(1);
        native_local_origins = native_local_origins.saturating_add(u64::from(origins.local));
        native_remote_origins = native_remote_origins.saturating_add(u64::from(origins.remote.is_some()));
        referenced_results.insert(action_result);
        let last_used = last_used_unix_nanos(&metadata, 0);
        oldest_used = Some(oldest_used.map_or(last_used, |current| current.min(last_used)));
        newest_used = Some(newest_used.map_or(last_used, |current| current.max(last_used)));
      }
      NativeActionStateKind::ConflictedResults { .. } => {
        native_conflicted = native_conflicted.saturating_add(1);
      }
      NativeActionStateKind::Quarantined { .. } => {
        native_quarantined = native_quarantined.saturating_add(1);
      }
    }
  }

  let mut active_leases = 0u64;
  let mut stale_leases = 0u64;
  let mut reclaimable_bytes = 0u64;
  for entry in bounded_optional_directory_entries(&root.join("leases"), "local CAS leases")? {
    let path = entry.path();
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_file() || is_link_or_reparse(&metadata) || !has_single_link(&metadata) {
      return Err(RailError::message(format!(
        "local CAS lease '{}' is not a bounded regular file",
        path.display()
      )));
    }
    let mut stats = ReadStats::default();
    let lease: LeaseRecord =
      read_canonical_json(&path, MAX_OBJECT_METADATA_BYTES, &mut stats).map_err(fault_to_error)?;
    if lease.version != CAS_VERSION {
      return Err(RailError::message("local CAS lease has an incompatible schema"));
    }
    validated_id_hex(&lease.action_result, ACTION_RESULT_PREFIX)?;
    if now.saturating_sub(lease.created_unix_seconds) >= STALE_LEASE_SECONDS {
      stale_leases = stale_leases.saturating_add(1);
      reclaimable_bytes = reclaimable_bytes.saturating_add(metadata.len());
    } else {
      active_leases = active_leases.saturating_add(1);
      referenced_results.insert(lease.action_result);
    }
  }

  let mut results = 0u64;
  let mut objects = 0u64;
  for entry in bounded_optional_directory_entries(&root.join("results"), "local CAS results")? {
    let name = entry
      .file_name()
      .into_string()
      .map_err(|_| RailError::message("local CAS result has a non-UTF-8 name"))?;
    validated_id_hex(&format!("{ACTION_RESULT_PREFIX}{name}"), ACTION_RESULT_PREFIX)?;
    validate_real_directory(&entry.path(), "local CAS result")?;
    let (files, bytes) = checked_tree_file_stats(&entry.path())?;
    results = results.saturating_add(1);
    objects = objects.saturating_add(files);
    if !referenced_results.contains(&format!("{ACTION_RESULT_PREFIX}{name}")) {
      reclaimable_bytes = reclaimable_bytes.saturating_add(bytes);
    }
  }

  let staging = &root.join("staging");
  let staging_entries = bounded_optional_directory_entries(staging, "local CAS staging")?.len() as u64;
  let (_, staging_bytes) = optional_tree_file_stats(staging)?;
  let index_files = optional_tree_file_stats(&root.join(EVIDENCE_CANDIDATE_INDEX_DIRECTORY))?
    .0
    .checked_add(optional_tree_file_stats(&root.join(NATIVE_ENVIRONMENT_SELECTOR_DIRECTORY))?.0)
    .ok_or_else(|| RailError::message("local CAS index file count overflow"))?;
  let capacity = validate_capacity_state(&root)?;
  let ledger = validate_native_ledger(&root)?;

  Ok(Some(LocalCasStatus {
    root: root.to_string_lossy().into_owned(),
    trust_domain: owner_trust_domain(&root)?,
    bytes: checked_tree_bytes(&root)?,
    max_bytes,
    committed_result_bytes: capacity.result_bytes,
    results,
    pins: pin_entries.len() as u64,
    native_actions: native_entries.len() as u64,
    native_unique,
    native_conflicted,
    native_quarantined,
    native_local_origins,
    native_remote_origins,
    native_ledger_bytes: ledger.terminal_bytes,
    native_ledger_max_bytes: MAX_NATIVE_TERMINAL_BYTES,
    native_ledger_disabled: ledger.disabled,
    objects,
    active_leases,
    stale_leases,
    staging_entries,
    staging_bytes,
    index_files,
    reclaimable_bytes,
    oldest_used_unix_ms: oldest_used.map(|value| u64::try_from(value / 1_000_000).unwrap_or(u64::MAX)),
    newest_used_unix_ms: newest_used.map(|value| u64::try_from(value / 1_000_000).unwrap_or(u64::MAX)),
  }))
}

fn last_used_unix_nanos(metadata: &fs::Metadata, created_unix_nanos: u128) -> u128 {
  metadata
    .modified()
    .ok()
    .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
    .map_or(created_unix_nanos, |duration| duration.as_nanos())
    .max(created_unix_nanos)
}

pub(super) fn remove_owned_root_at(root: &Path) -> RailResult<Option<(PathBuf, u64)>> {
  let lifecycle_lock = lifecycle_lock_path(root)?;
  let _lock = lock_local_cas(&lifecycle_lock, true, LockMode::Exclusive)?
    .ok_or_else(|| RailError::message("local CAS lifecycle lock was not created"))?;
  let Some(root) = existing_root_at(root)? else {
    return Ok(None);
  };
  let bytes = removable_tree_bytes(&root)?;
  safe_remove_tree(&root)?;
  Ok(Some((root, bytes)))
}

pub(super) fn legacy_status_at(root: &Path) -> RailResult<Option<(PathBuf, u64)>> {
  let Some(root) = existing_legacy_root_at(root)? else {
    return Ok(None);
  };
  let lifecycle_lock = root
    .parent()
    .ok_or_else(|| RailError::message("legacy local CAS has no owner directory"))?
    .join(format!("{LEGACY_CAS_ROOT_NAME}.lock"));
  let _lock = lock_local_cas(&lifecycle_lock, false, LockMode::Exclusive)?;
  let Some(root) = existing_legacy_root_at(&root)? else {
    return Ok(None);
  };
  let bytes = removable_tree_bytes(&root)?;
  Ok(Some((root, bytes)))
}

pub(super) fn remove_legacy_owned_root_at(root: &Path) -> RailResult<Option<(PathBuf, u64)>> {
  let lifecycle_lock = root
    .parent()
    .filter(|parent| parent.file_name() == Some(OsStr::new("cargo-rail")))
    .ok_or_else(|| RailError::message("legacy local CAS has no canonical owner directory"))?
    .join(format!("{LEGACY_CAS_ROOT_NAME}.lock"));
  let _lock = lock_local_cas(&lifecycle_lock, true, LockMode::Exclusive)?
    .ok_or_else(|| RailError::message("legacy local CAS lifecycle lock was not created"))?;
  let Some(root) = existing_legacy_root_at(root)? else {
    return Ok(None);
  };
  let bytes = removable_tree_bytes(&root)?;
  safe_remove_tree(&root)?;
  Ok(Some((root, bytes)))
}

fn existing_legacy_root_at(root: &Path) -> RailResult<Option<PathBuf>> {
  if !root.is_absolute()
    || root.file_name() != Some(OsStr::new(LEGACY_CAS_ROOT_NAME))
    || root.parent().and_then(Path::file_name) != Some(OsStr::new("cargo-rail"))
  {
    return Err(RailError::message(format!(
      "legacy local CAS reference '{}' is not a cargo-rail-owned cache path",
      root.display()
    )));
  }
  match fs::symlink_metadata(root) {
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error.into()),
    Ok(_) => validate_real_directory(root, "legacy local CAS root")?,
  }
  if fs::canonicalize(root)? != root {
    return Err(RailError::message("legacy local CAS reference is not canonical"));
  }
  let marker = root.join("OWNER");
  let metadata = fs::symlink_metadata(&marker)?;
  if !metadata.is_file()
    || is_link_or_reparse(&metadata)
    || !has_single_link(&metadata)
    || metadata.len() != LEGACY_OWNER_MARKER.len() as u64
    || fs::read(&marker)? != LEGACY_OWNER_MARKER
  {
    return Err(RailError::with_help(
      format!(
        "legacy local CAS root '{}' has an invalid ownership marker",
        root.display()
      ),
      "cargo-rail will not reclaim an unowned legacy path",
    ));
  }
  Ok(Some(root.to_path_buf()))
}

/// Measure an owned tree without following links so cleanup can still recover
/// a cache containing a hostile nested link.
fn removable_tree_bytes(root: &Path) -> RailResult<u64> {
  let mut bytes = 0u64;
  let mut visited = 0usize;
  let mut pending = vec![root.to_path_buf()];
  while let Some(path) = pending.pop() {
    visited = visited.saturating_add(1);
    if visited > MAX_ENTRIES {
      return Err(RailError::message(format!(
        "local CAS tree '{}' exceeds its {MAX_ENTRIES}-entry cleanup bound",
        root.display()
      )));
    }
    let metadata = match fs::symlink_metadata(&path) {
      Ok(metadata) => metadata,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
      Err(error) => return Err(error.into()),
    };
    if metadata.is_dir() && !is_link_or_reparse(&metadata) {
      let entries = match fs::read_dir(&path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
        Err(error) => return Err(error.into()),
      };
      let mut children = Vec::new();
      for entry in entries {
        match entry {
          Ok(entry) => children.push(entry),
          Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
          Err(error) => return Err(error.into()),
        }
      }
      children.sort_by_key(|entry| entry.file_name());
      pending.extend(children.into_iter().map(|entry| entry.path()));
    } else {
      bytes = bytes
        .checked_add(metadata.len())
        .ok_or_else(|| RailError::message("local CAS cleanup byte count overflow"))?;
    }
  }
  Ok(bytes)
}

fn owner_marker_bytes(trust_domain: &str) -> RailResult<Vec<u8>> {
  validate_trust_domain(trust_domain)?;
  Ok(format!("{OWNER_MARKER_PREFIX}{trust_domain}\n").into_bytes())
}

fn ensure_owner_marker_existing(root: &Path, expected_trust_domain: Option<&str>) -> RailResult<()> {
  let marker = root.join("OWNER");
  let metadata = fs::symlink_metadata(&marker).map_err(|error| {
    RailError::message(format!(
      "local CAS root '{}' has no ownership marker: {error}",
      root.display()
    ))
  })?;
  let bytes = if metadata.is_file()
    && !is_link_or_reparse(&metadata)
    && has_single_link(&metadata)
    && metadata.len() == (OWNER_MARKER_PREFIX.len() + 65) as u64
  {
    fs::read(&marker)?
  } else {
    Vec::new()
  };
  let trust_domain = bytes
    .strip_prefix(OWNER_MARKER_PREFIX.as_bytes())
    .and_then(|value| value.strip_suffix(b"\n"))
    .and_then(|value| std::str::from_utf8(value).ok());
  if trust_domain.is_none_or(|value| {
    validate_trust_domain(value).is_err() || expected_trust_domain.is_some_and(|expected| value != expected)
  }) {
    return Err(RailError::with_help(
      format!(
        "local CAS root '{}' has an invalid or mismatched authority marker",
        root.display()
      ),
      "select the matching machine-owned trust domain or explicitly clean the isolated cache root",
    ));
  }
  Ok(())
}

fn owner_trust_domain(root: &Path) -> RailResult<String> {
  ensure_owner_marker_existing(root, None)?;
  let bytes = fs::read(root.join("OWNER"))?;
  bytes
    .strip_prefix(OWNER_MARKER_PREFIX.as_bytes())
    .and_then(|value| value.strip_suffix(b"\n"))
    .and_then(|value| std::str::from_utf8(value).ok())
    .map(str::to_string)
    .ok_or_else(|| RailError::message("local CAS authority marker is malformed"))
}

fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
  crate::utils::is_symlink_or_reparse(metadata)
}

#[cfg(windows)]
fn is_directory_reparse(metadata: &fs::Metadata) -> bool {
  use std::os::windows::fs::MetadataExt as _;

  const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0010;
  const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
  let attributes = metadata.file_attributes();
  attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 && attributes & FILE_ATTRIBUTE_DIRECTORY != 0
}

#[cfg(not(windows))]
fn is_directory_reparse(_metadata: &fs::Metadata) -> bool {
  false
}

#[cfg(unix)]
fn has_single_link(metadata: &fs::Metadata) -> bool {
  use std::os::unix::fs::MetadataExt as _;

  metadata.nlink() == 1
}

#[cfg(not(unix))]
fn has_single_link(_metadata: &fs::Metadata) -> bool {
  true
}

#[cfg(test)]
mod tests {
  use std::sync::{Arc, Barrier};

  use super::*;
  use crate::source::ContentDigest;

  fn action_key(value: u8) -> String {
    format!("{ACTION_KEY_PREFIX}{value:064x}")
  }

  fn base_action_key(value: u8) -> String {
    format!("{}{value:064x}", crate::compiler::native_cache::BASE_ACTION_KEY_PREFIX)
  }

  fn write_fixture(root: &Path, bytes: &[u8]) -> OutputManifest {
    let target = root.join("target");
    let deps = target.join("deps");
    fs::create_dir_all(&deps).expect("output directories should be created");
    fs::write(deps.join("artifact.rmeta"), bytes).expect("output bytes should be written");
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;
      fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).expect("target mode");
      fs::set_permissions(&deps, fs::Permissions::from_mode(0o755)).expect("deps mode");
      fs::set_permissions(deps.join("artifact.rmeta"), fs::Permissions::from_mode(0o644)).expect("file mode");
    }
    manifest(vec![
      OutputEntry {
        path: "target".to_string(),
        kind: OutputEntryKind::Directory { mode: 0o755 },
      },
      OutputEntry {
        path: "target/deps".to_string(),
        kind: OutputEntryKind::Directory { mode: 0o755 },
      },
      OutputEntry {
        path: "target/deps/artifact.rmeta".to_string(),
        kind: OutputEntryKind::File {
          digest: format!("sha256:{}", ContentDigest::sha256(bytes)),
          mode: 0o644,
          bytes: bytes.len() as u64,
        },
      },
    ])
  }

  fn manifest(mut entries: Vec<OutputEntry>) -> OutputManifest {
    entries.sort();
    let files = entries
      .iter()
      .filter(|entry| matches!(entry.kind, OutputEntryKind::File { .. }))
      .count();
    let directories = entries
      .iter()
      .filter(|entry| matches!(entry.kind, OutputEntryKind::Directory { .. }))
      .count();
    let symlinks = entries
      .iter()
      .filter(|entry| matches!(entry.kind, OutputEntryKind::Symlink { .. }))
      .count();
    let bytes = entries
      .iter()
      .filter_map(|entry| match entry.kind {
        OutputEntryKind::File { bytes, .. } => Some(bytes),
        _ => None,
      })
      .sum();
    let digest = super::super::output_manifest_digest(&entries).expect("manifest should hash");
    OutputManifest {
      version: super::super::OUTPUT_MANIFEST_VERSION,
      digest,
      entries,
      files,
      directories,
      symlinks,
      bytes,
    }
  }

  fn store_fixture(cas: &LocalCas, output: &Path, key: &str, manifest: &OutputManifest) -> StoreStats {
    let result = super::super::hermetic_result_digest(key, manifest.digest());
    let lookup = super::super::test_pre_context_lookup_key();
    let validation = super::super::FastCacheValidation::fixture(key, &lookup);
    cas
      .store(StoreRequest {
        action_key: key,
        lookup_key: &lookup,
        result_digest: &result,
        manifest,
        validation: &validation,
        compiler_units: 1,
        source_root: output,
      })
      .expect("fixture should enter the CAS")
  }

  fn native_fixture(root: &Path) -> (OutputManifest, NativeCompilerValidation) {
    native_fixture_with_stdout(root, b"")
  }

  fn native_fixture_with_stdout(root: &Path, stdout: &[u8]) -> (OutputManifest, NativeCompilerValidation) {
    let files = [
      ("target/outputs/dep-info", b"dep-info".as_slice()),
      ("target/outputs/metadata", b"metadata".as_slice()),
      ("target/streams/stdout", stdout),
      ("target/streams/stderr", b"".as_slice()),
    ];
    let mut paths = Vec::new();
    for (relative, bytes) in files {
      let path = root.join(relative);
      fs::create_dir_all(path.parent().expect("fixture parent")).expect("fixture directory");
      fs::write(&path, bytes).expect("fixture output");
      paths.push(path);
    }
    let manifest = super::super::capture_native_compiler_outputs(root, &paths).expect("native output manifest");
    let validation = crate::compiler::native_cache::tests::cas_validation_with_stdout(stdout);
    (manifest, validation)
  }

  fn store_native_fixture(
    cas: &LocalCas,
    output: &Path,
    manifest: &OutputManifest,
    validation: &NativeCompilerValidation,
  ) -> StoreStats {
    cas
      .store_native(prepared_native_fixture(output, manifest, validation))
      .expect("native fixture should enter the CAS")
      .1
  }

  fn prepared_native_fixture(
    output: &Path,
    manifest: &OutputManifest,
    validation: &NativeCompilerValidation,
  ) -> PreparedNativeResult {
    let staging = tempfile::tempdir().expect("native prepared staging");
    for entry in &manifest.entries {
      let source = output.join(&entry.path);
      let destination = staging.path().join(&entry.path);
      match entry.kind {
        OutputEntryKind::Directory { .. } => fs::create_dir(&destination).expect("prepared directory"),
        OutputEntryKind::File { .. } => {
          fs::copy(source, destination).expect("prepared file");
        }
        OutputEntryKind::Symlink { .. } => panic!("native fixtures have no symlinks"),
      }
    }
    PreparedNativeResult::from_verified_staging(staging, manifest.clone(), validation.clone())
  }

  fn native_revision_validation(base: &NativeCompilerValidation, revision: u64) -> NativeCompilerValidation {
    let revision_bytes = revision.to_le_bytes();
    let action_key = format!(
      "{}{}",
      crate::compiler::native_cache::ACTION_KEY_PREFIX,
      framed_identity(
        b"cargo-rail-cas-test-hot-crate-revision\0",
        &[(b"revision", revision_bytes.as_slice())],
      )
    );

    // Keep the production descriptor encoding as the fixture authority. Only
    // its fixed-width action identity changes between logical source revisions.
    let association = crate::compiler::native_cache::pack::association(base).expect("base association");
    let mut descriptor = association.bytes().to_vec();
    let action_offset = descriptor
      .windows(base.action_key().len())
      .position(|window| window == base.action_key().as_bytes())
      .expect("descriptor action identity");
    assert_eq!(action_key.len(), base.action_key().len());
    descriptor[action_offset..action_offset + action_key.len()].copy_from_slice(action_key.as_bytes());

    let result_version = 4_u32.to_le_bytes();
    let result_key = format!(
      "{}{}",
      crate::compiler::native_cache::RESULT_KEY_PREFIX,
      framed_identity(
        b"cargo-rail-native-compiler-result\0",
        &[
          (b"version", result_version.as_slice()),
          (b"descriptor", descriptor.as_slice()),
        ],
      )
    );
    crate::compiler::native_cache::pack::decode_association(&descriptor, &action_key, &result_key)
      .expect("revision association");

    let mut value = serde_json::to_value(base).expect("native validation fixture");
    let object = value.as_object_mut().expect("native validation object");
    object.insert("action_key".to_string(), action_key.into());
    object.insert("result_key".to_string(), result_key.into());
    let validation = serde_json::from_value::<NativeCompilerValidation>(value).expect("revision validation");
    validation.validate_object().expect("valid revision identity");
    validation
  }

  fn native_action_result_for_revision(revision: u64) -> String {
    let revision_bytes = revision.to_le_bytes();
    format!(
      "{ACTION_RESULT_PREFIX}{}",
      framed_identity(
        b"cargo-rail-cas-test-hot-crate-action-result\0",
        &[(b"revision", revision_bytes.as_slice())],
      )
    )
  }

  fn write_native_revision_state(cas: &LocalCas, validation: &NativeCompilerValidation, action_result: &str) {
    let state = NativeActionState {
      version: NATIVE_ACTION_STATE_VERSION,
      action_key: validation.action_key().to_string(),
      state: NativeActionStateKind::UniqueResult {
        result_key: validation.result_key().to_string(),
        action_result: action_result.to_string(),
        origins: NativeResultOrigins {
          local: true,
          remote: None,
        },
      },
    };
    validate_native_action_state(&state, validation.action_key()).expect("revision action state");
    let action_hex = validated_id_hex(
      validation.action_key(),
      crate::compiler::native_cache::ACTION_KEY_PREFIX,
    )
    .expect("revision action key");
    fs::write(
      cas
        .root()
        .join(NATIVE_ACTION_STATE_DIRECTORY)
        .join(format!("{action_hex}.json")),
      canonical_json(&state).expect("canonical revision state"),
    )
    .expect("revision action state should be written");
  }

  #[test]
  fn native_environment_selector_round_trips_canonical_names_and_status() {
    let cache = tempfile::tempdir().expect("cache base");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let key = base_action_key(1);
    let names = vec!["CARGO_CFG_TARGET_ARCH".to_string(), "P73_SELECTED".to_string()];

    assert_eq!(cas.native_environment_selector(&key).expect("missing lookup"), None);
    assert_eq!(
      cas
        .publish_native_environment_selector(&key, &names)
        .expect("initial publication"),
      NativeEnvironmentSelectorPublication::Created
    );
    assert_eq!(
      cas.native_environment_selector(&key).expect("published lookup"),
      Some(names.clone())
    );
    assert_eq!(
      cas
        .publish_native_environment_selector(&key, &names)
        .expect("equal publication"),
      NativeEnvironmentSelectorPublication::Converged
    );
    assert_eq!(cas.status().expect("status").index_files, 1);

    let path = cas.native_environment_selector_path(&key).expect("selector path");
    assert_eq!(
      fs::read(&path).expect("selector bytes"),
      br#"["CARGO_CFG_TARGET_ARCH","P73_SELECTED"]"#
    );
    let file = File::open(&path).expect("selector file");
    assert!(
      crate::utils::private_file_matches_path(&file, &path, file.metadata().expect("selector metadata").len())
        .expect("selector privacy")
    );
  }

  #[test]
  fn native_environment_selector_divergence_preserves_the_first_binding() {
    let cache = tempfile::tempdir().expect("cache base");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let key = base_action_key(2);
    let first = vec!["FIRST".to_string()];
    let second = vec!["SECOND".to_string()];
    cas
      .publish_native_environment_selector(&key, &first)
      .expect("first publication");

    assert_eq!(
      cas
        .publish_native_environment_selector(&key, &second)
        .expect("divergent publication"),
      NativeEnvironmentSelectorPublication::Diverged
    );
    let selector_path = cas.native_environment_selector_path(&key).expect("selector path");
    assert_eq!(
      fs::read(&selector_path).expect("selector bytes"),
      canonical_json(&first).expect("canonical first selector")
    );
    let conflict_path = cas
      .native_environment_selector_conflict_path(&key)
      .expect("conflict path");
    assert_eq!(
      fs::read(&conflict_path).expect("conflict bytes"),
      NATIVE_ENVIRONMENT_SELECTOR_CONFLICT_BYTES
    );
    let error = cas
      .native_environment_selector(&key)
      .expect_err("conflicted selector must fail closed");
    assert!(error.to_string().contains("durably conflicted"), "{error}");
    assert_eq!(cas.status().expect("status").index_files, 2);

    drop(cas);
    let reopened = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should reopen");
    let error = reopened
      .native_environment_selector(&key)
      .expect_err("conflict must survive reopening");
    assert!(error.to_string().contains("durably conflicted"), "{error}");
  }

  #[test]
  fn concurrent_native_environment_selector_publishers_converge() {
    let cache = tempfile::tempdir().expect("cache base");
    let cas = Arc::new(LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open"));
    let barrier = Arc::new(Barrier::new(2));
    let key = base_action_key(3);
    let names = vec!["P73_SELECTED".to_string()];
    let outcomes = std::thread::scope(|scope| {
      let mut handles = Vec::new();
      for _ in 0..2 {
        let cas = Arc::clone(&cas);
        let barrier = Arc::clone(&barrier);
        let key = key.clone();
        let names = names.clone();
        handles.push(scope.spawn(move || {
          barrier.wait();
          cas
            .publish_native_environment_selector(&key, &names)
            .expect("concurrent publication")
        }));
      }
      handles
        .into_iter()
        .map(|handle| handle.join().expect("publisher should finish"))
        .collect::<Vec<_>>()
    });

    assert_eq!(
      outcomes
        .iter()
        .filter(|outcome| **outcome == NativeEnvironmentSelectorPublication::Created)
        .count(),
      1
    );
    assert_eq!(
      outcomes
        .iter()
        .filter(|outcome| **outcome == NativeEnvironmentSelectorPublication::Converged)
        .count(),
      1
    );
    assert_eq!(cas.native_environment_selector(&key).expect("lookup"), Some(names));
  }

  #[test]
  fn concurrent_differing_native_environment_selector_publishers_conflict_terminally() {
    let cache = tempfile::tempdir().expect("cache base");
    let cas = Arc::new(LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open"));
    let barrier = Arc::new(Barrier::new(2));
    let key = base_action_key(10);
    let selectors = [vec!["FIRST".to_string()], vec!["SECOND".to_string()]];
    let outcomes = std::thread::scope(|scope| {
      selectors
        .iter()
        .map(|names| {
          let cas = Arc::clone(&cas);
          let barrier = Arc::clone(&barrier);
          let key = key.clone();
          scope.spawn(move || {
            barrier.wait();
            cas
              .publish_native_environment_selector(&key, names)
              .expect("concurrent publication")
          })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|handle| handle.join().expect("publisher should finish"))
        .collect::<Vec<_>>()
    });

    assert!(
      outcomes
        .iter()
        .all(|outcome| *outcome != NativeEnvironmentSelectorPublication::Converged),
      "differing selectors cannot converge: {outcomes:?}"
    );
    assert!(
      outcomes.contains(&NativeEnvironmentSelectorPublication::Diverged),
      "one publisher must observe divergence: {outcomes:?}"
    );
    let selector_path = cas.native_environment_selector_path(&key).expect("selector path");
    let selector_bytes = fs::read(&selector_path).expect("winning selector bytes");
    assert!(
      selectors
        .iter()
        .any(|names| canonical_json(names).expect("canonical selector") == selector_bytes),
      "the immutable selector must contain one complete publication"
    );
    let conflict_path = cas
      .native_environment_selector_conflict_path(&key)
      .expect("conflict path");
    assert_eq!(
      fs::read(conflict_path).expect("conflict bytes"),
      NATIVE_ENVIRONMENT_SELECTOR_CONFLICT_BYTES
    );
    let error = cas
      .native_environment_selector(&key)
      .expect_err("differing concurrent publications must fail closed");
    assert!(error.to_string().contains("durably conflicted"), "{error}");
  }

  #[test]
  fn retained_native_action_hit_blocks_selector_conflict_publication() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let (manifest, validation) = native_fixture(output.path());
    store_native_fixture(&cas, output.path(), &manifest, &validation);
    let key = base_action_key(13);
    let first = vec!["FIRST".to_string()];
    let second = vec!["SECOND".to_string()];
    cas
      .publish_native_environment_selector(&key, &first)
      .expect("first selector publication");
    let NativeActionLookup::Hit(hit) = cas
      .native_action(validation.action_key())
      .expect("native action lookup")
    else {
      panic!("stored native action must be authoritative");
    };

    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
      let publisher = scope.spawn(|| {
        started_tx.send(()).expect("publisher start signal");
        let outcome = cas
          .publish_native_environment_selector(&key, &second)
          .expect("conflicting selector publication");
        finished_tx.send(outcome).expect("publisher completion signal");
      });
      started_rx.recv().expect("publisher must start");
      hit
        .validate_environment_selector(&key, first.iter().map(String::as_str))
        .expect("retained selector authority");
      assert!(matches!(
        finished_rx.recv_timeout(std::time::Duration::from_millis(100)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
      ));

      drop(hit);
      assert_eq!(
        finished_rx
          .recv_timeout(std::time::Duration::from_secs(5))
          .expect("publisher must finish after the hit is released"),
        NativeEnvironmentSelectorPublication::Diverged
      );
      publisher.join().expect("publisher thread");
    });
  }

  #[test]
  fn native_environment_selector_rejects_noncanonical_input_and_state() {
    let cache = tempfile::tempdir().expect("cache base");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let unsorted_key = base_action_key(4);
    let unsorted = vec!["Z".to_string(), "A".to_string()];
    let error = cas
      .publish_native_environment_selector(&unsorted_key, &unsorted)
      .expect_err("unsorted names must fail");
    assert!(error.to_string().contains("strictly sorted and unique"), "{error}");
    assert_eq!(
      cas
        .native_environment_selector(&unsorted_key)
        .expect("missing selector"),
      None
    );

    let malformed_key = base_action_key(5);
    cas
      .publish_native_environment_selector(&malformed_key, &["VALID".to_string()])
      .expect("valid publication");
    let malformed_path = cas
      .native_environment_selector_path(&malformed_key)
      .expect("malformed selector path");
    fs::write(&malformed_path, b"{").expect("malformed selector");
    let error = cas
      .native_environment_selector(&malformed_key)
      .expect_err("malformed state must fail");
    assert!(error.to_string().contains("is malformed"), "{error}");

    let noncanonical_key = base_action_key(6);
    let noncanonical_path = cas
      .native_environment_selector_path(&noncanonical_key)
      .expect("noncanonical selector path");
    fs::write(&noncanonical_path, b"[ \"VALID\" ]").expect("noncanonical selector");
    let error = cas
      .native_environment_selector(&noncanonical_key)
      .expect_err("noncanonical state must fail");
    assert!(error.to_string().contains("not canonically encoded"), "{error}");
  }

  #[test]
  fn native_environment_selector_validates_its_identity_names_and_bounds() {
    let cache = tempfile::tempdir().expect("cache base");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let key = base_action_key(9);

    let error = cas
      .native_environment_selector(&format!(
        "{}not-a-digest",
        crate::compiler::native_cache::BASE_ACTION_KEY_PREFIX
      ))
      .expect_err("invalid identity must fail");
    assert!(error.to_string().contains("canonical SHA-256"), "{error}");

    for invalid in [
      String::new(),
      "A\0B".to_string(),
      "A=B".to_string(),
      "A\nB".to_string(),
      CACHE_BASE_ENV.to_string(),
      "A".repeat(257),
    ] {
      let error = cas
        .publish_native_environment_selector(&key, &[invalid])
        .expect_err("invalid environment name must fail");
      assert!(error.to_string().contains("invalid environment name"), "{error}");
    }

    let too_many = (0..=512).map(|index| format!("ENV_{index:04}")).collect::<Vec<_>>();
    let error = cas
      .publish_native_environment_selector(&key, &too_many)
      .expect_err("name-count bound must fail");
    assert!(error.to_string().contains("512-name bound"), "{error}");
  }

  #[test]
  fn native_environment_selector_rejects_canonical_compiler_invalid_state() {
    let cache = tempfile::tempdir().expect("cache base");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let private_key = base_action_key(11);
    let private_path = cas
      .native_environment_selector_path(&private_key)
      .expect("private selector path");
    fs::write(
      private_path,
      canonical_json(&vec![CACHE_BASE_ENV.to_string()]).expect("canonical private selector"),
    )
    .expect("private selector bytes");
    let error = cas
      .native_environment_selector(&private_key)
      .expect_err("private compiler capability must fail");
    assert!(error.to_string().contains("invalid environment name"), "{error}");

    let too_many_key = base_action_key(12);
    let too_many_path = cas
      .native_environment_selector_path(&too_many_key)
      .expect("bounded selector path");
    let too_many = (0..=512).map(|index| format!("ENV_{index:04}")).collect::<Vec<_>>();
    fs::write(
      too_many_path,
      canonical_json(&too_many).expect("canonical oversized selector"),
    )
    .expect("oversized selector bytes");
    let error = cas
      .native_environment_selector(&too_many_key)
      .expect_err("canonical selector over compiler name bound must fail");
    assert!(error.to_string().contains("512-name bound"), "{error}");
  }

  #[cfg(unix)]
  #[test]
  fn native_environment_selector_rejects_linked_state() {
    use std::os::unix::fs::symlink;

    let cache = tempfile::tempdir().expect("cache base");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let hard_link_key = base_action_key(7);
    cas
      .publish_native_environment_selector(&hard_link_key, &["VALID".to_string()])
      .expect("valid publication");
    let hard_link_path = cas
      .native_environment_selector_path(&hard_link_key)
      .expect("hard-linked selector path");
    fs::hard_link(&hard_link_path, cache.path().join("outside-hard-link")).expect("hard link");
    let error = cas
      .native_environment_selector(&hard_link_key)
      .expect_err("hard-linked state must fail");
    assert!(
      error.to_string().contains("not a private bounded regular file"),
      "{error}"
    );

    let symlink_key = base_action_key(8);
    let symlink_path = cas
      .native_environment_selector_path(&symlink_key)
      .expect("linked selector path");
    let outside = cache.path().join("outside-selector");
    fs::write(&outside, b"[]").expect("outside selector");
    symlink(&outside, &symlink_path).expect("selector symlink");
    let error = cas
      .native_environment_selector(&symlink_key)
      .expect_err("symlinked state must fail");
    assert!(
      error.to_string().contains("not a private bounded regular file"),
      "{error}"
    );
  }

  #[test]
  fn native_hot_crate_revision_index_keeps_current_lookup_history_independent() {
    const REVISIONS: u64 = 4_096;

    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let (manifest, base) = native_fixture(output.path());
    let current = native_revision_validation(&base, REVISIONS - 1);
    let cas = LocalCas::open_at(cache.path(), 64 * 1024 * 1024).expect("CAS should open");
    let current_stats = store_native_fixture(&cas, output.path(), &manifest, &current);
    let current_action_result = current_stats.action_result.expect("current action result");

    let NativeActionLookup::Hit(before) = cas
      .native_action(current.action_key())
      .expect("baseline current lookup")
    else {
      panic!("current action should be authoritative");
    };
    let baseline_bytes_read = before.bytes_read;
    drop(before);

    // Past payloads are intentionally not materialized: this is a structural
    // proof that the exact action-state index does not traverse revision
    // history. Retained wall/RSS and payload-scale claims belong to benchmarks.
    let mut action_keys = BTreeSet::new();
    let mut result_keys = BTreeSet::new();
    for revision in 0..REVISIONS {
      let validation = native_revision_validation(&base, revision);
      assert!(action_keys.insert(validation.action_key().to_string()));
      assert!(result_keys.insert(validation.result_key().to_string()));
      if revision != REVISIONS - 1 {
        let action_result = native_action_result_for_revision(revision);
        assert_ne!(action_result, current_action_result);
        write_native_revision_state(&cas, &validation, &action_result);
      }
    }
    assert_eq!(action_keys.len(), REVISIONS as usize);
    assert_eq!(result_keys.len(), REVISIONS as usize);

    let mut fanout = BTreeMap::<String, BTreeSet<String>>::new();
    for entry in fs::read_dir(cas.root().join(NATIVE_ACTION_STATE_DIRECTORY)).expect("native action states") {
      let entry = entry.expect("native action entry");
      let name = entry.file_name().into_string().expect("UTF-8 action state name");
      let action_hex = name.strip_suffix(".json").expect("canonical action state name");
      let action_key = format!("{}{action_hex}", crate::compiler::native_cache::ACTION_KEY_PREFIX);
      let state = decode_native_action_state(&fs::read(entry.path()).expect("action state"), &action_key)
        .expect("canonical action state");
      let NativeActionStateKind::UniqueResult { result_key, .. } = state.state else {
        panic!("a revision must have exactly one result");
      };
      fanout.entry(action_key).or_default().insert(result_key);
    }
    assert_eq!(fanout.len(), REVISIONS as usize);
    assert!(fanout.values().all(|results| results.len() == 1));
    assert_eq!(fanout.keys().cloned().collect::<BTreeSet<_>>(), action_keys);

    let NativeActionLookup::Hit(after) = cas
      .native_action(current.action_key())
      .expect("current lookup after history")
    else {
      panic!("revision history must not weaken current authority");
    };
    assert_eq!(after.validation, current);
    assert_eq!(after.bytes_read, baseline_bytes_read);
    assert_eq!(
      fs::read_dir(cas.root().join("results"))
        .expect("result namespace")
        .count(),
      1
    );
  }

  fn prepopulate_unrelated_result_namespace(cas: &LocalCas, population: usize, reserved: &BTreeSet<String>) {
    let results = cas.root().join("results");
    let mut candidate = 0_u64;
    let mut created = 0usize;
    // Canonically named empty directories are scan tripwires, not result
    // fixtures. An under-capacity direct admission must ignore every unrelated
    // entry; trying to validate one would fail the test immediately.
    while created < population {
      let action_result = format!("{ACTION_RESULT_PREFIX}{candidate:064x}");
      candidate = candidate.checked_add(1).expect("bounded result population");
      if reserved.contains(&action_result) {
        continue;
      }
      let result_hex = validated_id_hex(&action_result, ACTION_RESULT_PREFIX).expect("tripwire result identity");
      fs::create_dir(results.join(result_hex)).expect("result namespace tripwire");
      created += 1;
    }
    assert_eq!(fs::read_dir(results).expect("result namespace").count(), population);
  }

  #[cfg(any(unix, windows))]
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  struct TestFileIdentity {
    volume: u64,
    index: u64,
    bytes: u64,
  }

  #[cfg(unix)]
  fn test_file_identity(path: &Path) -> TestFileIdentity {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = fs::symlink_metadata(path).expect("staged blob metadata");
    assert!(metadata.is_file());
    assert_eq!(metadata.nlink(), 1);
    TestFileIdentity {
      volume: metadata.dev(),
      index: metadata.ino(),
      bytes: metadata.len(),
    }
  }

  #[cfg(windows)]
  fn test_file_identity(path: &Path) -> TestFileIdentity {
    let file = cargo_rail_windows_fs::open_for_observation(path).expect("staged blob file");
    let information = cargo_rail_windows_fs::observe_file(&file).expect("staged blob identity");
    cargo_rail_windows_fs::prove_local_ntfs(&file, information.volume_serial_number)
      .expect("staged blob local NTFS proof");
    assert_eq!(information.number_of_links, 1);
    TestFileIdentity {
      volume: information.volume_serial_number,
      index: information.file_id,
      bytes: information.size,
    }
  }

  type NativeBatchAdmissionEvidence = (Vec<(String, String, u64, u64)>, Vec<u64>, u64);

  fn observe_native_batch_admission(population: usize, batch_size: usize) -> NativeBatchAdmissionEvidence {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let (manifest, base) = native_fixture(output.path());
    let cas = LocalCas::open_at(cache.path(), 64 * 1024 * 1024).expect("CAS should open");
    let staged = (0..batch_size)
      .map(|revision| {
        let validation = native_revision_validation(&base, revision as u64);
        cas
          .stage_native(prepared_native_fixture(output.path(), &manifest, &validation))
          .expect("native result should stage")
      })
      .collect::<Vec<_>>();
    #[cfg(any(unix, windows))]
    let staged_blob_identities = staged
      .iter()
      .flat_map(|result| {
        fs::read_dir(result.staged.payload.join("blobs"))
          .expect("staged blob directory")
          .map(|entry| {
            let entry = entry.expect("staged blob entry");
            (
              result.action_result.clone(),
              entry.file_name(),
              test_file_identity(&entry.path()),
            )
          })
          .collect::<Vec<_>>()
      })
      .collect::<Vec<_>>();
    let reserved = staged
      .iter()
      .map(|result| result.action_result.clone())
      .collect::<BTreeSet<_>>();
    assert_eq!(reserved.len(), batch_size);
    prepopulate_unrelated_result_namespace(&cas, population, &reserved);

    let missing = native_revision_validation(&base, u64::MAX);
    let NativeActionLookup::Miss(miss) = cas.native_action(missing.action_key()).expect("absent action lookup") else {
      panic!("an absent action must miss");
    };
    assert_eq!(miss.reason, "action_not_found");
    let miss_bytes_read = miss.bytes_read;

    let committed = cas.commit_new_native_batch(staged).expect("native batch admission");
    assert_eq!(committed.len(), batch_size);
    #[cfg(any(unix, windows))]
    for (action_result, name, identity) in staged_blob_identities {
      let destination = result_path(cas.root(), &action_result)
        .expect("published result path")
        .join("blobs")
        .join(name);
      assert_eq!(
        test_file_identity(&destination),
        identity,
        "authoritative admission recopied an already staged immutable blob"
      );
    }
    assert_eq!(
      fs::read_dir(cas.root().join("results"))
        .expect("result namespace")
        .count(),
      population + batch_size
    );
    assert_eq!(
      fs::read_dir(cas.root().join(NATIVE_ACTION_STATE_DIRECTORY))
        .expect("native action namespace")
        .count(),
      batch_size
    );

    let writes = committed
      .iter()
      .map(|(validation, stats)| {
        (
          validation.action_key().to_string(),
          stats.action_result.clone().expect("admitted action result"),
          stats.objects_written,
          stats.bytes_written,
        )
      })
      .collect::<Vec<_>>();
    let unit_objects = writes[0].2;
    let unit_bytes = writes[0].3;
    assert!(unit_objects > 0 && unit_bytes > 0);
    assert!(
      writes
        .iter()
        .all(|(_, _, objects, bytes)| *objects == unit_objects && *bytes == unit_bytes)
    );
    assert_eq!(
      writes.iter().map(|(_, _, objects, _)| objects).sum::<u64>(),
      unit_objects * batch_size as u64
    );
    assert_eq!(
      writes.iter().map(|(_, _, _, bytes)| bytes).sum::<u64>(),
      unit_bytes * batch_size as u64
    );

    let lookup_bytes = committed
      .iter()
      .map(|(validation, _)| {
        let NativeActionLookup::Hit(hit) = cas
          .native_action(validation.action_key())
          .expect("admitted action lookup")
        else {
          panic!("admitted native action should be authoritative");
        };
        hit.bytes_read
      })
      .collect();
    (writes, lookup_bytes, miss_bytes_read)
  }

  #[test]
  fn native_batch_admission_work_is_independent_of_result_namespace_population() {
    for batch_size in [1, 32, 256] {
      let small = observe_native_batch_admission(100, batch_size);
      let large = observe_native_batch_admission(10_000, batch_size);
      assert_eq!(small, large, "batch {batch_size} work changed with store population");
      assert_eq!(
        small.2, 0,
        "an exact absent-action lookup must not read unrelated results"
      );
    }
  }

  #[test]
  fn native_action_lookup_does_not_materialize_and_retained_view_restores() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let restore_parent = tempfile::tempdir().expect("restore parent");
    let (manifest, validation) = native_fixture(output.path());
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    store_native_fixture(&cas, output.path(), &manifest, &validation);

    let destination = restore_parent.path().join("native-output");
    let NativeActionLookup::Hit(cached) = cas.native_action(validation.action_key()).expect("action lookup") else {
      panic!("verified native action should be authoritative");
    };
    assert!(!destination.exists(), "action lookup must not materialize output");

    let NativeCacheLookup::Hit(hit) = cached.restore(&destination) else {
      panic!("verified native action should restore");
    };
    assert_eq!(hit.bytes_restored, manifest.bytes);
    assert_eq!(
      fs::read(destination.join("target/outputs/dep-info")).expect("dep-info"),
      b"dep-info"
    );
    assert_eq!(
      fs::read(destination.join("target/outputs/metadata")).expect("metadata"),
      b"metadata"
    );
    drop(cached);

    let action_hex = validated_action_key_hex(validation.action_key()).expect("native action key");
    fs::remove_file(
      cas
        .root
        .join(NATIVE_ACTION_STATE_DIRECTORY)
        .join(format!("{action_hex}.json")),
    )
    .expect("remove authoritative action state");
    assert!(matches!(
      cas
        .native_action(validation.action_key())
        .expect("missing action lookup"),
      NativeActionLookup::Miss(_)
    ));
  }

  #[test]
  fn native_registered_restore_uses_only_the_supplied_staging_path() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let restore_parent = tempfile::tempdir().expect("restore parent");
    let (manifest, validation) = native_fixture(output.path());
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    store_native_fixture(&cas, output.path(), &manifest, &validation);
    let NativeActionLookup::Hit(cached) = cas.native_action(validation.action_key()).expect("action lookup") else {
      panic!("verified native action should be authoritative");
    };
    let destination = restore_parent.path().join("registered-output");
    let staging = restore_parent.path().join("registered-staging");

    let NativeCacheLookup::Hit(hit) = cached.restore_registered(&destination, &staging) else {
      panic!("verified native action should restore through registered staging");
    };

    assert_eq!(hit.bytes_restored, manifest.bytes);
    assert!(
      !staging.exists(),
      "successful restore must remove the empty staging shell"
    );
    assert_eq!(
      fs::read(destination.join("target/outputs/metadata")).expect("metadata"),
      b"metadata"
    );
    let entries = fs::read_dir(restore_parent.path())
      .expect("restore parent entries")
      .map(|entry| entry.expect("restore parent entry").file_name())
      .collect::<Vec<_>>();
    assert_eq!(entries, [std::ffi::OsString::from("registered-output")]);
  }

  #[test]
  fn native_registered_restore_rejects_prepositioned_paths_without_creating_residue() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let restore_parent = tempfile::tempdir().expect("restore parent");
    let (manifest, validation) = native_fixture(output.path());
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    store_native_fixture(&cas, output.path(), &manifest, &validation);
    let NativeActionLookup::Hit(cached) = cas.native_action(validation.action_key()).expect("action lookup") else {
      panic!("verified native action should be authoritative");
    };

    let destination = restore_parent.path().join("prepositioned-output");
    let absent_staging = restore_parent.path().join("absent-staging");
    fs::write(&destination, b"existing").expect("preposition destination");
    let NativeCacheLookup::Miss(miss) = cached.restore_registered(&destination, &absent_staging) else {
      panic!("prepositioned destination must fail closed");
    };
    assert_eq!(miss.reason, "materialization_destination_prepositioned");
    assert!(
      !absent_staging.exists(),
      "destination rejection must not create staging"
    );

    let absent_destination = restore_parent.path().join("absent-output");
    let staging = restore_parent.path().join("prepositioned-staging");
    fs::create_dir(&staging).expect("preposition staging");
    fs::write(staging.join("sentinel"), b"untouched").expect("staging sentinel");
    let NativeCacheLookup::Miss(miss) = cached.restore_registered(&absent_destination, &staging) else {
      panic!("prepositioned staging must fail closed");
    };
    assert_eq!(miss.reason, "materialization_staging_prepositioned");
    assert!(!absent_destination.exists());
    assert_eq!(
      fs::read(staging.join("sentinel")).expect("staging sentinel"),
      b"untouched"
    );
  }

  #[test]
  fn native_manifest_must_match_the_validated_output_contract() {
    let output = tempfile::tempdir().expect("output root");
    let (mut manifest, validation) = native_fixture(output.path());
    let metadata = manifest
      .entries
      .iter_mut()
      .find(|entry| entry.path == "target/outputs/metadata")
      .expect("metadata entry");
    let OutputEntryKind::File { digest, .. } = &mut metadata.kind else {
      panic!("metadata should be a file");
    };
    *digest = format!("sha256:{}", ContentDigest::sha256(b"forged!"));
    manifest.digest = super::super::output_manifest_digest(&manifest.entries).expect("forged manifest digest");

    assert!(
      validate_manifest(&manifest).is_ok(),
      "the generic manifest remains internally valid"
    );
    assert!(
      validate_native_output_manifest(&manifest, &validation).is_err(),
      "native validation must retain authority over each output slot"
    );
  }

  #[test]
  fn native_action_lookup_ignores_unrelated_primary_pins() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let (manifest, validation) = native_fixture(output.path());
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024 * 1024).expect("CAS should open");
    store_native_fixture(&cas, output.path(), &manifest, &validation);

    let lookup = super::super::test_pre_context_lookup_key();
    for value in 0_u64..32 {
      let action_key = format!("{ACTION_KEY_PREFIX}{value:064x}");
      let pin = ActionPin {
        version: CAS_VERSION,
        action_key: action_key.clone(),
        action_result: format!("{ACTION_RESULT_PREFIX}{value:064x}"),
        lookup_key: lookup.clone(),
        created_unix_nanos: u128::from(value),
      };
      fs::write(
        cas.root.join("pins").join(format!(
          "{}.json",
          validated_action_key_hex(&action_key).expect("action key")
        )),
        canonical_json(&pin).expect("pin JSON"),
      )
      .expect("irrelevant pin");
    }

    assert!(matches!(
      cas.native_action(validation.action_key()).expect("action lookup"),
      NativeActionLookup::Hit(_)
    ));
  }

  #[test]
  fn legacy_native_action_state_is_upgraded_without_becoming_v6_authority() {
    const LEGACY_STATE: &[u8] =
      b"{\"action_key\":\"compiler-action-v5-sha256-legacy\",\"state\":{\"kind\":\"unique_result\"},\"version\":1}\n";
    const LEGACY_LEDGER: &[u8] = b"{\"version\":1,\"terminal_states\":0,\"terminal_bytes\":0,\"disabled\":false}";

    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let (manifest, validation) = native_fixture(output.path());
    let initialized = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should initialize");
    let root = initialized.root().to_path_buf();
    drop(initialized);

    let current_directory = root.join(NATIVE_ACTION_STATE_DIRECTORY);
    fs::remove_dir(&current_directory).expect("remove post-upgrade action directory from legacy fixture");
    fs::write(root.join(NATIVE_LEDGER_STATE_FILE), LEGACY_LEDGER).expect("legacy terminal ledger");
    let legacy_directory = root.join(LEGACY_NATIVE_ACTION_STATE_DIRECTORY);
    fs::create_dir(&legacy_directory).expect("legacy native action directory");
    let action_hex = validated_action_key_hex(validation.action_key()).expect("current action key");
    let legacy_state = legacy_directory.join(format!("{action_hex}.json"));
    fs::write(&legacy_state, LEGACY_STATE).expect("legacy state fixture");

    assert!(
      !current_directory.exists(),
      "fixture must begin without the v2 namespace"
    );
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should upgrade around legacy state");
    assert!(
      current_directory.is_dir(),
      "upgrade must create the v2 action namespace"
    );
    assert!(
      root.join(NATIVE_LEDGER_STATE_FILE).is_file(),
      "upgrade must retain a current ledger"
    );
    assert_eq!(fs::read(&legacy_state).expect("preserved legacy state"), LEGACY_STATE);
    let NativeActionLookup::Miss(miss) = cas.native_action(validation.action_key()).expect("legacy-key lookup") else {
      panic!("legacy state must not grant current action authority");
    };
    assert_eq!(miss.reason, "action_not_found");
    let ledger = validate_native_ledger(cas.root()).expect("current native ledger");
    assert_eq!(ledger.terminal_states, 0);
    assert_eq!(ledger.terminal_bytes, 0);
    assert!(!ledger.disabled);
    let status = cas.status().expect("CAS status");
    assert_eq!(status.native_actions, 0);
    assert_eq!(status.native_unique, 0);
    assert_eq!(status.native_conflicted, 0);
    assert_eq!(status.native_quarantined, 0);

    store_native_fixture(&cas, output.path(), &manifest, &validation);
    assert!(
      cas
        .root()
        .join(NATIVE_ACTION_STATE_DIRECTORY)
        .join(format!("{action_hex}.json"))
        .is_file(),
      "new authority must use the v2 action-state namespace"
    );
    assert_eq!(
      fs::read(&legacy_state).expect("legacy bytes after publish"),
      LEGACY_STATE
    );
    let status = cas.status().expect("published CAS status");
    assert_eq!(status.native_actions, 1);
    assert_eq!(status.native_unique, 1);
    assert_eq!(status.native_conflicted, 0);
    assert_eq!(status.native_quarantined, 0);
    let ledger = validate_native_ledger(cas.root()).expect("published native ledger");
    assert_eq!(ledger.terminal_states, 0);
    assert_eq!(ledger.terminal_bytes, 0);
    assert!(!ledger.disabled);
  }

  #[test]
  fn native_action_state_is_the_only_native_authority() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let (manifest, validation) = native_fixture(output.path());
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    store_native_fixture(&cas, output.path(), &manifest, &validation);

    let action_hex = validated_action_key_hex(validation.action_key()).expect("action key");
    fs::remove_file(
      cas
        .root
        .join(NATIVE_ACTION_STATE_DIRECTORY)
        .join(format!("{action_hex}.json")),
    )
    .expect("remove action state");

    let reopened = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should reopen");
    assert!(matches!(
      reopened.native_action(validation.action_key()).expect("missing action"),
      NativeActionLookup::Miss(_)
    ));
  }

  #[test]
  fn distinct_native_results_make_the_action_permanently_conflicted() {
    let cache = tempfile::tempdir().expect("cache base");
    let first_output = tempfile::tempdir().expect("first output root");
    let second_output = tempfile::tempdir().expect("second output root");
    let (first_manifest, first_validation) = native_fixture(first_output.path());
    let (second_manifest, second_validation) = native_fixture_with_stdout(second_output.path(), b"different");
    assert_eq!(first_validation.action_key(), second_validation.action_key());
    assert_ne!(first_validation.result_key(), second_validation.result_key());
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    store_native_fixture(&cas, first_output.path(), &first_manifest, &first_validation);

    let staging = tempfile::tempdir().expect("second prepared staging");
    for entry in &second_manifest.entries {
      let source = second_output.path().join(&entry.path);
      let destination = staging.path().join(&entry.path);
      match entry.kind {
        OutputEntryKind::Directory { .. } => fs::create_dir(&destination).expect("prepared directory"),
        OutputEntryKind::File { .. } => {
          fs::copy(source, destination).expect("prepared file");
        }
        OutputEntryKind::Symlink { .. } => panic!("native fixtures have no symlinks"),
      }
    }
    let error = cas
      .store_native(PreparedNativeResult::from_verified_staging(
        staging,
        second_manifest,
        second_validation,
      ))
      .expect_err("a distinct result must conflict");
    assert!(error.to_string().contains("two different verified results"), "{error}");
    let NativeActionLookup::Miss(miss) = cas
      .native_action(first_validation.action_key())
      .expect("conflict lookup")
    else {
      panic!("a conflicted action must not restore");
    };
    assert_eq!(miss.reason, "action_conflicted");

    drop(cas);
    let reopened = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should reopen");
    reopened.garbage_collect(0, None).expect("GC must preserve the ledger");
    let NativeActionLookup::Miss(miss) = reopened
      .native_action(first_validation.action_key())
      .expect("restarted conflict lookup")
    else {
      panic!("restart and GC must not make a conflict unique");
    };
    assert_eq!(miss.reason, "action_conflicted");
    assert_eq!(
      fs::read_dir(reopened.root.join(NATIVE_ACTION_STATE_DIRECTORY))
        .expect("native actions")
        .count(),
      1
    );
    assert_eq!(fs::read_dir(reopened.root.join("results")).expect("results").count(), 0);
  }

  #[test]
  fn full_terminal_ledger_disables_native_authority_without_erasing_unique_state() {
    let cache = tempfile::tempdir().expect("cache base");
    let first_output = tempfile::tempdir().expect("first output root");
    let second_output = tempfile::tempdir().expect("second output root");
    let (first_manifest, first_validation) = native_fixture(first_output.path());
    let (second_manifest, second_validation) = native_fixture_with_stdout(second_output.path(), b"different");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    store_native_fixture(&cas, first_output.path(), &first_manifest, &first_validation);
    write_native_ledger(
      cas.root(),
      &NativeLedgerState {
        version: NATIVE_ACTION_STATE_VERSION,
        terminal_states: MAX_NATIVE_TERMINAL_STATES,
        terminal_bytes: MAX_NATIVE_TERMINAL_BYTES,
        disabled: false,
      },
    )
    .expect("ledger at exact bound");

    let staging = tempfile::tempdir().expect("second prepared staging");
    for entry in &second_manifest.entries {
      let source = second_output.path().join(&entry.path);
      let destination = staging.path().join(&entry.path);
      match entry.kind {
        OutputEntryKind::Directory { .. } => fs::create_dir(&destination).expect("prepared directory"),
        OutputEntryKind::File { .. } => {
          fs::copy(source, destination).expect("prepared file");
        }
        OutputEntryKind::Symlink { .. } => panic!("native fixtures have no symlinks"),
      }
    }
    let error = cas
      .store_native(PreparedNativeResult::from_verified_staging(
        staging,
        second_manifest,
        second_validation,
      ))
      .expect_err("terminal ledger must refuse another conflict marker");
    assert!(error.to_string().contains("ledger is full"), "{error}");
    assert!(validate_native_ledger(cas.root()).expect("ledger").disabled);
    let NativeActionLookup::Miss(miss) = cas
      .native_action(first_validation.action_key())
      .expect("disabled lookup")
    else {
      panic!("a disabled native authority must not restore an old unique state");
    };
    assert_eq!(miss.reason, "native_authority_ledger_full");
    let action_hex = validated_action_key_hex(first_validation.action_key()).expect("action key");
    let state_path = cas
      .root()
      .join(NATIVE_ACTION_STATE_DIRECTORY)
      .join(format!("{action_hex}.json"));
    let state: NativeActionState = serde_json::from_slice(&fs::read(state_path).expect("unique state")).expect("state");
    assert!(matches!(state.state, NativeActionStateKind::UniqueResult { .. }));
  }

  #[test]
  fn malformed_native_action_state_is_durably_quarantined() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let (manifest, validation) = native_fixture(output.path());
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    store_native_fixture(&cas, output.path(), &manifest, &validation);
    let action_hex = validated_action_key_hex(validation.action_key()).expect("action key");
    let state_path = cas
      .root
      .join(NATIVE_ACTION_STATE_DIRECTORY)
      .join(format!("{action_hex}.json"));
    fs::write(&state_path, b"{").expect("corrupt action state");

    let NativeActionLookup::Miss(miss) = cas.native_action(validation.action_key()).expect("quarantine lookup") else {
      panic!("malformed state must not restore");
    };
    assert_eq!(miss.reason, "action_quarantined");
    let bytes = fs::read(&state_path).expect("quarantine marker");
    let state = decode_native_action_state(&bytes, validation.action_key()).expect("valid quarantine marker");
    assert!(matches!(state.state, NativeActionStateKind::Quarantined { .. }));

    drop(cas);
    let reopened = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should reopen");
    reopened.garbage_collect(0, None).expect("GC must preserve quarantine");
    let NativeActionLookup::Miss(miss) = reopened
      .native_action(validation.action_key())
      .expect("restarted quarantine lookup")
    else {
      panic!("restart and GC must not clear quarantine");
    };
    assert_eq!(miss.reason, "action_quarantined");
    assert!(state_path.is_file());
  }

  #[test]
  fn concurrent_native_publications_converge_on_one_binding() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let (manifest, validation) = native_fixture(output.path());
    let manifest = Arc::new(manifest);
    let validation = Arc::new(validation);
    LocalCas::open_at(cache.path(), 1024 * 1024).expect("initialize CAS");
    let barrier = Arc::new(Barrier::new(2));
    let cache_path = cache.path();
    let output_path = output.path();
    std::thread::scope(|scope| {
      let mut handles = Vec::new();
      for _ in 0..2 {
        let manifest = Arc::clone(&manifest);
        let validation = Arc::clone(&validation);
        let barrier = Arc::clone(&barrier);
        handles.push(scope.spawn(move || {
          let cas = LocalCas::open_at(cache_path, 1024 * 1024).expect("writer CAS");
          barrier.wait();
          store_native_fixture(&cas, output_path, &manifest, &validation);
        }));
      }
      for handle in handles {
        handle.join().expect("writer should converge");
      }
    });
    let root = cache.path().join("cargo-rail").join(CAS_ROOT_NAME);
    assert_eq!(fs::read_dir(root.join("pins")).expect("pins").count(), 0);
    assert_eq!(
      fs::read_dir(root.join(NATIVE_ACTION_STATE_DIRECTORY))
        .expect("native actions")
        .count(),
      1
    );
    assert_eq!(fs::read_dir(root.join("results")).expect("results").count(), 1);
  }

  #[test]
  fn verified_bundle_round_trips_exact_bytes_and_modes() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let restore_parent = tempfile::tempdir().expect("restore parent");
    let manifest = write_fixture(output.path(), b"exact compiler bytes");
    #[cfg(unix)]
    let manifest = {
      use std::os::unix::fs::PermissionsExt as _;
      let mut manifest = manifest;
      fs::set_permissions(
        output.path().join("target/deps/artifact.rmeta"),
        fs::Permissions::from_mode(0o755),
      )
      .expect("executable output mode");
      let entry = manifest
        .entries
        .iter_mut()
        .find(|entry| entry.path.ends_with("artifact.rmeta"))
        .expect("file manifest entry");
      let OutputEntryKind::File { mode, .. } = &mut entry.kind else {
        panic!("artifact should be a file");
      };
      *mode = 0o755;
      manifest.digest = super::super::output_manifest_digest(&manifest.entries).expect("updated manifest identity");
      manifest
    };
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let key = action_key(1);
    let stored = store_fixture(&cas, output.path(), &key, &manifest);
    assert!(stored.objects_written >= 5);
    let destination = restore_parent.path().join("clean-output");
    let CacheLookup::Hit(hit) = cas.restore(&key, &destination) else {
      panic!("verified result should hit");
    };
    assert_eq!(hit.bytes_restored, b"exact compiler bytes".len() as u64);
    assert_eq!(
      fs::read(destination.join("target/deps/artifact.rmeta")).expect("restored bytes"),
      b"exact compiler bytes"
    );
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;
      assert_eq!(
        fs::metadata(destination.join("target/deps/artifact.rmeta"))
          .expect("restored metadata")
          .permissions()
          .mode()
          & 0o777,
        0o755
      );
    }
    hit
      .output_manifest
      .validate_unchanged(&destination)
      .expect("restored manifest must revalidate");
  }

  #[cfg(any(unix, windows))]
  #[test]
  fn verified_bundle_round_trips_bounded_symlinks() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let restore_parent = tempfile::tempdir().expect("restore parent");
    fs::create_dir(output.path().join("target")).expect("target directory");
    fs::write(output.path().join("target/real"), b"bytes").expect("real output");
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;

      fs::set_permissions(output.path().join("target"), fs::Permissions::from_mode(0o755)).expect("target mode");
      fs::set_permissions(output.path().join("target/real"), fs::Permissions::from_mode(0o644)).expect("file mode");
      std::os::unix::fs::symlink("real", output.path().join("target/link")).expect("bounded output symlink");
    }
    #[cfg(windows)]
    std::os::windows::fs::symlink_file("real", output.path().join("target/link"))
      .expect("Windows test host must permit file symlinks");
    let manifest = manifest(vec![
      OutputEntry {
        path: "target".to_string(),
        kind: OutputEntryKind::Directory { mode: 0o755 },
      },
      OutputEntry {
        path: "target/link".to_string(),
        kind: OutputEntryKind::Symlink {
          target: "real".to_string(),
        },
      },
      OutputEntry {
        path: "target/real".to_string(),
        kind: OutputEntryKind::File {
          digest: format!("sha256:{}", ContentDigest::sha256(b"bytes")),
          mode: 0o644,
          bytes: 5,
        },
      },
    ]);
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let key = action_key(9);
    store_fixture(&cas, output.path(), &key, &manifest);
    let destination = restore_parent.path().join("clean-output");
    let CacheLookup::Hit(_) = cas.restore(&key, &destination) else {
      panic!("symlink result should hit");
    };
    assert_eq!(
      fs::read_link(destination.join("target/link")).expect("restored link"),
      Path::new("real")
    );
    assert_eq!(
      fs::read(destination.join("target/link")).expect("linked bytes"),
      b"bytes"
    );
  }

  #[cfg(windows)]
  #[test]
  fn windows_junctions_never_gain_cache_authority() {
    let root = tempfile::tempdir().expect("junction root");
    let target = root.path().join("target");
    let removal_root = root.path().join("removal-root");
    let junction = removal_root.join("junction");
    fs::create_dir(&target).expect("junction target");
    fs::create_dir(&removal_root).expect("removal root");
    let output = std::process::Command::new("cmd.exe")
      .args(["/D", "/C", "mklink", "/J"])
      .arg(&junction)
      .arg(&target)
      .output()
      .expect("create junction");
    assert!(
      output.status.success(),
      "mklink failed: {}",
      String::from_utf8_lossy(&output.stderr)
    );

    let metadata = fs::symlink_metadata(&junction).expect("junction metadata");
    assert!(is_link_or_reparse(&metadata));
    assert!(validate_real_directory(&junction, "junction").is_err());
    safe_remove_tree(&removal_root).expect("remove tree without traversing junction");
    assert!(target.is_dir());
    assert!(!removal_root.exists());
  }

  #[test]
  fn same_size_blob_tampering_is_a_corrupt_miss() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let restore_parent = tempfile::tempdir().expect("restore parent");
    let manifest = write_fixture(output.path(), b"alpha");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let key = action_key(2);
    let stored = store_fixture(&cas, output.path(), &key, &manifest);
    let result = stored.action_result.expect("action result identity");
    let bundle = result_path(cas.root(), &result).expect("bundle path");
    let blob = fs::read_dir(bundle.join("blobs"))
      .expect("blob directory")
      .next()
      .expect("one blob")
      .expect("blob entry")
      .path();
    fs::write(blob, b"omega").expect("same-size tamper");
    let CacheLookup::Miss(miss) = cas.restore(&key, &restore_parent.path().join("restore")) else {
      panic!("tampered blob must not hit");
    };
    assert_eq!(miss.kind, CacheMissKind::Corrupt);
    assert!(miss.reason.contains("blob_digest_mismatch"), "{}", miss.reason);
  }

  #[cfg(unix)]
  #[test]
  fn hard_linked_outputs_are_not_published() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let outside = tempfile::tempdir().expect("outside root");
    let manifest = write_fixture(output.path(), b"linked");
    fs::hard_link(
      output.path().join("target/deps/artifact.rmeta"),
      outside.path().join("alias"),
    )
    .expect("hard link fixture");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let key = action_key(10);
    let result = super::super::hermetic_result_digest(&key, manifest.digest());
    let lookup = super::super::test_pre_context_lookup_key();
    let validation = super::super::FastCacheValidation::fixture(&key, &lookup);
    let error = cas
      .store(StoreRequest {
        action_key: &key,
        lookup_key: &lookup,
        result_digest: &result,
        manifest: &manifest,
        validation: &validation,
        compiler_units: 1,
        source_root: output.path(),
      })
      .expect_err("hard-linked output must not enter the CAS");
    assert!(
      error.to_string().contains("changed before local CAS publication"),
      "{error}"
    );
    assert_eq!(
      fs::read(outside.path().join("alias")).expect("outside alias"),
      b"linked"
    );
  }

  #[cfg(windows)]
  #[test]
  fn hard_links_cannot_change_cache_authority() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let outside = tempfile::tempdir().expect("outside root");
    let restore_parent = tempfile::tempdir().expect("restore parent");
    let manifest = write_fixture(output.path(), b"linked");
    let source = output.path().join("target/deps/artifact.rmeta");
    let source_alias = outside.path().join("source-alias");
    fs::hard_link(&source, &source_alias).expect("source hard link fixture");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let key = action_key(10);
    let stored = store_fixture(&cas, output.path(), &key, &manifest);

    fs::write(&source_alias, b"mutate").expect("mutate source through hard link");
    let first_destination = restore_parent.path().join("source-alias-restore");
    let CacheLookup::Hit(_) = cas.restore(&key, &first_destination) else {
      panic!("source aliases must not affect the private CAS copy");
    };
    assert_eq!(
      fs::read(first_destination.join("target/deps/artifact.rmeta")).expect("restored artifact"),
      b"linked"
    );

    let result = stored.action_result.expect("action result identity");
    let bundle = result_path(cas.root(), &result).expect("bundle path");
    let blob = fs::read_dir(bundle.join("blobs"))
      .expect("blob directory")
      .next()
      .expect("one blob")
      .expect("blob entry")
      .path();
    let blob_alias = outside.path().join("blob-alias");
    fs::hard_link(&blob, &blob_alias).expect("CAS blob hard link fixture");
    fs::write(&blob_alias, b"mutate").expect("mutate CAS blob through hard link");
    let CacheLookup::Miss(miss) = cas.restore(&key, &restore_parent.path().join("blob-alias-restore")) else {
      panic!("a hard-linked blob mutation must never authorize reuse");
    };
    assert_eq!(miss.kind, CacheMissKind::Corrupt);
    assert_eq!(miss.reason, "blob_digest_mismatch");
  }

  #[test]
  fn missing_and_incompatible_objects_fail_closed() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let restore_parent = tempfile::tempdir().expect("restore parent");
    let manifest = write_fixture(output.path(), b"payload");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let key = action_key(3);
    let stored = store_fixture(&cas, output.path(), &key, &manifest);
    let result = stored.action_result.expect("action result identity");
    let bundle = result_path(cas.root(), &result).expect("bundle path");
    let blob = fs::read_dir(bundle.join("blobs"))
      .expect("blob directory")
      .next()
      .expect("one blob")
      .expect("blob entry")
      .path();
    fs::remove_file(blob).expect("blob removal");
    let CacheLookup::Miss(missing) = cas.restore(&key, &restore_parent.path().join("missing")) else {
      panic!("missing blob must not hit");
    };
    assert_eq!(missing.kind, CacheMissKind::Corrupt);

    let key = action_key(4);
    store_fixture(&cas, output.path(), &key, &manifest);
    let pin_path = cas.root().join("pins").join(format!(
      "{}.json",
      validated_id_hex(&key, ACTION_KEY_PREFIX).expect("key hex")
    ));
    let mut pin: ActionPin = serde_json::from_slice(&fs::read(&pin_path).expect("pin bytes")).expect("pin JSON");
    pin.version += 1;
    fs::write(&pin_path, canonical_json(&pin).expect("canonical pin")).expect("incompatible pin");
    let CacheLookup::Miss(incompatible) = cas.restore(&key, &restore_parent.path().join("incompatible")) else {
      panic!("incompatible pin must not hit");
    };
    assert_eq!(incompatible.kind, CacheMissKind::Incompatible);
  }

  #[test]
  fn truncated_and_malicious_metadata_objects_fail_closed() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let restore_parent = tempfile::tempdir().expect("restore parent");
    let manifest = write_fixture(output.path(), b"payload");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");

    let truncated_key = action_key(11);
    let truncated = store_fixture(&cas, output.path(), &truncated_key, &manifest)
      .action_result
      .expect("action result identity");
    let truncated_bundle = result_path(cas.root(), &truncated).expect("bundle path");
    fs::write(truncated_bundle.join("action-result.json"), b"{").expect("truncate metadata object");
    let CacheLookup::Miss(truncated) = cas.restore(&truncated_key, &restore_parent.path().join("truncated")) else {
      panic!("truncated metadata must not hit");
    };
    assert_eq!(truncated.kind, CacheMissKind::Corrupt);
    assert!(truncated.reason.contains("object_decode"), "{}", truncated.reason);

    let malicious_key = action_key(12);
    let malicious_result = store_fixture(&cas, output.path(), &malicious_key, &manifest)
      .action_result
      .expect("action result identity");
    let old_bundle = result_path(cas.root(), &malicious_result).expect("old bundle path");
    let mut object: ActionResultObject =
      serde_json::from_slice(&fs::read(old_bundle.join("action-result.json")).expect("result object"))
        .expect("result JSON");
    let malicious_tree = TreeObject {
      version: CAS_VERSION,
      entries: vec![TreeEntry {
        name: "..".to_string(),
        kind: TreeEntryKind::Symlink {
          target: "../../outside".to_string(),
          directory: false,
        },
      }],
    };
    let malicious_tree_id = tree_id(&malicious_tree).expect("malicious tree identity");
    let malicious_tree_hex = validated_id_hex(&malicious_tree_id, TREE_PREFIX).expect("tree hex");
    fs::write(
      old_bundle.join("trees").join(format!("{malicious_tree_hex}.json")),
      canonical_json(&malicious_tree).expect("canonical tree"),
    )
    .expect("malicious tree object");
    object.output_tree = Some(malicious_tree_id);
    let forged_result = action_result_id(&object).expect("forged result identity");
    fs::write(
      old_bundle.join("action-result.json"),
      canonical_json(&object).expect("canonical action result"),
    )
    .expect("forged action result");
    let forged_bundle = result_path(cas.root(), &forged_result).expect("forged bundle path");
    fs::rename(&old_bundle, &forged_bundle).expect("rename forged bundle");
    let pin_path = cas.root().join("pins").join(format!(
      "{}.json",
      validated_id_hex(&malicious_key, ACTION_KEY_PREFIX).expect("key hex")
    ));
    let mut pin: ActionPin = serde_json::from_slice(&fs::read(&pin_path).expect("pin")).expect("pin JSON");
    pin.action_result = forged_result;
    fs::write(&pin_path, canonical_json(&pin).expect("canonical pin")).expect("forged pin");
    let CacheLookup::Miss(malicious) = cas.restore(&malicious_key, &restore_parent.path().join("malicious")) else {
      panic!("malicious tree must not hit");
    };
    assert_eq!(malicious.kind, CacheMissKind::Corrupt);
    assert_eq!(malicious.reason, "unsafe_output_name");
    assert!(!restore_parent.path().join("outside").exists());
  }

  #[test]
  fn unsafe_paths_collisions_and_symlinks_never_enter_a_tree() {
    let file = |path: &str| OutputEntry {
      path: path.to_string(),
      kind: OutputEntryKind::File {
        digest: format!("sha256:{}", ContentDigest::sha256(b"x")),
        mode: 0o644,
        bytes: 1,
      },
    };
    let traversal = manifest(vec![file("target/../outside")]);
    assert_eq!(
      validate_manifest(&traversal)
        .expect_err("parent traversal must fail")
        .reason,
      "unsafe_output_path"
    );
    let collision = manifest(vec![
      OutputEntry {
        path: "target".to_string(),
        kind: OutputEntryKind::Directory { mode: 0o755 },
      },
      file("target/A"),
      file("target/a"),
    ]);
    let root = tempfile::tempdir().expect("output root");
    fs::create_dir(root.path().join("target")).expect("target directory");
    fs::write(root.path().join("target/A"), b"x").expect("A");
    fs::write(root.path().join("target/a"), b"x").expect("a");
    validate_manifest(&collision).expect("flat manifest is structurally valid");
    assert_eq!(
      prepare_tree(&collision, root.path())
        .expect_err("portable name collision must fail")
        .reason,
      "platform_colliding_output_names"
    );
    let escaping_link = manifest(vec![OutputEntry {
      path: "target/link".to_string(),
      kind: OutputEntryKind::Symlink {
        target: "../../outside".to_string(),
      },
    }]);
    assert_eq!(
      validate_manifest(&escaping_link)
        .expect_err("escaping link must fail")
        .reason,
      "symlink_target_escape"
    );
  }

  #[test]
  fn hostile_prepositioned_materialization_root_is_preserved() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let restore_parent = tempfile::tempdir().expect("restore parent");
    let manifest = write_fixture(output.path(), b"payload");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let key = action_key(5);
    store_fixture(&cas, output.path(), &key, &manifest);
    let destination = restore_parent.path().join("occupied");
    fs::create_dir(&destination).expect("occupied destination");
    fs::write(destination.join("keep"), b"keep").expect("sentinel");
    let CacheLookup::Miss(miss) = cas.restore(&key, &destination) else {
      panic!("pre-positioned destination must not hit");
    };
    assert_eq!(miss.kind, CacheMissKind::Corrupt);
    assert_eq!(fs::read(destination.join("keep")).expect("sentinel"), b"keep");
  }

  #[test]
  fn concurrent_writers_converge_on_one_complete_bundle() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let manifest = Arc::new(write_fixture(output.path(), b"concurrent"));
    let key = action_key(6);
    LocalCas::open_at(cache.path(), 1024 * 1024).expect("initialize CAS");
    let barrier = Arc::new(Barrier::new(2));
    let cache_path = cache.path();
    let output_path = output.path();
    std::thread::scope(|scope| {
      let mut handles = Vec::new();
      for _ in 0..2 {
        let manifest = Arc::clone(&manifest);
        let barrier = Arc::clone(&barrier);
        let key = key.clone();
        handles.push(scope.spawn(move || {
          let cas = LocalCas::open_at(cache_path, 1024 * 1024).expect("writer CAS");
          barrier.wait();
          store_fixture(&cas, output_path, &key, &manifest);
        }));
      }
      for handle in handles {
        handle.join().expect("writer should converge");
      }
    });
    assert_eq!(
      fs::read_dir(cache.path().join("cargo-rail").join(CAS_ROOT_NAME).join("pins"))
        .unwrap()
        .count(),
      1
    );
    assert_eq!(
      fs::read_dir(cache.path().join("cargo-rail").join(CAS_ROOT_NAME).join("results"))
        .unwrap()
        .count(),
      1
    );
  }

  #[test]
  fn concurrent_reader_sees_only_a_miss_or_one_complete_publication() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let restore_parent = tempfile::tempdir().expect("restore parent");
    let manifest = Arc::new(write_fixture(output.path(), b"concurrent"));
    let key = action_key(15);
    let cas = Arc::new(LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open"));
    let barrier = Arc::new(Barrier::new(2));
    let published = Arc::new(std::sync::atomic::AtomicBool::new(false));
    std::thread::scope(|scope| {
      let writer_cas = Arc::clone(&cas);
      let writer_manifest = Arc::clone(&manifest);
      let writer_barrier = Arc::clone(&barrier);
      let writer_published = Arc::clone(&published);
      let writer_key = key.clone();
      let writer = scope.spawn(move || {
        writer_barrier.wait();
        store_fixture(&writer_cas, output.path(), &writer_key, &writer_manifest);
        writer_published.store(true, std::sync::atomic::Ordering::Release);
      });

      let reader_cas = Arc::clone(&cas);
      let reader_barrier = Arc::clone(&barrier);
      let reader_published = Arc::clone(&published);
      let reader_key = key.clone();
      let reader = scope.spawn(move || {
        reader_barrier.wait();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        for attempt in 0_u64.. {
          let destination = restore_parent.path().join(format!("restore-{attempt}"));
          match reader_cas.restore(&reader_key, &destination) {
            CacheLookup::Miss(miss) if miss.kind == CacheMissKind::Miss && miss.reason == "action_not_found" => {
              assert!(
                !reader_published.load(std::sync::atomic::Ordering::Acquire),
                "an action pin remained invisible after publication completed"
              );
              assert!(
                std::time::Instant::now() < deadline,
                "the writer did not complete in 10 seconds"
              );
              std::thread::yield_now();
            }
            CacheLookup::Miss(miss) => {
              panic!(
                "a concurrent reader observed a partial publication: {:?} ({})",
                miss.kind, miss.reason
              );
            }
            CacheLookup::Hit(_) => {
              assert_eq!(
                fs::read(destination.join("target/deps/artifact.rmeta")).expect("restored artifact"),
                b"concurrent"
              );
              return;
            }
          }
        }
      });

      writer.join().expect("writer should publish");
      reader.join().expect("reader should observe the publication");
    });
  }

  #[test]
  fn capacity_refusal_publishes_no_authoritative_state() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let manifest = write_fixture(output.path(), b"payload");
    let initialized = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should initialize");
    let cas = LocalCas {
      root: initialized.root.clone(),
      lifecycle_lock: initialized.lifecycle_lock.clone(),
      max_bytes: 1,
    };
    let key = action_key(16);
    let lookup = super::super::test_pre_context_lookup_key();
    let result = super::super::hermetic_result_digest(&key, manifest.digest());
    let validation = super::super::FastCacheValidation::fixture(&key, &lookup);
    let error = cas
      .store(StoreRequest {
        action_key: &key,
        lookup_key: &lookup,
        result_digest: &result,
        manifest: &manifest,
        validation: &validation,
        compiler_units: 1,
        source_root: output.path(),
      })
      .expect_err("an exhausted cache must refuse publication");
    assert!(error.to_string().contains("above the local CAS limit"), "{error}");
    assert_eq!(fs::read_dir(cas.root.join("pins")).expect("pins").count(), 0);
    assert_eq!(fs::read_dir(cas.root.join("results")).expect("results").count(), 0);
    assert_eq!(fs::read_dir(cas.root.join("staging")).expect("staging").count(), 0);
    assert_eq!(
      fs::read(output.path().join("target/deps/artifact.rmeta")).unwrap(),
      b"payload"
    );
  }

  #[test]
  fn interrupted_staging_state_never_authorizes_reuse() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let restore_parent = tempfile::tempdir().expect("restore parent");
    let manifest = write_fixture(output.path(), b"complete");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let orphan = cas.root().join("staging/result-interrupted/payload");
    fs::create_dir_all(orphan.join("blobs")).expect("orphaned staging directories");
    fs::write(orphan.join("action-result.json"), b"{").expect("partial staged object");

    let absent_key = action_key(13);
    let CacheLookup::Miss(absent) = cas.restore(&absent_key, &restore_parent.path().join("absent")) else {
      panic!("unpublished staging state must not hit");
    };
    assert_eq!(absent.kind, CacheMissKind::Miss);
    assert_eq!(absent.reason, "action_not_found");

    let published_key = action_key(14);
    store_fixture(&cas, output.path(), &published_key, &manifest);
    let CacheLookup::Hit(_) = cas.restore(&published_key, &restore_parent.path().join("published")) else {
      panic!("an unrelated complete publication must remain reusable");
    };
    assert!(orphan.exists(), "only atomic pin publication may authorize a result");
  }

  #[test]
  fn process_death_during_publication_leaves_no_authoritative_state() {
    const CACHE_ENV: &str = "CARGO_RAIL_TEST_CAS_PROCESS_DEATH_CACHE";
    const OUTPUT_ENV: &str = "CARGO_RAIL_TEST_CAS_PROCESS_DEATH_OUTPUT";
    const PAUSE_ENV: &str = "CARGO_RAIL_TEST_CAS_PAUSE_AFTER_FIRST_OBJECT";

    let root = tempfile::tempdir().expect("process-death root");
    let cache = root.path().join("cache");
    let output = root.path().join("output");
    let control = root.path().join("control");
    fs::create_dir(&cache).expect("cache base");
    fs::create_dir(&output).expect("output base");
    let mut child = std::process::Command::new(std::env::current_exe().expect("current test executable"))
      .args([
        "--exact",
        "hermetic::cas::tests::process_death_publication_worker",
        "--nocapture",
      ])
      .env(CACHE_ENV, &cache)
      .env(OUTPUT_ENV, &output)
      .env(PAUSE_ENV, &control)
      .spawn()
      .expect("publication worker should start");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !control.join("ready").is_file() {
      assert!(
        child.try_wait().expect("worker status").is_none(),
        "publication worker exited before reaching the partial-write boundary"
      );
      assert!(
        std::time::Instant::now() < deadline,
        "publication worker did not reach the partial-write boundary"
      );
      std::thread::sleep(std::time::Duration::from_millis(10));
    }
    child.kill().expect("publication worker should be terminated");
    let status = child.wait().expect("publication worker status");
    assert!(
      !status.success(),
      "a terminated publication worker must not report success"
    );

    let cas = LocalCas::open_at(&cache, 1024 * 1024).expect("CAS should reopen after process death");
    let restore_parent = root.path().join("restore");
    fs::create_dir(&restore_parent).expect("restore parent");
    let CacheLookup::Miss(miss) = cas.restore(&action_key(17), &restore_parent.join("result")) else {
      panic!("partial staging state must not authorize reuse");
    };
    assert_eq!(miss.kind, CacheMissKind::Miss);
    assert_eq!(miss.reason, "action_not_found");
    assert_eq!(fs::read_dir(cas.root.join("pins")).expect("pins").count(), 0);
    assert_eq!(fs::read_dir(cas.root.join("results")).expect("results").count(), 0);
    assert_eq!(
      fs::read_dir(cas.root.join("staging")).expect("staging").count(),
      0,
      "the next coordinated open must reclaim partial publication state"
    );
    remove_owned_root_at(&cas.root).expect("owned cleanup should remove partial staging");
    assert!(!cas.root.exists());
    assert!(cache.exists(), "cleanup must preserve the configured cache base");
  }

  #[test]
  fn process_death_publication_worker() {
    const CACHE_ENV: &str = "CARGO_RAIL_TEST_CAS_PROCESS_DEATH_CACHE";
    const OUTPUT_ENV: &str = "CARGO_RAIL_TEST_CAS_PROCESS_DEATH_OUTPUT";
    let (Some(cache), Some(output)) = (std::env::var_os(CACHE_ENV), std::env::var_os(OUTPUT_ENV)) else {
      return;
    };
    let cache = PathBuf::from(cache);
    let output = PathBuf::from(output);
    let manifest = write_fixture(&output, b"partial publication");
    let cas = LocalCas::open_at(&cache, 1024 * 1024).expect("worker CAS should open");
    store_fixture(&cas, &output, &action_key(17), &manifest);
  }

  #[test]
  fn out_of_space_during_publication_preserves_prior_authority() {
    const CACHE_ENV: &str = "CARGO_RAIL_TEST_CAS_ENOSPC_CACHE";
    const OUTPUT_ENV: &str = "CARGO_RAIL_TEST_CAS_ENOSPC_OUTPUT";
    const FAIL_ENV: &str = "CARGO_RAIL_TEST_CAS_FAIL_AFTER_FIRST_OBJECT";

    let root = tempfile::tempdir().expect("out-of-space root");
    let cache = root.path().join("cache");
    let prior_output = root.path().join("prior-output");
    let failed_output = root.path().join("failed-output");
    fs::create_dir(&cache).expect("cache base");
    fs::create_dir(&prior_output).expect("prior output");
    fs::create_dir(&failed_output).expect("failed output");
    let prior_manifest = write_fixture(&prior_output, b"prior valid result");
    let cas = LocalCas::open_at(&cache, 1024 * 1024).expect("CAS should open");
    store_fixture(&cas, &prior_output, &action_key(18), &prior_manifest);

    let child = std::process::Command::new(std::env::current_exe().expect("current test executable"))
      .args([
        "--exact",
        "hermetic::cas::tests::out_of_space_publication_worker",
        "--nocapture",
      ])
      .env(CACHE_ENV, &cache)
      .env(OUTPUT_ENV, &failed_output)
      .env(FAIL_ENV, "1")
      .output()
      .expect("out-of-space worker should run");
    assert!(
      child.status.success(),
      "out-of-space worker failed:\nstdout={}\nstderr={}",
      String::from_utf8_lossy(&child.stdout),
      String::from_utf8_lossy(&child.stderr)
    );

    let reopened = LocalCas::open_at(&cache, 1024 * 1024).expect("CAS should reopen");
    let restore = root.path().join("restore");
    let CacheLookup::Hit(_) = reopened.restore(&action_key(18), &restore) else {
      panic!("the prior result must survive an out-of-space publication failure");
    };
    assert_eq!(
      fs::read(restore.join("target/deps/artifact.rmeta")).unwrap(),
      b"prior valid result"
    );
    let CacheLookup::Miss(miss) = reopened.restore(&action_key(19), &root.path().join("failed-restore")) else {
      panic!("the failed publication must not gain authority");
    };
    assert_eq!(miss.reason, "action_not_found");
    assert_eq!(fs::read_dir(reopened.root().join("pins")).unwrap().count(), 1);
    assert_eq!(fs::read_dir(reopened.root().join("results")).unwrap().count(), 1);
    assert_eq!(fs::read_dir(reopened.root().join("staging")).unwrap().count(), 0);
  }

  #[test]
  fn out_of_space_publication_worker() {
    const CACHE_ENV: &str = "CARGO_RAIL_TEST_CAS_ENOSPC_CACHE";
    const OUTPUT_ENV: &str = "CARGO_RAIL_TEST_CAS_ENOSPC_OUTPUT";
    let (Some(cache), Some(output)) = (std::env::var_os(CACHE_ENV), std::env::var_os(OUTPUT_ENV)) else {
      return;
    };
    let output = PathBuf::from(output);
    let manifest = write_fixture(&output, b"unpublished result");
    let cas = LocalCas::open_at(Path::new(&cache), 1024 * 1024).expect("worker CAS should open");
    let key = action_key(19);
    let lookup = super::super::test_pre_context_lookup_key();
    let result = super::super::hermetic_result_digest(&key, manifest.digest());
    let validation = super::super::FastCacheValidation::fixture(&key, &lookup);
    let error = cas
      .store(StoreRequest {
        action_key: &key,
        lookup_key: &lookup,
        result_digest: &result,
        manifest: &manifest,
        validation: &validation,
        compiler_units: 1,
        source_root: &output,
      })
      .expect_err("the publication failpoint must surface an out-of-space error");
    assert!(error.to_string().to_ascii_lowercase().contains("space"), "{error}");
  }

  #[test]
  fn aggregate_object_limits_fail_before_authorization() {
    let bundle = tempfile::tempdir().expect("bundle root");
    fs::create_dir(bundle.path().join("trees")).expect("tree object directory");
    let tree = TreeObject {
      version: CAS_VERSION,
      entries: vec![TreeEntry {
        name: "target".to_string(),
        kind: TreeEntryKind::Symlink {
          target: "inside".to_string(),
          directory: false,
        },
      }],
    };
    let identity = tree_id(&tree).expect("tree identity");
    let hex = validated_id_hex(&identity, TREE_PREFIX).expect("tree hex");
    fs::write(
      bundle.path().join("trees").join(format!("{hex}.json")),
      canonical_json(&tree).expect("canonical tree"),
    )
    .expect("tree object");
    let mut total_entries = MAX_ENTRIES;
    let mut loading = BTreeSet::new();
    let mut loaded = BTreeMap::new();
    let mut stats = ReadStats::default();
    let error = load_tree_recursive(
      bundle.path(),
      &identity,
      0,
      &mut total_entries,
      &mut loading,
      &mut loaded,
      &mut stats,
    )
    .expect_err("aggregate entry limit must fail");
    assert_eq!(error.reason, "tree_entry_limit");

    let object = bundle.path().join("bounded.json");
    fs::write(&object, b"{}").expect("bounded object");
    let mut exhausted = ReadStats {
      bytes: MAX_LOOKUP_BYTES,
      ..ReadStats::default()
    };
    let error = read_bounded_file(&object, MAX_OBJECT_METADATA_BYTES, &mut exhausted)
      .expect_err("aggregate byte limit must fail");
    assert_eq!(error.reason, "result_byte_limit");
  }

  #[cfg(unix)]
  #[test]
  fn owned_cleanup_unlinks_nested_symlinks_without_following_them() {
    use std::os::unix::fs::symlink;

    let cache = tempfile::tempdir().expect("cache base");
    let outside = tempfile::tempdir().expect("outside root");
    fs::write(outside.path().join("keep"), b"outside").expect("outside sentinel");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    symlink(outside.path(), cas.root().join("staging/hostile-link")).expect("hostile nested link");
    let removed = remove_owned_root_at(cas.root()).expect("owned cleanup should succeed");
    assert!(removed.is_some());
    assert_eq!(
      fs::read(outside.path().join("keep")).expect("outside sentinel"),
      b"outside"
    );
  }

  #[test]
  fn cache_open_refuses_to_adopt_a_nonempty_unowned_root() {
    let cache = tempfile::tempdir().expect("cache base");
    let root = cache.path().join("cargo-rail").join(CAS_ROOT_NAME);
    fs::create_dir_all(&root).expect("hostile root");
    let sentinel = root.join("user-data");
    fs::write(&sentinel, b"preserve me").expect("hostile sentinel");

    let error = LocalCas::open_at(cache.path(), 1024 * 1024).expect_err("unowned root must not be adopted");
    assert!(error.to_string().contains("nonempty"), "{error}");
    assert!(!root.join("OWNER").exists());
    assert_eq!(fs::read(&sentinel).expect("sentinel must survive"), b"preserve me");
    assert!(remove_owned_root_at(&root).is_err());
    assert_eq!(
      fs::read(&sentinel).expect("cleanup refusal must preserve sentinel"),
      b"preserve me"
    );
  }

  #[test]
  fn cleanup_refuses_an_invalid_owner_marker() {
    let cache = tempfile::tempdir().expect("cache base");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    fs::write(cas.root().join("OWNER"), b"forged\n").expect("tamper owner marker");
    let error = remove_owned_root_at(cas.root()).expect_err("invalid marker must block recursive cleanup");
    assert!(error.to_string().contains("authority marker"), "{error}");
    assert!(cas.root().exists());
  }

  #[test]
  fn deterministic_gc_removes_unpinned_bundles() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let manifest = write_fixture(output.path(), b"payload");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    store_fixture(&cas, output.path(), &action_key(7), &manifest);
    store_fixture(&cas, output.path(), &action_key(8), &manifest);
    cas.garbage_collect(0, None).expect("GC should be deterministic");
    assert_eq!(fs::read_dir(cas.root().join("pins")).unwrap().count(), 0);
    assert_eq!(fs::read_dir(cas.root().join("results")).unwrap().count(), 0);
  }

  #[test]
  fn stale_leases_lose_authority_during_collection() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let manifest = write_fixture(output.path(), b"stale lease");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let key = action_key(33);
    let stored = store_fixture(&cas, output.path(), &key, &manifest);
    let result = stored.action_result.expect("action result");
    let lease = cas.create_lease(&result).expect("lease");
    let lease_path = lease.path.clone();
    std::mem::forget(lease);
    fs::write(
      &lease_path,
      canonical_json(&LeaseRecord {
        version: CAS_VERSION,
        action_result: result.clone(),
        created_unix_seconds: 0,
      })
      .expect("stale lease record"),
    )
    .expect("age lease");
    fs::remove_file(
      cas
        .root()
        .join("pins")
        .join(format!("{}.json", validated_action_key_hex(&key).expect("action key"))),
    )
    .expect("remove authoritative pin");

    cas.garbage_collect(0, None).expect("collect stale lease");

    assert!(!lease_path.exists());
    assert!(!result_path(cas.root(), &result).expect("result path").exists());
  }

  #[test]
  fn native_gc_revokes_unique_action_state_before_removing_its_result() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let (manifest, validation) = native_fixture(output.path());
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    store_native_fixture(&cas, output.path(), &manifest, &validation);
    let action_hex = validated_id_hex(
      validation.action_key(),
      crate::compiler::native_cache::ACTION_KEY_PREFIX,
    )
    .expect("action key");
    let action_state = cas
      .root()
      .join(NATIVE_ACTION_STATE_DIRECTORY)
      .join(format!("{action_hex}.json"));
    assert!(action_state.is_file());

    cas.garbage_collect(0, None).expect("GC should remove native result");
    assert_eq!(fs::read_dir(cas.root().join("pins")).unwrap().count(), 0);
    assert_eq!(fs::read_dir(cas.root().join("results")).unwrap().count(), 0);
    assert!(!action_state.exists());
  }

  #[test]
  fn cleanup_waits_for_an_in_flight_cache_reader() {
    let cache = tempfile::tempdir().expect("cache base");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let root = cas.root.clone();
    let reader = cas.read_lock().expect("shared lifecycle lock");
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();

    std::thread::scope(|scope| {
      scope.spawn(move || {
        let removed = remove_owned_root_at(&root).expect("cleanup should wait, then remove");
        finished_tx.send(removed).expect("cleanup result");
      });
      assert!(
        finished_rx.recv_timeout(std::time::Duration::from_millis(100)).is_err(),
        "cleanup crossed the shared lifecycle boundary"
      );
      drop(reader);
      assert!(
        finished_rx
          .recv_timeout(std::time::Duration::from_secs(10))
          .expect("cleanup should finish")
          .is_some()
      );
    });
    assert!(!cas.root.exists());
  }

  #[test]
  fn cleanup_waits_for_an_in_flight_cache_mutation() {
    let cache = tempfile::tempdir().expect("cache base");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let root = cas.root.clone();
    let mutation = cas.lock().expect("exclusive lifecycle lock");
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();

    std::thread::scope(|scope| {
      scope.spawn(move || {
        let removed = remove_owned_root_at(&root).expect("cleanup should wait, then remove");
        finished_tx.send(removed).expect("cleanup result");
      });
      assert!(
        finished_rx.recv_timeout(std::time::Duration::from_millis(100)).is_err(),
        "cleanup crossed the exclusive lifecycle boundary"
      );
      drop(mutation);
      assert!(
        finished_rx
          .recv_timeout(std::time::Duration::from_secs(10))
          .expect("cleanup should finish")
          .is_some()
      );
    });
    assert!(!cas.root.exists());
  }

  #[test]
  fn status_and_cleanup_accept_an_owned_missing_optional_evidence_index() {
    let cache = tempfile::tempdir().expect("cache base");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    fs::remove_dir(cas.root().join(EVIDENCE_CANDIDATE_INDEX_DIRECTORY)).expect("remove transition directory");

    let status = status_at_with_max(cas.root(), 1024 * 1024)
      .expect("transition status")
      .expect("present transition root");
    assert_eq!(status.index_files, 0);

    let removed = remove_owned_root_at(cas.root()).expect("transition cleanup");
    assert!(removed.is_some());
    assert!(!cas.root().exists());
  }

  #[test]
  fn garbage_collection_evicts_the_least_recently_used_pin() {
    let cache = tempfile::tempdir().expect("cache base");
    let output = tempfile::tempdir().expect("output root");
    let manifest = write_fixture(output.path(), b"payload");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let newer = action_key(21);
    let older = action_key(22);
    store_fixture(&cas, output.path(), &newer, &manifest);
    store_fixture(&cas, output.path(), &older, &manifest);

    let pin = |key: &str| {
      cas
        .root
        .join("pins")
        .join(format!("{}.json", validated_action_key_hex(key).expect("action key")))
    };
    let now = SystemTime::now();
    OpenOptions::new()
      .write(true)
      .open(pin(&older))
      .expect("older pin")
      .set_modified(now + std::time::Duration::from_secs(1))
      .expect("older use time");
    OpenOptions::new()
      .write(true)
      .open(pin(&newer))
      .expect("newer pin")
      .set_modified(now + std::time::Duration::from_secs(2))
      .expect("newer use time");

    let current = validate_capacity_state(cas.root())
      .expect("capacity state")
      .result_bytes;
    cas.garbage_collect(current - 1, None).expect("bounded LRU collection");

    assert!(pin(&newer).exists(), "the most recently used result must survive");
    assert!(!pin(&older).exists(), "the least recently used result must be evicted");
  }

  #[test]
  fn result_capacity_excludes_bounded_control_metadata() {
    let measurement_cache = tempfile::tempdir().expect("measurement cache base");
    let output = tempfile::tempdir().expect("output root");
    let manifest = write_fixture(output.path(), b"bounded payload");
    let measurement = LocalCas::open_at(measurement_cache.path(), 1024 * 1024).expect("measurement CAS");
    let stored = store_fixture(&measurement, output.path(), &action_key(31), &manifest);
    let result = stored.action_result.expect("action result");
    let result_bytes =
      checked_tree_bytes(&result_path(measurement.root(), &result).expect("result path")).expect("result bundle bytes");

    let bounded_cache = tempfile::tempdir().expect("bounded cache base");
    let mut bounded = LocalCas::open_at(bounded_cache.path(), 1024 * 1024).expect("bounded CAS");
    bounded.max_bytes = result_bytes;
    let key = action_key(32);
    let result_digest = super::super::hermetic_result_digest(&key, manifest.digest());
    let lookup = super::super::test_pre_context_lookup_key();
    let validation = super::super::FastCacheValidation::fixture(&key, &lookup);

    let stored = bounded
      .store(StoreRequest {
        action_key: &key,
        lookup_key: &lookup,
        result_digest: &result_digest,
        manifest: &manifest,
        validation: &validation,
        compiler_units: 1,
        source_root: output.path(),
      })
      .expect("the exact result-byte bound must admit its control metadata separately");

    assert_eq!(stored.bytes_written, result_bytes);
    assert_eq!(fs::read_dir(bounded.root().join("pins")).unwrap().count(), 1);
    assert_eq!(fs::read_dir(bounded.root().join("results")).unwrap().count(), 1);
  }

  #[test]
  fn initialized_cache_open_validates_without_reclaiming_shared_staging() {
    let cache = tempfile::tempdir().expect("cache base");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let in_flight = cas.root().join("staging/in-flight");
    fs::write(&in_flight, b"owned by another compiler process").expect("staging sentinel");

    let base = fs::canonicalize(cache.path()).expect("canonical cache base");
    let reopened = LocalCas::open_initialized_at(&base, 1024 * 1024).expect("initialized CAS should open");

    assert_eq!(reopened.root(), cas.root());
    assert_eq!(
      fs::read(in_flight).expect("initialized open must preserve staging"),
      b"owned by another compiler process"
    );
  }

  #[test]
  fn staging_creation_holds_lifecycle_authority_until_its_active_lease_exists() {
    const CACHE_ENV: &str = "CARGO_RAIL_TEST_CAS_STAGING_CACHE";
    const PAUSE_ENV: &str = "CARGO_RAIL_TEST_CAS_PAUSE_BEFORE_ACTIVE";

    let root = tempfile::tempdir().expect("staging race root");
    let cache = root.path().join("cache");
    let control = root.path().join("control");
    fs::create_dir(&cache).expect("cache base");
    let cas = LocalCas::open_at(&cache, 1024 * 1024).expect("CAS should open");
    let mut child = std::process::Command::new(std::env::current_exe().expect("current test executable"))
      .args([
        "--exact",
        "hermetic::cas::tests::guarded_staging_creation_worker",
        "--nocapture",
      ])
      .env(CACHE_ENV, &cache)
      .env(PAUSE_ENV, &control)
      .spawn()
      .expect("staging worker should start");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !control.join("ready").is_file() {
      assert!(
        child.try_wait().expect("worker status").is_none(),
        "staging worker exited before reaching the unleased-directory boundary"
      );
      assert!(
        std::time::Instant::now() < deadline,
        "staging worker did not reach the unleased-directory boundary"
      );
      std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let lifecycle = crate::utils::open_cache_lock_file(&cas.lifecycle_lock, false).expect("lifecycle lock file");
    assert!(
      matches!(lifecycle.try_lock(), Err(fs::TryLockError::WouldBlock)),
      "cleanup authority must remain excluded until the staging lease is locked"
    );
    assert_eq!(
      fs::read_dir(cas.root.join("staging"))
        .expect("staging directory")
        .count(),
      1,
      "the unleased staging directory must remain protected by lifecycle authority"
    );

    write_new_synced(&control.join("continue"), b"continue\n").expect("release staging worker");
    let status = child.wait().expect("staging worker status");
    assert!(status.success(), "staging worker failed: {status}");
  }

  #[test]
  fn guarded_staging_creation_worker() {
    const CACHE_ENV: &str = "CARGO_RAIL_TEST_CAS_STAGING_CACHE";
    let Some(cache) = std::env::var_os(CACHE_ENV) else {
      return;
    };
    let base = fs::canonicalize(cache).expect("canonical cache base");
    let cas = LocalCas::open_initialized_at(&base, 1024 * 1024).expect("initialized CAS should open");
    let _staging = cas.native_result_staging().expect("guarded staging should be created");
  }

  #[cfg(unix)]
  #[test]
  fn cache_open_rejects_a_linked_lifecycle_lock() {
    use std::os::unix::fs::symlink;

    let cache = tempfile::tempdir().expect("cache base");
    let owner = cache.path().join("cargo-rail");
    fs::create_dir(&owner).expect("CAS owner");
    let outside = tempfile::NamedTempFile::new().expect("outside file");
    fs::write(outside.path(), b"preserve").expect("outside contents");
    symlink(outside.path(), owner.join(format!("{CAS_ROOT_NAME}.lock"))).expect("hostile lock link");

    let error = LocalCas::open_at(cache.path(), 1024 * 1024).expect_err("linked lock must fail");

    assert!(error.to_string().contains("not a private regular file"), "{error}");
    assert_eq!(fs::read(outside.path()).expect("outside contents"), b"preserve");
  }

  #[cfg(any(unix, windows))]
  #[test]
  fn cache_open_rejects_a_hard_linked_lifecycle_lock() {
    let cache = tempfile::tempdir().expect("cache base");
    let owner = cache.path().join("cargo-rail");
    fs::create_dir(&owner).expect("CAS owner");
    let outside = tempfile::NamedTempFile::new().expect("outside file");
    fs::hard_link(outside.path(), owner.join(format!("{CAS_ROOT_NAME}.lock"))).expect("hostile hard-linked lock");

    let error = LocalCas::open_at(cache.path(), 1024 * 1024).expect_err("hard-linked lock must fail");

    assert!(error.to_string().contains("not a private regular file"), "{error}");
    assert!(outside.path().exists(), "outside lock target must survive");
  }

  #[cfg(unix)]
  #[test]
  fn cache_open_unlinks_hostile_staging_links_without_following_them() {
    use std::os::unix::fs::symlink;

    let cache = tempfile::tempdir().expect("cache base");
    let outside = tempfile::tempdir().expect("outside root");
    let sentinel = outside.path().join("keep");
    fs::write(&sentinel, b"outside").expect("outside sentinel");
    let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
    let hostile = cas.root().join("staging/hostile-link");
    symlink(outside.path(), &hostile).expect("hostile staging link");

    let reopened = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should reclaim staging");

    assert!(!hostile.exists(), "hostile staging link must be unlinked");
    assert_eq!(fs::read(&sentinel).expect("outside sentinel"), b"outside");
    assert_eq!(fs::read_dir(reopened.root().join("staging")).unwrap().count(), 0);
  }
}
