//! Immutable machine-local storage for verified cache objects.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::cache::result::{
    OUTPUT_MANIFEST_VERSION, OutputEntry, OutputEntryKind, OutputManifest, output_manifest_digest,
    symlink_target_escapes,
};
use crate::compiler::diagnostics_store::{
    CompilerEvidenceObject, CompilerEvidenceValidation, EVIDENCE_ACTION_KEY_PREFIX, EVIDENCE_CANDIDATE_KEY_PREFIX,
    EVIDENCE_OBJECT_PREFIX, validate_evidence_action_key, validate_evidence_candidate_key, validate_evidence_object,
};
use crate::compiler::native_cache::{
    NativeCompilerValidation, NativeDurabilityPhase, PreparedNativeParts, PreparedNativeResult, native_durability_phase,
};
use crate::error::{RailError, RailResult};

const CAS_VERSION: u32 = 2;
const CAS_ROOT_NAME: &str = "local-cas-v2";
const OWNER_MARKER_PREFIX: &str = "cargo-rail-local-cas\nschema=2\ntrust-domain=";
const DEFAULT_TRUST_DOMAIN_FILE: &str = "LOCAL_TRUST_DOMAIN";
pub(crate) const CACHE_BASE_ENV: &str = "CARGO_RAIL_CACHE_DIR";
pub(crate) const CACHE_MAX_BYTES_ENV: &str = "CARGO_RAIL_CACHE_MAX_BYTES";
pub(crate) const CACHE_TRUST_DOMAIN_ENV: &str = "CARGO_RAIL_CACHE_TRUST_DOMAIN";
pub(crate) const DEFAULT_CACHE_MAX_BYTES: u64 = 10 * 1024 * 1024 * 1024;
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
const ACCESS_REFRESH_INTERVAL: Duration = Duration::from_secs(60 * 60);
const EVIDENCE_CANDIDATE_INDEX_VERSION: u32 = 1;
const EVIDENCE_CANDIDATE_INDEX_DIRECTORY: &str = "compiler-evidence-candidates";
const NATIVE_ENVIRONMENT_SELECTOR_DIRECTORY: &str = "native-environment-selectors-v1";
const NATIVE_LINK_CANDIDATE_DIRECTORY: &str = "native-link-candidates-v1";
const NATIVE_LINK_CANDIDATE_VERSION: u32 = 1;
const MAX_NATIVE_LINK_CANDIDATES: usize = 64;
const MAX_NATIVE_LINK_CANDIDATE_BYTES: u64 = 1024;
const MAX_NATIVE_ENVIRONMENT_SELECTOR_BYTES: u64 = 1024 * 1024;
const NATIVE_ENVIRONMENT_SELECTOR_CONFLICT_BYTES: &[u8] =
    b"cargo-rail-native-environment-selector-conflict\nschema=1\n";
const NATIVE_ACTION_STATE_VERSION: u32 = 2;
const NATIVE_ACTION_STATE_DIRECTORY: &str = "native-actions-v2";
const PACKED_NATIVE_ACTION_MAGIC: &[u8; 8] = b"CRNAL1P1";
const PACKED_NATIVE_ACTION_VERSION: u16 = 1;
const PACKED_NATIVE_ACTION_PRELUDE_BYTES: u64 = 8 + 2 + 4;
const PACKED_NATIVE_ACTION_PRELUDE_LEN: usize = 8 + 2 + 4;
const MAX_PACKED_NATIVE_ACTION_HEADER_BYTES: u64 = 1024 * 1024;
const MAX_PACKED_NATIVE_ACTION_BYTES: u64 = crate::compiler::native_cache::pack::MAX_PACK_BYTES + 1024 * 1024;
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
const VALIDATION_PREFIX: &str = "validation-v1-sha256-";

#[cfg(any(unix, windows, test))]
pub(crate) struct NativeCacheHit {
    pub(crate) bytes_read: u64,
    pub(crate) bytes_restored: u64,
}

#[cfg(any(unix, windows, test))]
pub(crate) struct NativeCacheMiss {
    pub(crate) reason: String,
    pub(crate) bytes_read: u64,
}

pub(crate) struct PackedNativeActionStagingRequest<'a> {
    pub(crate) base_action_key: &'a str,
    pub(crate) environment_names: &'a [String],
    pub(crate) action_key: &'a str,
    pub(crate) result_key: &'a str,
    pub(crate) remote_authority: &'a crate::compiler::native_cache::RemoteAuthorityId,
    pub(crate) pack_bytes: u64,
    pub(crate) compressed_bytes: u64,
}

#[cfg(any(unix, windows, test))]
struct MaterializeBlobRequest<'a> {
    bundle: &'a Path,
    identity: &'a str,
    content_digest: &'a str,
    expected_bytes: u64,
    mode: u32,
    destination: &'a Path,
    stats: &'a mut ReadStats,
    durable: bool,
}

#[cfg(any(unix, windows, test))]
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
    Packed(Box<PackedNativeActionHit<'a>>),
    Miss(NativeCacheMiss),
}

/// One compressed, authenticated native result held as its action authority.
pub(crate) struct PackedNativeActionHit<'a> {
    header: PackedNativeActionHeader,
    file: File,
    pub(crate) bytes_read: u64,
    refresh_access: bool,
    cas: &'a LocalCas,
    _lock: LocalCasLifecycleLock,
}

/// One uniquely authoritative native result held under a stable local CAS view.
pub(crate) struct NativeActionHit<'a> {
    pub(crate) validation: NativeCompilerValidation,
    pub(crate) bytes_read: u64,
    refresh_access: bool,
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

pub(crate) struct CompilerEvidenceStoreRequest<'a> {
    pub(crate) validation: &'a CompilerEvidenceValidation,
    pub(crate) evidence: &'a CompilerEvidenceObject,
}

/// One validated local CAS rooted outside any physical checkout.
#[derive(Debug, Clone)]
pub(crate) struct LocalCas {
    root: PathBuf,
    lifecycle_lock: PathBuf,
    max_bytes: u64,
}

/// One explicit machine-selected local CAS authority.
///
/// Transparent compiler wrappers load this value from the private installation
/// receipt instead of accepting repository-controlled cache environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalCacheSelection {
    base: PathBuf,
    max_bytes: u64,
    trust_domain: Option<String>,
}

impl LocalCacheSelection {
    pub(crate) fn new(base: PathBuf, max_bytes: u64, trust_domain: Option<String>) -> RailResult<Self> {
        if !base.is_absolute() || base.as_os_str().is_empty() || base.as_os_str().as_encoded_bytes().contains(&0) {
            return Err(RailError::message("local cache base must be a non-empty absolute path"));
        }
        if max_bytes == 0 {
            return Err(RailError::message("local cache capacity must be positive"));
        }
        if let Some(trust_domain) = trust_domain.as_deref() {
            validate_trust_domain(trust_domain)?;
        }
        Ok(Self {
            base,
            max_bytes,
            trust_domain,
        })
    }

    pub(crate) fn from_environment() -> RailResult<Self> {
        let trust_domain = std::env::var_os(CACHE_TRUST_DOMAIN_ENV)
            .map(|value| {
                value
                    .into_string()
                    .map_err(|_| RailError::message(format!("{CACHE_TRUST_DOMAIN_ENV} is not valid UTF-8")))
            })
            .transpose()?;
        Self::new(cache_base()?, cache_max_bytes()?, trust_domain)
    }

    pub(crate) fn base(&self) -> &Path {
        &self.base
    }

    pub(crate) const fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    pub(crate) fn trust_domain(&self) -> Option<&str> {
        self.trust_domain.as_deref()
    }

    pub(crate) fn configured_root(&self) -> RailResult<Option<PathBuf>> {
        configured_root_for(self)
    }
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

/// One per-destination native restore lock protected from CAS staging cleanup.
pub(crate) struct NativeRestoreLock {
    // Struct fields drop in declaration order: release the destination lock
    // while cleanup is still excluded by the shared lifecycle authority.
    _file: File,
    _lifecycle: LocalCasLifecycleLock,
}

impl NativeActionHit<'_> {
    fn refresh_access_if_stale(&self) {
        if !self.refresh_access {
            return;
        }
        let Ok(key_hex) = validated_action_key_hex(self.validation.action_key()) else {
            return;
        };
        let state_path = self
            .cas
            .root
            .join(NATIVE_ACTION_STATE_DIRECTORY)
            .join(format!("{key_hex}.json"));
        drop(
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(state_path)
                .and_then(|file| file.set_modified(SystemTime::now())),
        );
    }

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

    /// Revalidate the base-action selector before exposing this result remotely.
    pub(crate) fn validate_remote_publication<'a>(&'a self, base_action_key: &str) -> RailResult<&'a [String]> {
        let expected = self.validation.remote_publication_environment_names(base_action_key)?;
        self.validate_environment_selector(base_action_key, expected.iter().map(String::as_str))?;
        Ok(expected)
    }

    pub(crate) fn association(&self) -> RailResult<crate::compiler::native_cache::pack::NativeAssociation> {
        crate::compiler::native_cache::pack::association(&self.validation)
    }

    /// Stream the canonical result pack from immutable CAS blobs while this view retains read authority.
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
        self.refresh_access_if_stale();
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
        self.refresh_access_if_stale();
        NativeCacheLookup::Hit(NativeCacheHit {
            bytes_read: stats.bytes,
            bytes_restored: stats.restored,
        })
    }
}

impl PackedNativeActionHit<'_> {
    pub(crate) fn base_action_key(&self) -> &str {
        &self.header.base_action_key
    }

    pub(crate) fn environment_names(&self) -> &[String] {
        &self.header.environment_names
    }

    pub(crate) fn action_key(&self) -> &str {
        &self.header.action_key
    }

    pub(crate) fn result_key(&self) -> &str {
        &self.header.result_key
    }

    pub(crate) const fn pack_bytes(&self) -> u64 {
        self.header.pack_bytes
    }

    pub(crate) const fn compressed_bytes(&self) -> u64 {
        self.header.compressed_bytes
    }

    pub(crate) fn compressed_reader(&self) -> RailResult<File> {
        let mut file = self.file.try_clone()?;
        file.seek(std::io::SeekFrom::Start(packed_native_action_payload_offset(
            &self.header,
        )?))?;
        Ok(file)
    }

    pub(crate) fn validate_environment_selector(&self) -> RailResult<()> {
        if self
            .cas
            .native_environment_selector_conflicted(&self.header.base_action_key)?
        {
            return Err(RailError::message(
                "local CAS packed native environment selector is durably conflicted",
            ));
        }
        let Some(actual) = self
            .cas
            .load_native_environment_selector(&self.header.base_action_key)?
        else {
            return Err(RailError::message(
                "local CAS packed native environment selector is absent",
            ));
        };
        if actual != self.header.environment_names {
            return Err(RailError::message(
                "local CAS packed native environment selector does not match its authority",
            ));
        }
        Ok(())
    }

    pub(crate) fn refresh_access_if_stale(&self) {
        if self.refresh_access {
            drop(self.file.set_modified(SystemTime::now()));
        }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackedNativeActionHeader {
    version: u32,
    base_action_key: String,
    environment_names: Vec<String>,
    action_key: String,
    result_key: String,
    remote_authority: String,
    pack_bytes: u64,
    compressed_bytes: u64,
}

/// Private same-filesystem staging for one packed native action authority.
pub(crate) struct PackedNativeActionStaging {
    _directory: tempfile::TempDir,
    _active: File,
    path: PathBuf,
    file: File,
    header: PackedNativeActionHeader,
    payload_offset: u64,
}

impl PackedNativeActionStaging {
    pub(crate) fn writer(&mut self) -> &mut File {
        &mut self.file
    }

    pub(crate) fn finish_payload(&mut self) -> RailResult<()> {
        self.file.flush()?;
        let expected = self
            .payload_offset
            .checked_add(self.header.compressed_bytes)
            .ok_or_else(|| RailError::message("packed native action size overflow"))?;
        if !crate::utils::private_file_matches_path(&self.file, &self.path, expected)? {
            return Err(RailError::message(
                "packed native action staging is not the expected private file",
            ));
        }
        Ok(())
    }

    pub(crate) fn compressed_reader(&self) -> RailResult<File> {
        let mut file = self.file.try_clone()?;
        file.seek(std::io::SeekFrom::Start(self.payload_offset))?;
        Ok(file)
    }
}

/// Result of publishing one packed native action under exact conflict authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PackedNativeActionPublication {
    Created,
    Converged,
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

/// Disposable pointer from one pre-link selector to an exact witnessed action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeLinkCandidateIndexEntry {
    version: u32,
    candidate_key: String,
    action_key: String,
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
        drop(fs::remove_file(&self.path));
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
    validation: &'a NativeCompilerValidation,
    validation_bytes: &'a [u8],
    prepared: &'a PreparedTree,
    verified_generations: &'a BTreeMap<PathBuf, Vec<u8>>,
    move_preverified_blobs: bool,
}

struct StagedBundle {
    _temporary: tempfile::TempDir,
    _active: File,
    payload: PathBuf,
    stats: StoreStats,
}

/// One native result whose immutable bundle is ready for its authority commit.
struct StagedNativeResult {
    validation: NativeCompilerValidation,
    origins: NativeResultOrigins,
    object: ActionResultObject,
    action_result: String,
    incoming: u64,
    staged: StagedBundle,
}

impl StagedNativeResult {
    fn validation(&self) -> &NativeCompilerValidation {
        &self.validation
    }
}

struct CommittedNativeResult {
    validation: NativeCompilerValidation,
    stats: StoreStats,
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

type StoredValidation = NativeCompilerValidation;

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
        let _durability = native_durability_phase(NativeDurabilityPhase::CasLockWait);
        lock_local_cas(&self.lifecycle_lock, false, LockMode::Exclusive)?
            .ok_or_else(|| RailError::message("local CAS lifecycle lock disappeared"))
    }

    fn read_lock(&self) -> RailResult<LocalCasLifecycleLock> {
        lock_local_cas(&self.lifecycle_lock, false, LockMode::Shared)?
            .ok_or_else(|| RailError::message("local CAS lifecycle lock disappeared"))
    }

    /// Serialize publication to one exact Cargo output set without leaving target-visible residue.
    pub(crate) fn native_restore_lock(&self, identity: &crate::source::ContentDigest) -> RailResult<NativeRestoreLock> {
        let lifecycle = self.read_lock()?;
        let staging = self.root.join("staging");
        validate_real_directory(&staging, "local CAS staging")?;
        let path = staging.join(format!(".native-restore-{identity}.lock"));
        let file = crate::utils::open_cache_lock_file(&path, true)?;
        if !crate::utils::private_file_matches_path(&file, &path, 0)? {
            return Err(RailError::message(
                "native restore-commit lock is not a private empty file",
            ));
        }
        {
            let _durability = native_durability_phase(NativeDurabilityPhase::RestoreLockWait);
            file.lock()?;
        }
        if !crate::utils::private_file_matches_path(&file, &path, 0)? {
            return Err(RailError::message(
                "native restore-commit lock changed while it was acquired",
            ));
        }
        Ok(NativeRestoreLock {
            _file: file,
            _lifecycle: lifecycle,
        })
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
        sync_l1_file_full(temporary.as_file())?;
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
                    Some(existing) if existing == names => {
                        Ok(if self.native_environment_selector_conflicted(base_action_key)? {
                            NativeEnvironmentSelectorPublication::Diverged
                        } else {
                            NativeEnvironmentSelectorPublication::Converged
                        })
                    }
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
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
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
        sync_l1_file_full(temporary.as_file())?;
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
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
        (&mut file)
            .take(metadata.len().saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != metadata.len()
            || !crate::utils::private_file_matches_path(&file, &path, metadata.len())?
        {
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
        Ok(self
            .root
            .join(NATIVE_ENVIRONMENT_SELECTOR_DIRECTORY)
            .join(format!("{key}.json")))
    }

    fn native_environment_selector_conflict_path(&self, base_action_key: &str) -> RailResult<PathBuf> {
        let key = validated_id_hex(base_action_key, crate::compiler::native_cache::BASE_ACTION_KEY_PREFIX)?;
        Ok(self
            .root
            .join(NATIVE_ENVIRONMENT_SELECTOR_DIRECTORY)
            .join(format!("{key}.conflict")))
    }

    /// Load bounded exact-action candidates for one non-authoritative pre-link selector.
    #[cfg(any(target_os = "macos", target_os = "linux", test))]
    pub(crate) fn native_link_candidates(&self, candidate_key: &str) -> RailResult<Vec<String>> {
        let _lock = self.read_lock()?;
        let candidate_hex = validated_id_hex(candidate_key, crate::compiler::native_cache::CANDIDATE_SELECTOR_PREFIX)?;
        let directory = self.root.join(NATIVE_LINK_CANDIDATE_DIRECTORY).join(candidate_hex);
        match fs::symlink_metadata(&directory) {
            Ok(_) => validate_real_directory(&directory, "local CAS native link candidate")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        }
        let mut entries = fs::read_dir(&directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        if entries.len() > MAX_NATIVE_LINK_CANDIDATES {
            return Err(RailError::message(
                "local CAS native link candidate set exceeds its bound",
            ));
        }
        entries
            .into_iter()
            .map(|entry| {
                let metadata = fs::symlink_metadata(entry.path())?;
                if !metadata.is_file()
                    || is_link_or_reparse(&metadata)
                    || !has_single_link(&metadata)
                    || metadata.len() > MAX_NATIVE_LINK_CANDIDATE_BYTES
                {
                    return Err(RailError::message(
                        "local CAS native link candidate is not a bounded regular file",
                    ));
                }
                let mut stats = ReadStats::default();
                let candidate: NativeLinkCandidateIndexEntry =
                    read_canonical_json(&entry.path(), MAX_NATIVE_LINK_CANDIDATE_BYTES, &mut stats)
                        .map_err(fault_to_error)?;
                if candidate.version != NATIVE_LINK_CANDIDATE_VERSION || candidate.candidate_key != candidate_key {
                    return Err(RailError::message("local CAS native link candidate binding is invalid"));
                }
                crate::compiler::native_cache::validate_action_key(&candidate.action_key)?;
                let action_hex =
                    validated_id_hex(&candidate.action_key, crate::compiler::native_cache::ACTION_KEY_PREFIX)?;
                if entry.file_name() != OsStr::new(&format!("{action_hex}.json")) {
                    return Err(RailError::message(
                        "local CAS native link candidate filename is invalid",
                    ));
                }
                Ok(candidate.action_key)
            })
            .collect()
    }

    /// Publish a disposable pre-link selector only after exact action authority exists.
    pub(crate) fn publish_native_link_candidate(&self, candidate_key: &str, action_key: &str) -> RailResult<()> {
        let _lock = self.lock()?;
        validated_id_hex(candidate_key, crate::compiler::native_cache::CANDIDATE_SELECTOR_PREFIX)?;
        crate::compiler::native_cache::validate_action_key(action_key)?;
        let candidate_hex = validated_id_hex(candidate_key, crate::compiler::native_cache::CANDIDATE_SELECTOR_PREFIX)?;
        let action_hex = validated_id_hex(action_key, crate::compiler::native_cache::ACTION_KEY_PREFIX)?;
        let root = self.root.join(NATIVE_LINK_CANDIDATE_DIRECTORY);
        validate_real_directory(&root, "local CAS native link candidate root")?;
        let directory = create_real_directory(&root, candidate_hex)?;
        let candidate = NativeLinkCandidateIndexEntry {
            version: NATIVE_LINK_CANDIDATE_VERSION,
            candidate_key: candidate_key.to_string(),
            action_key: action_key.to_string(),
        };
        let bytes = canonical_json(&candidate)?;
        let destination = directory.join(format!("{action_hex}.json"));
        let destination_exists = fs::symlink_metadata(&destination).is_ok();
        if !destination_exists
            && bounded_optional_directory_entries(&directory, "local CAS native link candidate")?.len()
                >= MAX_NATIVE_LINK_CANDIDATES
        {
            return Err(RailError::message("local CAS native link candidate set is full"));
        }
        let mut temporary = tempfile::NamedTempFile::new_in(self.root.join("staging"))?;
        temporary.write_all(&bytes)?;
        sync_l1_file_full(temporary.as_file())?;
        match persist_noclobber_committed(temporary, &destination) {
            Ok(_) => sync_directory(&directory),
            Err(_)
                if fs::symlink_metadata(&destination).is_ok_and(|metadata| {
                    metadata.is_file() && !is_link_or_reparse(&metadata) && has_single_link(&metadata)
                }) =>
            {
                let mut stats = ReadStats::default();
                let existing: NativeLinkCandidateIndexEntry =
                    read_canonical_json(&destination, MAX_NATIVE_LINK_CANDIDATE_BYTES, &mut stats)
                        .map_err(fault_to_error)?;
                if existing == candidate {
                    Ok(())
                } else {
                    Err(RailError::message("local CAS native link candidate binding diverged"))
                }
            }
            Err(error) => Err(RailError::message(format!(
                "failed to publish local CAS native link candidate '{}': {}",
                destination.display(),
                error.error
            ))),
        }
    }

    /// Create private same-filesystem native-result staging guarded from concurrent GC.
    pub(crate) fn native_result_staging(&self) -> RailResult<crate::compiler::native_cache::pack::NativeResultStaging> {
        let (directory, active) = self.create_guarded_staging("native-result-")?;
        Ok(crate::compiler::native_cache::pack::NativeResultStaging::guarded(
            directory, active,
        ))
    }

    /// Stage one provider-neutral compressed result as its eventual action authority.
    pub(crate) fn packed_native_action_staging(
        &self,
        request: PackedNativeActionStagingRequest<'_>,
    ) -> RailResult<PackedNativeActionStaging> {
        let PackedNativeActionStagingRequest {
            base_action_key,
            environment_names,
            action_key,
            result_key,
            remote_authority,
            pack_bytes,
            compressed_bytes,
        } = request;
        let header = PackedNativeActionHeader {
            version: 1,
            base_action_key: base_action_key.to_string(),
            environment_names: environment_names.to_vec(),
            action_key: action_key.to_string(),
            result_key: result_key.to_string(),
            remote_authority: remote_authority.as_str().to_string(),
            pack_bytes,
            compressed_bytes,
        };
        validate_packed_native_action_header(&header, action_key)?;
        let header_bytes = canonical_json(&header)?;
        if header_bytes.len() as u64 > MAX_PACKED_NATIVE_ACTION_HEADER_BYTES {
            return Err(RailError::message("packed native action header exceeds its byte bound"));
        }
        let header_length = u32::try_from(header_bytes.len())
            .map_err(|_| RailError::message("packed native action header length is out of range"))?;
        let (directory, active) = self.create_guarded_staging("native-packed-action-")?;
        let path = directory.path().join("authority");
        let mut file = OpenOptions::new().read(true).write(true).create_new(true).open(&path)?;
        file.write_all(PACKED_NATIVE_ACTION_MAGIC)?;
        file.write_all(&PACKED_NATIVE_ACTION_VERSION.to_le_bytes())?;
        file.write_all(&header_length.to_le_bytes())?;
        file.write_all(&header_bytes)?;
        let payload_offset = PACKED_NATIVE_ACTION_PRELUDE_BYTES.saturating_add(u64::from(header_length));
        Ok(PackedNativeActionStaging {
            _directory: directory,
            _active: active,
            path,
            file,
            header,
            payload_offset,
        })
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
        self.root
            .join(SYSROOT_IDENTITY_MEMO_DIRECTORY)
            .join(format!("{lookup}.json"))
    }

    pub(crate) fn open() -> RailResult<Self> {
        Self::open_selected(&LocalCacheSelection::from_environment()?)
    }

    pub(crate) fn open_selected(selection: &LocalCacheSelection) -> RailResult<Self> {
        let base = selection.base().to_path_buf();
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
        let authority = selected_cache_authority(&cargo_rail, true, selection.trust_domain())?;
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
            NATIVE_LINK_CANDIDATE_DIRECTORY,
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
            max_bytes: selection.max_bytes(),
        })
    }

    /// Open the CAS prepared by this Cargo session without repeating lifecycle mutation.
    ///
    /// Native compiler wrappers call this only after the parent wrote the session
    /// through `LocalCas::open`. Every lookup and restore still takes the shared
    /// lifecycle lock and verifies its exact objects at the final operation.
    pub(crate) fn open_initialized() -> RailResult<Self> {
        Self::open_initialized_selected(&LocalCacheSelection::from_environment()?)
    }

    pub(crate) fn open_initialized_selected(selection: &LocalCacheSelection) -> RailResult<Self> {
        let base = selection.base();
        let base = fs::canonicalize(base).map_err(|error| {
            RailError::message(format!(
                "failed to resolve initialized local cache base '{}': {error}",
                base.display()
            ))
        })?;
        Self::open_initialized_at(&base, selection.max_bytes(), selection.trust_domain())
    }

    fn open_initialized_at(base: &Path, max_bytes: u64, trust_domain: Option<&str>) -> RailResult<Self> {
        validate_real_directory(base, "local cache base")?;
        let cargo_rail = base.join("cargo-rail");
        validate_real_directory(&cargo_rail, "local CAS owner")?;
        let authority = selected_cache_authority(&cargo_rail, false, trust_domain)?;
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
            NATIVE_LINK_CANDIDATE_DIRECTORY,
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
            NATIVE_LINK_CANDIDATE_DIRECTORY,
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

    #[cfg(any(unix, windows, test))]
    pub(crate) fn native_action(&self, action_key: &str) -> RailResult<NativeActionLookup<'_>> {
        self.native_action_with_retry(action_key, true)
    }

    #[cfg(any(unix, windows, test))]
    fn native_action_with_retry(&self, action_key: &str, retry_after_race: bool) -> RailResult<NativeActionLookup<'_>> {
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
        let state_metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(NativeActionLookup::Miss(NativeCacheMiss {
                    reason: "action_not_found".to_string(),
                    bytes_read: 0,
                }));
            }
            Err(error) => return Err(error.into()),
        };
        if state_metadata.is_file()
            && !is_link_or_reparse(&state_metadata)
            && has_single_link(&state_metadata)
            && state_metadata.len() >= PACKED_NATIVE_ACTION_PRELUDE_BYTES
        {
            let mut file = File::open(&path)?;
            let mut magic = [0_u8; 8];
            file.read_exact(&mut magic)?;
            if &magic == PACKED_NATIVE_ACTION_MAGIC {
                match read_packed_native_action_header(&mut file, &path, action_key) {
                    Ok((header, payload_offset)) => {
                        return Ok(NativeActionLookup::Packed(Box::new(PackedNativeActionHit {
                            bytes_read: payload_offset,
                            header,
                            file,
                            refresh_access: access_refresh_due(&state_metadata),
                            cas: self,
                            _lock: lock,
                        })));
                    }
                    Err(error) => {
                        let evidence = error.to_string().into_bytes();
                        drop(file);
                        drop(lock);
                        let quarantined = self.quarantine_native_action_if_invalid(
                            action_key,
                            NativeStateFault::Malformed,
                            &evidence,
                        )?;
                        if !quarantined && retry_after_race {
                            return self.native_action_with_retry(action_key, false);
                        }
                        return Ok(NativeActionLookup::Miss(NativeCacheMiss {
                            reason: "action_quarantined".to_string(),
                            bytes_read: PACKED_NATIVE_ACTION_PRELUDE_BYTES,
                        }));
                    }
                }
            }
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
                    return self.native_action_with_retry(action_key, false);
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
                    return self.native_action_with_retry(action_key, false);
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
        if !origins.local {
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
        let validation = verified.validation.clone();
        validation.validate_object()?;
        if validation.action_key() != action_key || validation.result_key() != result_key {
            return Err(RailError::message(
                "local CAS native descriptor does not match its action state",
            ));
        }
        Ok(NativeActionLookup::Hit(Box::new(NativeActionHit {
            validation,
            bytes_read: stats.bytes,
            refresh_access: access_refresh_due(&state_metadata),
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
        let (fault, evidence, packed_bytes) = match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (observed_fault, observed_evidence.to_vec(), None)
            }
            Err(error) => (NativeStateFault::Unreadable, error.to_string().into_bytes(), None),
            Ok(metadata) if metadata.is_file() && !is_link_or_reparse(&metadata) && has_single_link(&metadata) => {
                match try_read_packed_native_action(&path, action_key) {
                    Ok(Some(_)) => (observed_fault, observed_evidence.to_vec(), Some(metadata.len())),
                    Err(error) => (
                        NativeStateFault::Malformed,
                        error.to_string().into_bytes(),
                        Some(metadata.len()),
                    ),
                    Ok(None) if metadata.len() <= MAX_OBJECT_METADATA_BYTES => match fs::read(&path) {
                        Ok(bytes) => match decode_native_action_state(&bytes, action_key) {
                            Ok(_) => return Ok(false),
                            Err(fault) => (fault, bytes, None),
                        },
                        Err(error) => (NativeStateFault::Unreadable, error.to_string().into_bytes(), None),
                    },
                    Ok(None) => (NativeStateFault::Unreadable, observed_evidence.to_vec(), None),
                }
            }
            Ok(_) => (NativeStateFault::Unreadable, observed_evidence.to_vec(), None),
        };
        let quarantined = quarantined_native_action_state(action_key, fault, &evidence);
        self.publish_terminal_native_state(&path, &quarantined)?;
        if let Some(packed_bytes) = packed_bytes {
            self.settle_result_capacity(packed_bytes, 0)?;
        }
        Ok(true)
    }

    pub(crate) fn quarantine_packed_native_action(&self, action_key: &str, reason: &str) -> RailResult<()> {
        self.quarantine_native_action_if_invalid(action_key, NativeStateFault::Malformed, reason.as_bytes())
            .map(|_| ())
    }

    /// Load fully verified compiler evidence discovered by one non-authoritative configuration key.
    pub(crate) fn compiler_evidence_candidates(
        &self,
        candidate_key: &str,
    ) -> RailResult<Vec<CompilerEvidenceCandidate>> {
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

    #[cfg(test)]
    pub(crate) fn store_native(
        &self,
        prepared: PreparedNativeResult,
    ) -> RailResult<(NativeCompilerValidation, StoreStats)> {
        self.store_native_revalidated(prepared, |_| Ok(()))
    }

    /// Publish one already-authenticated compressed result as the action's sole L1 result authority.
    pub(crate) fn commit_packed_native_action_revalidated<F>(
        &self,
        mut staging: PackedNativeActionStaging,
        validation: &NativeCompilerValidation,
        mut revalidate: F,
    ) -> RailResult<PackedNativeActionPublication>
    where
        F: FnMut(&NativeCompilerValidation) -> RailResult<()>,
    {
        staging.finish_payload()?;
        validation.validate_object()?;
        if validation.action_key() != staging.header.action_key || validation.result_key() != staging.header.result_key
        {
            return Err(RailError::message(
                "packed native action does not match its authenticated validation",
            ));
        }
        let expected_bytes = staging
            .payload_offset
            .checked_add(staging.header.compressed_bytes)
            .ok_or_else(|| RailError::message("packed native action size overflow"))?;
        let generation = crate::utils::stable_file_generation(&staging.path)
            .ok_or_else(|| RailError::message("packed native action has no stable local generation"))?;
        {
            let _durability = native_durability_phase(NativeDurabilityPhase::L1FileSync);
            staging.file.sync_all()?;
        }
        if crate::utils::stable_file_generation(&staging.path).as_ref() != Some(&generation)
            || !crate::utils::private_file_matches_path(&staging.file, &staging.path, expected_bytes)?
        {
            return Err(RailError::message(
                "packed native action changed during its durability barrier",
            ));
        }
        revalidate(validation)?;

        let _lock = self.lock()?;
        let _durability = native_durability_phase(NativeDurabilityPhase::CasCommit);
        if validate_native_ledger(&self.root)?.disabled {
            return Err(RailError::with_help(
                "native cache authority is disabled because its terminal-state ledger is full",
                "run `cargo rail cache clean --scope local` to explicitly reset the complete authority root",
            ));
        }
        let action_key = staging.header.action_key.clone();
        let result_key = staging.header.result_key.clone();
        let action_hex = validated_action_key_hex(&action_key)?;
        let destination = self
            .root
            .join(NATIVE_ACTION_STATE_DIRECTORY)
            .join(format!("{action_hex}.json"));
        match fs::symlink_metadata(&destination) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.reserve_result_capacity(expected_bytes, None)?;
                rename_committed(&staging.path, &destination, false).map_err(|error| {
                    RailError::message(format!(
                        "failed to publish packed native action '{}': {error}",
                        destination.display()
                    ))
                })?;
                if !crate::utils::private_file_matches_path(&staging.file, &destination, expected_bytes)? {
                    return Err(RailError::message(
                        "packed native action changed at its publication boundary",
                    ));
                }
                sync_directory_before_commit(
                    destination
                        .parent()
                        .ok_or_else(|| RailError::message("packed native action has no parent directory"))?,
                )?;
                crate::instrumentation::record_cas_write(expected_bytes, 1);
                Ok(PackedNativeActionPublication::Created)
            }
            Err(error) => Err(error.into()),
            Ok(metadata) => {
                if !metadata.is_file() || is_link_or_reparse(&metadata) || !has_single_link(&metadata) {
                    let quarantined = quarantined_native_action_state(&action_key, NativeStateFault::Unreadable, &[]);
                    self.publish_terminal_native_state(&destination, &quarantined)?;
                    return Err(RailError::message(
                        "local CAS native action state was quarantined because it was unreadable",
                    ));
                }
                let mut existing = File::open(&destination)?;
                let mut magic = [0_u8; 8];
                existing.read_exact(&mut magic)?;
                let (existing_result, packed_bytes) = if &magic == PACKED_NATIVE_ACTION_MAGIC {
                    let (header, _) = read_packed_native_action_header(&mut existing, &destination, &action_key)?;
                    (header.result_key, Some(metadata.len()))
                } else if metadata.len() <= MAX_OBJECT_METADATA_BYTES {
                    let bytes = fs::read(&destination)?;
                    let state = decode_native_action_state(&bytes, &action_key)
                        .map_err(|_| RailError::message("existing local CAS native action state is malformed"))?;
                    match state.state {
                        NativeActionStateKind::UniqueResult { result_key, .. } => (result_key, None),
                        NativeActionStateKind::ConflictedResults { .. } => {
                            return Err(RailError::message("local CAS native action is durably conflicted"));
                        }
                        NativeActionStateKind::Quarantined { .. } => {
                            return Err(RailError::message("local CAS native action is durably quarantined"));
                        }
                    }
                } else {
                    return Err(RailError::message(
                        "existing local CAS native action state exceeds its byte bound",
                    ));
                };
                if existing_result == result_key {
                    return Ok(PackedNativeActionPublication::Converged);
                }
                let (first_result_key, second_result_key) = if existing_result < result_key {
                    (existing_result, result_key)
                } else {
                    (result_key, existing_result)
                };
                let conflicted = NativeActionState {
                    version: NATIVE_ACTION_STATE_VERSION,
                    action_key,
                    state: NativeActionStateKind::ConflictedResults {
                        first_result_key,
                        second_result_key,
                    },
                };
                self.publish_terminal_native_state(&destination, &conflicted)?;
                if let Some(packed_bytes) = packed_bytes {
                    self.settle_result_capacity(packed_bytes, 0)?;
                }
                Err(RailError::message(
                    "native action produced two different verified results",
                ))
            }
        }
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
        let committed = self.commit_staged_native(staged)?;
        Ok((committed.validation, committed.stats))
    }

    /// Prepare one native result completely without publishing payload or
    /// action authority. This is intentionally safe to overlap with rustc.
    fn stage_native(&self, prepared: PreparedNativeResult) -> RailResult<StagedNativeResult> {
        let PreparedNativeParts {
            staging,
            staging_lock: _staging_lock,
            verified_generations,
            manifest,
            validation,
            move_preverified_blobs,
        } = prepared.into_parts();
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
        let native_origins = NativeResultOrigins {
            local: true,
            remote: None,
        };
        if !move_preverified_blobs {
            manifest.validate_unchanged(source_root)?;
        }
        let prepared = prepare_tree(&manifest, source_root).map_err(fault_to_error)?;
        let manifest_bytes = canonical_json(&manifest)?;
        let validation_bytes = canonical_json(&validation)?;
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
                validation: &validation,
                validation_bytes: &validation_bytes,
                prepared: &prepared,
                verified_generations: &verified_generations,
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

    fn commit_staged_native(&self, staged: StagedNativeResult) -> RailResult<CommittedNativeResult> {
        let StagedNativeResult {
            validation,
            origins,
            object,
            action_result,
            incoming,
            staged,
        } = staged;
        let _lock = self.lock()?;
        let _durability = native_durability_phase(NativeDurabilityPhase::CasCommit);
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
        Ok(CommittedNativeResult { validation, stats })
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
}

struct VerifiedResult {
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

fn selected_cache_authority(
    owner: &Path,
    create_default: bool,
    explicit_trust_domain: Option<&str>,
) -> RailResult<SelectedCacheAuthority> {
    if let Some(trust_domain) = explicit_trust_domain {
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
            if fs::symlink_metadata(&path).is_ok_and(|metadata| {
                metadata.is_file() && !is_link_or_reparse(&metadata) && has_single_link(&metadata)
            }) =>
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

fn configured_root_for(selection: &LocalCacheSelection) -> RailResult<Option<PathBuf>> {
    let base = selection.base();
    let base = match fs::canonicalize(base) {
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
    let authority = match selected_cache_authority(&owner, false, selection.trust_domain()) {
        Ok(authority) => authority,
        Err(_)
            if selection.trust_domain().is_none()
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
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        RailError::message(format!("failed to inspect {description} '{}': {error}", path.display()))
    })?;
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
    use crate::windows_fs::{observe_file, open_for_observation, prove_local_ntfs};

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
            if fs::symlink_metadata(&marker)
                .is_ok_and(|metadata| metadata.is_file() && !is_link_or_reparse(&metadata)) => {}
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
        NATIVE_LINK_CANDIDATE_DIRECTORY,
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
    if action_key.starts_with(EVIDENCE_ACTION_KEY_PREFIX) {
        validated_id_hex(action_key, EVIDENCE_ACTION_KEY_PREFIX)
    } else {
        validated_id_hex(action_key, crate::compiler::native_cache::ACTION_KEY_PREFIX)
    }
}

fn validate_any_lookup_key(lookup_key: &str) -> RailResult<()> {
    if lookup_key.starts_with(EVIDENCE_CANDIDATE_KEY_PREFIX) {
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

fn validate_packed_native_action_header(header: &PackedNativeActionHeader, expected_action: &str) -> RailResult<()> {
    if header.version != 1 || header.action_key != expected_action {
        return Err(RailError::message("packed native action has an incompatible identity"));
    }
    crate::compiler::native_cache::validate_base_action_key(&header.base_action_key)?;
    crate::compiler::native_cache::validate_environment_selector_names(
        header.environment_names.iter().map(String::as_str),
    )?;
    crate::compiler::native_cache::validate_action_key(&header.action_key)?;
    crate::compiler::native_cache::validate_result_key(&header.result_key)?;
    crate::compiler::native_cache::RemoteAuthorityId::parse(header.remote_authority.clone())?;
    if header.pack_bytes == 0
        || header.pack_bytes > crate::compiler::native_cache::pack::MAX_PACK_BYTES
        || header.compressed_bytes == 0
        || header.compressed_bytes > MAX_PACKED_NATIVE_ACTION_BYTES
    {
        return Err(RailError::message("packed native action has invalid byte bounds"));
    }
    Ok(())
}

fn packed_native_action_payload_offset(header: &PackedNativeActionHeader) -> RailResult<u64> {
    let header_bytes = canonical_json(header)?;
    let header_length = u64::try_from(header_bytes.len())
        .map_err(|_| RailError::message("packed native action header length is out of range"))?;
    if header_length > MAX_PACKED_NATIVE_ACTION_HEADER_BYTES {
        return Err(RailError::message("packed native action header exceeds its byte bound"));
    }
    PACKED_NATIVE_ACTION_PRELUDE_BYTES
        .checked_add(header_length)
        .ok_or_else(|| RailError::message("packed native action header size overflow"))
}

fn read_packed_native_action_header(
    file: &mut File,
    path: &Path,
    expected_action: &str,
) -> RailResult<(PackedNativeActionHeader, u64)> {
    file.seek(std::io::SeekFrom::Start(0))?;
    let mut prelude = [0_u8; PACKED_NATIVE_ACTION_PRELUDE_LEN];
    file.read_exact(&mut prelude)?;
    if &prelude[..8] != PACKED_NATIVE_ACTION_MAGIC
        || u16::from_le_bytes([prelude[8], prelude[9]]) != PACKED_NATIVE_ACTION_VERSION
    {
        return Err(RailError::message("packed native action prelude is incompatible"));
    }
    let header_length = u32::from_le_bytes(
        prelude[10..14]
            .try_into()
            .map_err(|_| RailError::message("packed native action header length is malformed"))?,
    ) as u64;
    if header_length > MAX_PACKED_NATIVE_ACTION_HEADER_BYTES {
        return Err(RailError::message("packed native action header exceeds its byte bound"));
    }
    let header_capacity = usize::try_from(header_length)
        .map_err(|_| RailError::message("packed native action header length is out of range"))?;
    let mut encoded = vec![0_u8; header_capacity];
    file.read_exact(&mut encoded)?;
    let header = serde_json::from_slice::<PackedNativeActionHeader>(&encoded)
        .map_err(|_| RailError::message("packed native action header is malformed"))?;
    validate_packed_native_action_header(&header, expected_action)?;
    if canonical_json(&header)? != encoded {
        return Err(RailError::message("packed native action header is not canonical"));
    }
    let payload_offset = PACKED_NATIVE_ACTION_PRELUDE_BYTES
        .checked_add(header_length)
        .ok_or_else(|| RailError::message("packed native action header size overflow"))?;
    let expected_bytes = payload_offset
        .checked_add(header.compressed_bytes)
        .ok_or_else(|| RailError::message("packed native action size overflow"))?;
    if !crate::utils::private_file_matches_path(file, path, expected_bytes)? {
        return Err(RailError::message(
            "packed native action is not the expected private file",
        ));
    }
    Ok((header, payload_offset))
}

fn try_read_packed_native_action(
    path: &Path,
    expected_action: &str,
) -> RailResult<Option<(PackedNativeActionHeader, u64)>> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.len() < PACKED_NATIVE_ACTION_PRELUDE_BYTES {
        return Ok(None);
    }
    let mut file = File::open(path)?;
    let mut magic = [0_u8; 8];
    file.read_exact(&mut magic)?;
    if &magic != PACKED_NATIVE_ACTION_MAGIC {
        return Ok(None);
    }
    read_packed_native_action_header(&mut file, path, expected_action).map(Some)
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
            FaultKind::Corrupt => "corruption",
            FaultKind::Incompatible => "incompatibility",
        },
        fault.reason
    ))
}

fn validate_manifest(manifest: &OutputManifest) -> Result<(), Fault> {
    if manifest.version != OUTPUT_MANIFEST_VERSION {
        return Err(Fault::incompatible("output_manifest_schema_version"));
    }
    if manifest.entries.len() > MAX_ENTRIES {
        return Err(Fault::corrupt("output_manifest_entry_limit"));
    }
    let expected_digest = output_manifest_digest(&manifest.entries)
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
                let Some((expected_digest, expected_bytes, expected_mode)) = expected_files.remove(entry.path.as_str())
                else {
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
    let parsed =
        crate::source::RepositoryPath::new(Path::new(path)).map_err(|_| Fault::corrupt("unsafe_output_path"))?;
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
        valid_regular_file_mode(mode) || valid_executable_file_mode(mode)
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

#[cfg(unix)]
const fn valid_executable_file_mode(mode: u32) -> bool {
    mode & !0o777 == 0 && mode & 0o500 == 0o500 && mode & 0o111 != 0
}

#[cfg(not(unix))]
const fn valid_regular_file_mode(mode: u32) -> bool {
    matches!(mode, 0o444 | 0o644)
}

#[cfg(not(unix))]
const fn valid_executable_file_mode(mode: u32) -> bool {
    mode == 0o755
}

fn validate_symlink(path: &str, target: &str) -> Result<(), Fault> {
    if target.is_empty() || target.contains(['\0', '\\']) || target.len() > MAX_PATH_BYTES {
        return Err(Fault::corrupt("unsafe_symlink_target"));
    }
    let path =
        crate::source::RepositoryPath::new(Path::new(path)).map_err(|_| Fault::corrupt("unsafe_symlink_path"))?;
    if symlink_target_escapes(&path, target) {
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
        if validation.action_key() != object.action_key || validation.action_key() != object.lookup_key {
            return Err(Fault::corrupt("validation_action_binding_mismatch"));
        }
        validation
            .validate_object()
            .map_err(|error| Fault::corrupt(format!("validation_object: {error}")))?;
        validate_native_output_manifest(&manifest, &validation)?;
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
        Ok(VerifiedResult {
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

    #[cfg(test)]
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
    // durable CAS bundle. Unix can flush the published copy through a reopened
    // read handle. Windows cannot, so its original writable materialization
    // handle owns the barrier before the write-through rename.
    let durable = cfg!(windows);
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
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
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
                materialize_blob(MaterializeBlobRequest {
                    bundle,
                    identity: blob,
                    content_digest,
                    expected_bytes: *bytes,
                    mode: *mode,
                    destination: &path,
                    stats,
                    durable,
                })?;
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
fn materialize_blob(request: MaterializeBlobRequest<'_>) -> Result<(), Fault> {
    let MaterializeBlobRequest {
        bundle,
        identity,
        content_digest,
        expected_bytes,
        mode,
        destination,
        stats,
        durable,
    } = request;
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
    let input = File::open(&source).map_err(|error| Fault::corrupt(format!("blob_open: {error}")))?;
    let opened = input
        .metadata()
        .map_err(|error| Fault::corrupt(format!("blob_opened_metadata: {error}")))?;
    if !opened.is_file() || !has_single_link(&opened) || opened.len() != expected_bytes {
        return Err(Fault::corrupt("blob_changed_before_read"));
    }
    let (output, cloned) = match crate::utils::try_clone_regular_file(&input, destination) {
        Some(output) => (output, true),
        None => (
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)
                .map_err(|error| Fault::corrupt(format!("blob_destination_create: {error}")))?,
            false,
        ),
    };
    let mut readable = if cloned { &output } else { &input };
    let mut writable = &output;
    let mut digest = Sha256::new();
    let mut copied = 0u64;
    let mut buffer = [0_u8; IO_BUFFER_BYTES];
    loop {
        let remaining = maximum_read.saturating_sub(copied);
        if remaining == 0 {
            break;
        }
        let read_capacity = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = readable
            .read(&mut buffer[..read_capacity])
            .map_err(|error| Fault::corrupt(format!("blob_read: {error}")))?;
        if read == 0 {
            break;
        }
        if !cloned {
            writable
                .write_all(&buffer[..read])
                .map_err(|error| Fault::corrupt(format!("blob_write: {error}")))?;
        }
        digest.update(&buffer[..read]);
        copied = copied.saturating_add(read as u64);
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
    crate::instrumentation::record_hash(usize::try_from(copied).unwrap_or(usize::MAX));
    crate::instrumentation::record_hashed_file_bytes_read(usize::try_from(copied).unwrap_or(usize::MAX));
    set_exact_file_mode(&output, mode)?;
    output
        .set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::now()))
        .map_err(|error| Fault::corrupt(format!("output_mtime: {error}")))?;
    if durable {
        let _durability = native_durability_phase(NativeDurabilityPhase::L1FileSync);
        output
            .sync_all()
            .map_err(|error| Fault::corrupt(format!("blob_sync: {error}")))?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_exact_file_mode(file: &File, mode: u32) -> Result<(), Fault> {
    use std::os::unix::fs::PermissionsExt as _;

    file.set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|error| Fault::corrupt(format!("output_mode: {error}")))
}

#[cfg(not(unix))]
fn set_exact_file_mode(file: &File, mode: u32) -> Result<(), Fault> {
    let mut permissions = file
        .metadata()
        .map_err(|error| Fault::corrupt(format!("output_mode_metadata: {error}")))?
        .permissions();
    permissions.set_readonly(mode & 0o200 == 0);
    file.set_permissions(permissions)
        .map_err(|error| Fault::corrupt(format!("output_mode: {error}")))
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
    let _durability = native_durability_phase(NativeDurabilityPhase::L1DirectorySync);
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
            verified_generations,
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
                move_blob_verified(
                    blob,
                    identity,
                    &destination,
                    verified_generations.get(&blob.source).map(Vec::as_slice),
                )?
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
                self.load_verified_result(&object.action_key, action_result, &mut read)
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
                self.load_verified_result(&object.action_key, action_result, &mut read)
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
                self.load_verified_compiler_evidence(&object.action_key, action_result, &mut read)
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
                self.load_verified_compiler_evidence(&object.action_key, action_result, &mut read)
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
        sync_l1_file_full(temporary.as_file())?;
        match persist_noclobber_committed(temporary, &destination) {
            Ok(_) => {
                sync_directory(&directory)?;
                sync_directory(&index_root)
            }
            Err(_)
                if fs::symlink_metadata(&destination).is_ok_and(|metadata| {
                    metadata.is_file() && !is_link_or_reparse(&metadata) && has_single_link(&metadata)
                }) =>
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
        sync_l1_file_full(temporary.as_file())?;
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
                if !metadata.is_file() || is_link_or_reparse(&metadata) || !has_single_link(&metadata) {
                    let quarantined = quarantined_native_action_state(action_key, NativeStateFault::Unreadable, &[]);
                    self.publish_terminal_native_state(&destination, &quarantined)?;
                    return Err(RailError::message(
                        "local CAS native action state was quarantined because it was unreadable",
                    ));
                }
                if metadata.len() >= PACKED_NATIVE_ACTION_PRELUDE_BYTES {
                    let mut file = File::open(&destination)?;
                    let mut magic = [0_u8; 8];
                    file.read_exact(&mut magic)?;
                    if &magic == PACKED_NATIVE_ACTION_MAGIC {
                        let (packed, _) = read_packed_native_action_header(&mut file, &destination, action_key)?;
                        if packed.result_key == result_key {
                            return Ok(());
                        }
                        let (first_result_key, second_result_key) = if packed.result_key.as_str() < result_key {
                            (packed.result_key, result_key.to_string())
                        } else {
                            (result_key.to_string(), packed.result_key)
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
                        self.settle_result_capacity(metadata.len(), 0)?;
                        return Err(RailError::with_help(
                            format!("native action '{action_key}' produced two different verified results"),
                            "the action is nondeterministic; this cache authority root will never restore that action",
                        ));
                    }
                }
                if metadata.len() > MAX_OBJECT_METADATA_BYTES {
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
        sync_l1_file_full(temporary.as_file())?;
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
    sync_l1_file_full(&file)?;
    Ok(())
}

fn sync_l1_file_full(file: &File) -> RailResult<()> {
    let _durability = native_durability_phase(NativeDurabilityPhase::L1FileSync);
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
    if let Err(error) = crate::windows_fs::rename_write_through(&temporary_path, destination, false) {
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
    crate::windows_fs::rename_write_through(source, destination, replace)
}

#[cfg(not(windows))]
fn rename_committed(source: &Path, destination: &Path, _replace: bool) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(target_os = "macos")]
fn sync_before_commit(file: &File) -> RailResult<()> {
    let _durability = native_durability_phase(NativeDurabilityPhase::L1FileSync);
    rustix::fs::fsync(file).map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn sync_before_commit(file: &File) -> RailResult<()> {
    sync_l1_file_full(file)?;
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
    if !metadata.is_file()
        || is_link_or_reparse(&metadata)
        || !has_single_link(&metadata)
        || metadata.len() != blob.bytes
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
    crate::instrumentation::record_hash(usize::try_from(copied).unwrap_or(usize::MAX));
    crate::instrumentation::record_hashed_file_bytes_read(usize::try_from(copied).unwrap_or(usize::MAX));
    Ok(copied)
}

fn move_blob_verified(
    blob: &PreparedBlob,
    identity: &str,
    destination: &Path,
    verified_generation: Option<&[u8]>,
) -> RailResult<u64> {
    let metadata = fs::symlink_metadata(&blob.source)?;
    if !metadata.is_file()
        || is_link_or_reparse(&metadata)
        || !has_single_link(&metadata)
        || metadata.len() != blob.bytes
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
    if let Some(verified_generation) = verified_generation {
        if blob_id(&blob.content_digest, blob.bytes).map_err(fault_to_error)? != identity
            || crate::utils::stable_file_generation(&blob.source).as_deref() != Some(verified_generation)
        {
            return Err(RailError::message(format!(
                "verified staged output '{}' changed after its content digest was captured",
                blob.source.display()
            )));
        }
        #[cfg(not(windows))]
        sync_before_commit(&input)?;
        if !crate::utils::private_file_matches_path(&input, &blob.source, blob.bytes)?
            || crate::utils::stable_file_generation(&blob.source).as_deref() != Some(verified_generation)
        {
            return Err(RailError::message(format!(
                "verified staged output '{}' changed before zero-copy admission",
                blob.source.display()
            )));
        }
        drop(input);
        fs::rename(&blob.source, destination)?;
        return Ok(blob.bytes);
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
    // Unix command-scoped wrapper staging is intentionally not durable: a crash
    // may lose a candidate, but must never expose authority over unsynced bytes.
    // Its admission worker owns the barrier here. Windows staging was already
    // flushed through the producer's writable handle because a reopened read
    // handle cannot provide that barrier.
    #[cfg(not(windows))]
    sync_before_commit(&input)?;
    drop(input);
    fs::rename(&blob.source, destination)?;
    crate::instrumentation::record_hash(usize::try_from(copied).unwrap_or(usize::MAX));
    crate::instrumentation::record_hashed_file_bytes_read(usize::try_from(copied).unwrap_or(usize::MAX));
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
    result: Option<String>,
    packed_bytes: u64,
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
            let pin: ActionPin =
                read_canonical_json(&path, MAX_OBJECT_METADATA_BYTES, &mut stats).map_err(fault_to_error)?;
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
                result: Some(pin.action_result),
                packed_bytes: 0,
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
            if try_read_packed_native_action(&path, &action_key)?.is_some() {
                authorities.push(GcAuthority {
                    path,
                    key: action_key,
                    result: None,
                    packed_bytes: metadata.len(),
                    last_used: last_used_unix_nanos(&metadata, 0),
                    kind: GcAuthorityKind::NativeAction,
                });
                continue;
            }
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
                    result: Some(action_result),
                    packed_bytes: 0,
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
            if let Some(result) = &authority.result {
                *references.entry(result.clone()).or_default() += 1;
            }
        }
        for (result, size) in result_sizes.clone() {
            if !references.contains_key(&result) && !leased.contains(&result) {
                let path = result_path(&self.root, &result)?;
                safe_remove_tree(&path)?;
                result_sizes.remove(&result);
                let _ = size;
            }
        }

        let materialized = result_sizes.values().try_fold(0u64, |total, size| {
            total
                .checked_add(*size)
                .ok_or_else(|| RailError::message("local CAS result size overflow"))
        })?;
        let mut current = authorities.iter().try_fold(materialized, |total, authority| {
            total
                .checked_add(authority.packed_bytes)
                .ok_or_else(|| RailError::message("local CAS packed result size overflow"))
        })?;
        for authority in authorities {
            if current <= target_bytes {
                break;
            }
            if authority.result.as_ref().is_some_and(|result| leased.contains(result)) {
                continue;
            }
            fs::remove_file(&authority.path)?;
            current = current.saturating_sub(authority.packed_bytes);
            match &authority.kind {
                GcAuthorityKind::Pin { lookup_key } if lookup_key.starts_with(EVIDENCE_CANDIDATE_KEY_PREFIX) => {
                    current = current
                        .saturating_sub(self.remove_compiler_evidence_candidate_index(&authority.key, lookup_key)?);
                    sync_directory(&pins_directory)?;
                }
                GcAuthorityKind::Pin { .. } => sync_directory(&pins_directory)?,
                GcAuthorityKind::NativeAction => sync_directory(&native_actions_directory)?,
            }
            let Some(result) = authority.result else {
                continue;
            };
            if let Some(count) = references.get_mut(&result) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    references.remove(&result);
                    if let Some(size) = result_sizes.remove(&result) {
                        safe_remove_tree(&result_path(&self.root, &result)?)?;
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
    Ok(root
        .join("results")
        .join(validated_id_hex(identity, ACTION_RESULT_PREFIX)?))
}

fn checked_tree_bytes(root: &Path) -> RailResult<u64> {
    checked_tree_file_stats(root).map(|(_, bytes)| bytes)
}

fn reconcile_capacity_state(root: &Path) -> RailResult<()> {
    let materialized = checked_tree_bytes(&root.join("results"))?;
    let mut packed = 0_u64;
    for entry in bounded_optional_directory_entries(
        &root.join(NATIVE_ACTION_STATE_DIRECTORY),
        "local CAS packed native actions",
    )? {
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| RailError::message("local CAS native action state has a non-UTF-8 name"))?;
        let key = name
            .strip_suffix(".json")
            .ok_or_else(|| RailError::message("local CAS native action state has a noncanonical name"))?;
        let action_key = format!("{}{key}", crate::compiler::native_cache::ACTION_KEY_PREFIX);
        if try_read_packed_native_action(&path, &action_key)?.is_some() {
            packed = packed
                .checked_add(fs::symlink_metadata(path)?.len())
                .ok_or_else(|| RailError::message("local CAS packed result size overflow"))?;
        }
    }
    write_capacity_state(
        root,
        materialized
            .checked_add(packed)
            .ok_or_else(|| RailError::message("local CAS result size overflow"))?,
    )
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
        .unwrap_or_else(|| Path::new("."));
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
        .unwrap_or_else(|| Path::new("."));
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
    if let Err(error) = crate::windows_fs::rename_write_through(&temporary_path, path, true) {
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
        let packed = try_read_packed_native_action(&path, &action_key);
        let terminal = if matches!(packed, Ok(Some(_))) {
            false
        } else if packed.is_err() || metadata.len() > MAX_OBJECT_METADATA_BYTES {
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

pub(crate) fn existing_root_at(root: &Path) -> RailResult<Option<PathBuf>> {
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
        NATIVE_LINK_CANDIDATE_DIRECTORY,
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

pub(crate) fn status_at_with_max(root: &Path, max_bytes: u64) -> RailResult<Option<LocalCasStatus>> {
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
        let pin: ActionPin =
            read_canonical_json(&path, MAX_OBJECT_METADATA_BYTES, &mut stats).map_err(fault_to_error)?;
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
        if try_read_packed_native_action(&path, &action_key)?.is_some() {
            native_unique = native_unique.saturating_add(1);
            native_local_origins = native_local_origins.saturating_add(1);
            native_remote_origins = native_remote_origins.saturating_add(1);
            let last_used = last_used_unix_nanos(&metadata, 0);
            oldest_used = Some(oldest_used.map_or(last_used, |current| current.min(last_used)));
            newest_used = Some(newest_used.map_or(last_used, |current| current.max(last_used)));
            continue;
        }
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
    let native_link_index_files = optional_tree_file_stats(&root.join(NATIVE_LINK_CANDIDATE_DIRECTORY))?.0;
    let index_files = optional_tree_file_stats(&root.join(EVIDENCE_CANDIDATE_INDEX_DIRECTORY))?
        .0
        .checked_add(optional_tree_file_stats(&root.join(NATIVE_ENVIRONMENT_SELECTOR_DIRECTORY))?.0)
        .and_then(|files| files.checked_add(native_link_index_files))
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

fn access_refresh_due(metadata: &fs::Metadata) -> bool {
    metadata
        .modified()
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_none_or(|elapsed| elapsed >= ACCESS_REFRESH_INTERVAL)
}

pub(crate) fn remove_owned_root_at(root: &Path) -> RailResult<Option<(PathBuf, u64)>> {
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

    #[test]
    fn preverified_blob_generation_rejects_same_length_mutation() {
        let staging = tempfile::tempdir().expect("staging");
        let source = staging.path().join("source");
        let destination = staging.path().join("destination");
        let bytes = b"verified";
        fs::write(&source, bytes).expect("source");
        let generation = crate::utils::stable_file_generation(&source).expect("stable file generation");
        let content_digest = format!("sha256:{}", ContentDigest::sha256(bytes));
        let blob = PreparedBlob {
            source: source.clone(),
            content_digest: content_digest.clone(),
            bytes: bytes.len() as u64,
        };
        let identity = blob_id(&content_digest, bytes.len() as u64).expect("blob identity");
        let started = std::time::Instant::now();
        loop {
            fs::write(&source, b"tampered").expect("same-length mutation");
            if crate::utils::stable_file_generation(&source).as_ref() != Some(&generation) {
                break;
            }
            assert!(
                started.elapsed() < std::time::Duration::from_secs(1),
                "the filesystem generation must advance after a same-length mutation"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let error = move_blob_verified(&blob, &identity, &destination, Some(&generation))
            .expect_err("changed generation must be rejected");

        assert!(
            error.to_string().contains("changed after its content digest"),
            "{error}"
        );
        assert!(!destination.exists());
    }

    fn base_action_key(value: u8) -> String {
        format!("{}{value:064x}", crate::compiler::native_cache::BASE_ACTION_KEY_PREFIX)
    }

    fn native_action_key(value: u8) -> String {
        format!("{}{value:064x}", crate::compiler::native_cache::ACTION_KEY_PREFIX)
    }

    fn native_link_candidate_key(value: u8) -> String {
        format!(
            "{}{value:064x}",
            crate::compiler::native_cache::CANDIDATE_SELECTOR_PREFIX
        )
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
            set_exact_mode(&path, 0o644).expect("fixture output mode");
            paths.push(path);
        }
        let manifest =
            crate::cache::result::capture_native_compiler_outputs(root, &paths).expect("native output manifest");
        let validation = crate::compiler::native_cache::tests::cas_validation_with_stdout(stdout);
        (manifest, validation)
    }

    fn store_native_fixture(
        cas: &LocalCas,
        output: &Path,
        manifest: &OutputManifest,
        validation: &NativeCompilerValidation,
    ) -> StoreStats {
        cas.store_native(prepared_native_fixture(output, manifest, validation))
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
            match &entry.kind {
                OutputEntryKind::Directory { mode } => {
                    fs::create_dir(&destination).expect("prepared directory");
                    set_exact_mode(&destination, *mode).expect("prepared directory mode");
                }
                OutputEntryKind::File { mode, .. } => {
                    fs::copy(source, &destination).expect("prepared file");
                    set_exact_mode(&destination, *mode).expect("prepared file mode");
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

        base.with_action_key_for_test(action_key)
            .expect("valid revision identity")
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
            cas.root()
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
            cas.publish_native_environment_selector(&key, &names)
                .expect("initial publication"),
            NativeEnvironmentSelectorPublication::Created
        );
        assert_eq!(
            cas.native_environment_selector(&key).expect("published lookup"),
            Some(names.clone())
        );
        assert_eq!(
            cas.publish_native_environment_selector(&key, &names)
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
    fn packed_native_action_is_the_single_reopenable_accounted_authority() {
        let source_base = tempfile::tempdir().expect("source cache base");
        let source = LocalCas::open_at(source_base.path(), 16 * 1024 * 1024).expect("source CAS should open");
        let output = tempfile::tempdir().expect("native output");
        let (manifest, validation) = native_fixture(output.path());
        store_native_fixture(&source, output.path(), &manifest, &validation);
        let NativeActionLookup::Hit(materialized) = source
            .native_action(validation.action_key())
            .expect("materialized action lookup")
        else {
            panic!("source action should be materialized");
        };
        let mut pack = Vec::new();
        let exported = materialized.export_pack(&mut pack).expect("native pack export");
        assert_eq!(exported.content_length, pack.len() as u64);
        let compressed = zstd::stream::encode_all(pack.as_slice(), 1).expect("native pack compression");

        let destination_base = tempfile::tempdir().expect("destination cache base");
        let destination =
            LocalCas::open_at(destination_base.path(), 16 * 1024 * 1024).expect("destination CAS should open");
        let base_action = base_action_key(91);
        let environment_names = Vec::new();
        destination
            .publish_native_environment_selector(&base_action, &environment_names)
            .expect("environment selector publication");
        let authority =
            crate::compiler::native_cache::RemoteAuthorityId::parse(format!("remote-authority-v1-sha256-{:064x}", 7))
                .expect("remote authority");
        let mut staging = destination
            .packed_native_action_staging(PackedNativeActionStagingRequest {
                base_action_key: &base_action,
                environment_names: &environment_names,
                action_key: validation.action_key(),
                result_key: validation.result_key(),
                remote_authority: &authority,
                pack_bytes: pack.len() as u64,
                compressed_bytes: compressed.len() as u64,
            })
            .expect("packed staging");
        staging.writer().write_all(&compressed).expect("compressed payload");
        assert_eq!(
            destination
                .commit_packed_native_action_revalidated(staging, &validation, |_| Ok(()))
                .expect("packed publication"),
            PackedNativeActionPublication::Created
        );
        assert_eq!(
            bounded_optional_directory_entries(&destination.root().join("results"), "test materialized results")
                .expect("materialized results")
                .len(),
            0
        );
        let committed_bytes = destination.status().expect("packed status").committed_result_bytes;
        assert!(committed_bytes > compressed.len() as u64);

        let NativeActionLookup::Packed(packed) = destination
            .native_action(validation.action_key())
            .expect("packed action lookup")
        else {
            panic!("destination action should be packed");
        };
        let mut observed = Vec::new();
        packed
            .compressed_reader()
            .expect("compressed reader")
            .read_to_end(&mut observed)
            .expect("compressed read");
        assert_eq!(observed, compressed);
        drop(packed);
        drop(destination);

        let reopened =
            LocalCas::open_at(destination_base.path(), 16 * 1024 * 1024).expect("destination CAS should reopen");
        assert!(matches!(
            reopened
                .native_action(validation.action_key())
                .expect("reopened lookup"),
            NativeActionLookup::Packed(_)
        ));
        assert_eq!(
            reopened.status().expect("reopened status").committed_result_bytes,
            committed_bytes
        );
        reopened
            .quarantine_packed_native_action(validation.action_key(), "test corruption")
            .expect("packed quarantine");
        assert_eq!(reopened.status().expect("quarantined status").committed_result_bytes, 0);
        assert!(matches!(
            reopened
                .native_action(validation.action_key())
                .expect("quarantined lookup"),
            NativeActionLookup::Miss(_)
        ));
    }

    #[test]
    fn native_link_candidates_are_bounded_canonical_disposable_pointers() {
        let cache = tempfile::tempdir().expect("cache base");
        let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
        let candidate = native_link_candidate_key(1);
        let first = native_action_key(2);
        let second = native_action_key(3);

        assert!(
            cas.native_link_candidates(&candidate)
                .expect("empty candidates")
                .is_empty()
        );
        cas.publish_native_link_candidate(&candidate, &second)
            .expect("second candidate");
        cas.publish_native_link_candidate(&candidate, &first)
            .expect("first candidate");
        cas.publish_native_link_candidate(&candidate, &first)
            .expect("converged candidate");
        assert_eq!(
            cas.native_link_candidates(&candidate).expect("candidate lookup"),
            vec![first, second]
        );
        assert_eq!(cas.status().expect("status").index_files, 2);
    }

    #[test]
    fn native_link_candidates_reject_noncanonical_and_hostile_state() {
        let cache = tempfile::tempdir().expect("cache base");
        let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
        cas.native_link_candidates("compiler-candidate-v4-sha256-1")
            .unwrap_err();

        let candidate = native_link_candidate_key(4);
        let action = native_action_key(5);
        cas.publish_native_link_candidate(&candidate, &action)
            .expect("candidate publication");
        let candidate_hex = validated_id_hex(&candidate, crate::compiler::native_cache::CANDIDATE_SELECTOR_PREFIX)
            .expect("candidate identity");
        let action_hex =
            validated_id_hex(&action, crate::compiler::native_cache::ACTION_KEY_PREFIX).expect("action identity");
        let entry = cas
            .root()
            .join(NATIVE_LINK_CANDIDATE_DIRECTORY)
            .join(candidate_hex)
            .join(format!("{action_hex}.json"));
        fs::write(&entry, b"{}\n").expect("malformed candidate");
        cas.native_link_candidates(&candidate).unwrap_err();
    }

    #[cfg(unix)]
    #[test]
    fn native_link_candidate_lookup_never_follows_a_directory_symlink() {
        use std::os::unix::fs::symlink;

        let cache = tempfile::tempdir().expect("cache base");
        let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
        let candidate = native_link_candidate_key(6);
        let candidate_hex = validated_id_hex(&candidate, crate::compiler::native_cache::CANDIDATE_SELECTOR_PREFIX)
            .expect("candidate identity");
        let outside = tempfile::tempdir().expect("outside directory");
        let directory = cas.root().join(NATIVE_LINK_CANDIDATE_DIRECTORY).join(candidate_hex);
        symlink(outside.path(), &directory).expect("candidate symlink");
        cas.native_link_candidates(&candidate).unwrap_err();
    }

    #[test]
    fn native_environment_selector_divergence_preserves_the_first_binding() {
        let cache = tempfile::tempdir().expect("cache base");
        let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
        let key = base_action_key(2);
        let first = vec!["FIRST".to_string()];
        let second = vec!["SECOND".to_string()];
        cas.publish_native_environment_selector(&key, &first)
            .expect("first publication");

        assert_eq!(
            cas.publish_native_environment_selector(&key, &second)
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
                    cas.publish_native_environment_selector(&key, &names)
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
                        cas.publish_native_environment_selector(&key, names)
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
        cas.publish_native_environment_selector(&key, &first)
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
            hit.validate_environment_selector(&key, first.iter().map(String::as_str))
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
            cas.native_environment_selector(&unsorted_key)
                .expect("missing selector"),
            None
        );

        let malformed_key = base_action_key(5);
        cas.publish_native_environment_selector(&malformed_key, &["VALID".to_string()])
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
        cas.publish_native_environment_selector(&hard_link_key, &["VALID".to_string()])
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
        let revision_count = usize::try_from(REVISIONS).expect("revision count fits usize");
        assert_eq!(action_keys.len(), revision_count);
        assert_eq!(result_keys.len(), revision_count);

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
        assert_eq!(fanout.len(), revision_count);
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
            cas.root
                .join(NATIVE_ACTION_STATE_DIRECTORY)
                .join(format!("{action_hex}.json")),
        )
        .expect("remove authoritative action state");
        assert!(matches!(
            cas.native_action(validation.action_key())
                .expect("missing action lookup"),
            NativeActionLookup::Miss(_)
        ));
    }

    #[test]
    fn native_access_refresh_batches_hot_hits_but_retains_lru_evidence() {
        let cache = tempfile::tempdir().expect("cache base");
        let output = tempfile::tempdir().expect("output root");
        let restore_parent = tempfile::tempdir().expect("restore parent");
        let (manifest, validation) = native_fixture(output.path());
        let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
        store_native_fixture(&cas, output.path(), &manifest, &validation);
        let action_hex = validated_action_key_hex(validation.action_key()).expect("native action key");
        let state = cas
            .root
            .join(NATIVE_ACTION_STATE_DIRECTORY)
            .join(format!("{action_hex}.json"));

        let fresh = fs::metadata(&state)
            .expect("fresh action state")
            .modified()
            .expect("fresh access time");
        let NativeActionLookup::Hit(cached) = cas.native_action(validation.action_key()).expect("fresh lookup") else {
            panic!("fresh native action should be authoritative");
        };
        assert!(matches!(
            cached.restore(&restore_parent.path().join("fresh")),
            NativeCacheLookup::Hit(_)
        ));
        drop(cached);
        assert_eq!(
            fs::metadata(&state)
                .expect("fresh action state after restore")
                .modified()
                .expect("fresh access time after restore"),
            fresh,
            "a hot hit must not rewrite LRU metadata"
        );

        let stale = SystemTime::now()
            .checked_sub(ACCESS_REFRESH_INTERVAL.saturating_mul(2))
            .expect("stale access time");
        OpenOptions::new()
            .write(true)
            .open(&state)
            .expect("stale action state")
            .set_modified(stale)
            .expect("set stale access time");
        let NativeActionLookup::Hit(cached) = cas.native_action(validation.action_key()).expect("stale lookup") else {
            panic!("stale native action should remain authoritative");
        };
        assert!(matches!(
            cached.restore(&restore_parent.path().join("stale")),
            NativeCacheLookup::Hit(_)
        ));
        drop(cached);
        assert!(
            fs::metadata(state)
                .expect("refreshed action state")
                .modified()
                .expect("refreshed access time")
                > stale,
            "a stale successful hit must refresh LRU evidence"
        );
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
        manifest.digest = output_manifest_digest(&manifest.entries).expect("forged manifest digest");

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
        let NativeActionLookup::Miss(miss) = cas.native_action(validation.action_key()).expect("legacy-key lookup")
        else {
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
            cas.root()
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
            cas.root
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

        let error = cas
            .store_native(prepared_native_fixture(
                second_output.path(),
                &second_manifest,
                &second_validation,
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

        let error = cas
            .store_native(prepared_native_fixture(
                second_output.path(),
                &second_manifest,
                &second_validation,
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
        let state: NativeActionState =
            serde_json::from_slice(&fs::read(state_path).expect("unique state")).expect("state");
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

        let NativeActionLookup::Miss(miss) = cas.native_action(validation.action_key()).expect("quarantine lookup")
        else {
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
        remove_owned_root_at(&root).unwrap_err();
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
    fn initialized_cache_open_validates_without_reclaiming_shared_staging() {
        let cache = tempfile::tempdir().expect("cache base");
        let cas = LocalCas::open_at(cache.path(), 1024 * 1024).expect("CAS should open");
        let in_flight = cas.root().join("staging/in-flight");
        fs::write(&in_flight, b"owned by another compiler process").expect("staging sentinel");

        let base = fs::canonicalize(cache.path()).expect("canonical cache base");
        let reopened = LocalCas::open_initialized_at(&base, 1024 * 1024, None).expect("initialized CAS should open");

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
                "cache::cas::tests::guarded_staging_creation_worker",
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
        let cas = LocalCas::open_initialized_at(&base, 1024 * 1024, None).expect("initialized CAS should open");
        let _staging = cas.native_result_staging().expect("guarded staging should be created");
    }

    #[cfg(unix)]
    #[test]
    fn output_modes_accept_executables_created_under_a_group_writable_umask() {
        validate_mode(0o775, false).expect("rustc executable mode");
        validate_mode(0o755, false).expect("ordinary executable mode");
        assert!(
            validate_mode(0o2775, false).is_err(),
            "special mode bits must remain rejected"
        );
        assert!(
            validate_mode(0o275, false).is_err(),
            "the owner must retain read and execute authority"
        );
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
