//! Bounded protocol and one-shot worker for typed remote compiler operations.
//!
//! This module deliberately supports one portable non-linking Rust compilation
//! class with exact metadata-only and metadata-plus-rlib output contracts. It is not an
//! arbitrary command runner. A machine-owned installation may activate
//! either the local process proof or one directly addressed mutual-TLS worker
//! beneath ordinary Cargo. Production automatic placement accepts only the
//! Linux Bubblewrap policy; the process-only runtime remains an explicit
//! qualification path. Neither runtime is a multi-tenant scheduler.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use crate::compiler::native_cache::NativePhaseMeasurement;
use crate::compiler::native_cache::pack::NativeResultStaging;
use crate::error::{RailError, RailResult};
use crate::source::ContentDigest;

const PROTOCOL_VERSION: u32 = 3;
const REQUEST_MAGIC: &[u8; 8] = b"CRXREQ3\0";
const REQUEST_TRAILER: &[u8; 8] = b"CRXEND3\0";
const RESPONSE_MAGIC: &[u8; 8] = b"CRXRES3\0";
const RESPONSE_TRAILER: &[u8; 8] = b"CRXDONE3";
const CANCEL_MAGIC: &[u8; 8] = b"CRXCAN3\0";
const CANCEL_TRAILER: &[u8; 8] = b"CRXCEND3";
const CAPABILITY_MAGIC: &[u8; 8] = b"CRXCAP3\0";
const CAPABILITY_TRAILER: &[u8; 8] = b"CRXCPEN3";
const LEASE_REQUEST_MAGIC: &[u8; 8] = b"CRXLRQ3\0";
const LEASE_REQUEST_TRAILER: &[u8; 8] = b"CRXLRQE3";
const LEASE_GRANT_MAGIC: &[u8; 8] = b"CRXLGT3\0";
const LEASE_GRANT_TRAILER: &[u8; 8] = b"CRXLGTE3";
#[cfg(target_os = "linux")]
const SANDBOX_READY_MAGIC: &[u8; 8] = b"CRXRUN3\0";
#[cfg(target_os = "linux")]
const VIRTUAL_WORKER: &str = "/cargo-rail/exec/v3/worker";
pub(crate) const VIRTUAL_ROOT: &str = "/cargo-rail/exec/v3";
pub(crate) const VIRTUAL_WORKSPACE: &str = "/cargo-rail/exec/v3/workspace";
const VIRTUAL_DEPENDENCIES: &str = "/cargo-rail/exec/v3/dependencies";
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_INPUT_ENTRIES: usize = 16 * 1024;
const MAX_INPUT_PATH_BYTES: usize = 1024 * 1024;
const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TOTAL_INPUT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_OUTPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TOTAL_OUTPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_STREAM_BYTES: u64 = 8 * 1024 * 1024;
const MAX_WALL_TIME_MS: u64 = 2 * 60 * 1000;
const MAX_CPU_PERIOD_MICROS: u64 = 100 * 1000;
const MAX_CPU_QUOTA_MICROS: u64 = 100 * 1000;
const MAX_MEMORY_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_PROCESSES: u32 = 64;
const MAX_SCRATCH_BYTES: u64 = 512 * 1024 * 1024;
const MAX_IDENTITY_BYTES: usize = 128;
const MAX_CRATE_NAME_BYTES: usize = 128;
const MAX_METADATA_BYTES: usize = 128;
const MAX_EXTRA_FILENAME_BYTES: usize = 128;
const MAX_CHECK_CFG_ENTRIES: usize = 128;
const MAX_CHECK_CFG_BYTES: usize = 32 * 1024;
const MAX_CFG_ENTRIES: usize = 1024;
const MAX_CFG_BYTES: usize = 128 * 1024;
const MAX_LINT_ENTRIES: usize = 1024;
const MAX_LINT_BYTES: usize = 128 * 1024;
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(5);
const WORKER_DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(25);
const CLIENT_PROCESS_GRACE: Duration = Duration::from_secs(5);
const NETWORK_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const WORKER_DRAIN_TIMEOUT: Duration = Duration::from_secs(145);
const MAX_NETWORK_CONCURRENCY: u32 = 256;
const PLACEMENT_HISTORY_VERSION: u32 = 2;
const MAX_PLACEMENT_CLASSES: usize = 128;
const MAX_PLACEMENT_SAMPLES: u32 = 1_000_000;
const MIN_PLACEMENT_SAMPLES: u32 = 3;
const MAX_PLACEMENT_SAMPLE_NS: u64 = 24 * 60 * 60 * 1_000_000_000;
const PLACEMENT_HISTORY_MAX_AGE_SECS: u64 = 7 * 24 * 60 * 60;
const PLACEMENT_LOCAL_FLOOR_NS: u64 = 250 * 1_000_000;
const PLACEMENT_MINIMUM_MARGIN_NS: u64 = 25 * 1_000_000;

static LOCAL_LEASE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const fn worker_execution_limits() -> ExecutionLimits {
    ExecutionLimits {
        cpu_period_micros: MAX_CPU_PERIOD_MICROS,
        cpu_quota_micros: MAX_CPU_QUOTA_MICROS,
        max_output_bytes: MAX_OUTPUT_BYTES,
        max_processes: MAX_PROCESSES,
        max_stream_bytes: MAX_STREAM_BYTES,
        memory_bytes: MAX_MEMORY_BYTES,
        scratch_bytes: MAX_SCRATCH_BYTES,
        wall_time_ms: MAX_WALL_TIME_MS,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerCapability {
    architecture: String,
    capability_id: String,
    endianness: String,
    environment_contract: String,
    filesystem_contract: String,
    host_target: String,
    isolation: WorkerIsolation,
    isolation_identity: String,
    operating_system: String,
    operation_classes: Vec<OperationClass>,
    platform_family: String,
    protocol_version: u32,
    resource_limits: ExecutionLimits,
    rustc_content_digest: String,
    rustc_verbose_version: String,
    sysroot_identity: String,
    working_directory_contract: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WorkerIsolation {
    ProcessOnlyUnqualified,
    BubblewrapLinuxV2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OperationClass {
    RustLibrary,
}

struct CapturedWorkerCapability {
    capability: WorkerCapability,
    rustc: PathBuf,
    rustc_generation: Vec<u8>,
    runtime: WorkerRuntime,
    #[cfg(target_os = "linux")]
    sysroot: PathBuf,
}

enum WorkerRuntime {
    ProcessOnly,
    #[cfg(target_os = "linux")]
    Bubblewrap {
        cgroup: CgroupV2Root,
        executable: PathBuf,
        generation: Vec<u8>,
        worker: PathBuf,
        worker_generation: Vec<u8>,
    },
}

#[cfg(target_os = "linux")]
struct CgroupV2Root {
    attempts: PathBuf,
}

#[cfg(target_os = "linux")]
struct CgroupV2Attempt {
    path: PathBuf,
    armed: bool,
}

#[cfg(target_os = "linux")]
#[derive(Default)]
struct CgroupV2Outcome {
    cpu_throttles: u64,
    memory_oom_kills: u64,
    process_limit_hits: u64,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
enum CgroupProbe {
    Cpu,
    Memory,
    Processes,
}

#[cfg(target_os = "linux")]
impl CgroupProbe {
    fn name(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Memory => "memory",
            Self::Processes => "processes",
        }
    }

    fn limits(self) -> ExecutionLimits {
        let mut limits = worker_execution_limits();
        limits.scratch_bytes = 16 * 1024 * 1024;
        match self {
            Self::Cpu => {
                limits.cpu_quota_micros = 1_000;
                limits.memory_bytes = 128 * 1024 * 1024;
                limits.max_processes = 4;
            }
            Self::Memory => {
                limits.memory_bytes = 64 * 1024 * 1024;
                limits.max_processes = 4;
            }
            Self::Processes => {
                limits.memory_bytes = 256 * 1024 * 1024;
                limits.max_processes = 4;
            }
        }
        limits
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseRequest {
    action_id: String,
    capability_id: String,
    client_nonce: String,
    protocol_version: u32,
    workload_identity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseGrant {
    action_id: String,
    capability_id: String,
    lease_id: String,
    protocol_version: u32,
    workload_identity: String,
}

/// Machine-owned address, files, and peer name for one mutually authenticated
/// worker. The endpoint travels with the credentials that authorize it, so a
/// caller cannot pair one worker's address with another worker's identity.
pub(crate) struct MutualTlsClientIdentity<'a> {
    pub(crate) endpoint: &'a str,
    pub(crate) server_name: &'a str,
    pub(crate) worker_capability_id: &'a str,
    pub(crate) authority_certificate: &'a Path,
    pub(crate) client_certificate: &'a Path,
    pub(crate) client_private_key: &'a Path,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionRequest {
    action_id: String,
    capability_id: String,
    inputs: Vec<InputFrame>,
    lease_id: String,
    limits: ExecutionLimits,
    operation: RustLibraryOperation,
    protocol_version: u32,
    workload_identity: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionLimits {
    cpu_period_micros: u64,
    cpu_quota_micros: u64,
    max_output_bytes: u64,
    max_processes: u32,
    max_stream_bytes: u64,
    memory_bytes: u64,
    scratch_bytes: u64,
    wall_time_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RustLibraryOperation {
    cap_lints: Option<String>,
    cargo_json_diagnostics: bool,
    check_cfg: Vec<String>,
    codegen: RustLibraryCodegen,
    color: Option<String>,
    crate_name: String,
    crate_type: RustLibraryCrateType,
    cfg: Vec<String>,
    dependencies: Vec<RustLibraryDependency>,
    diagnostic_width: Option<u32>,
    dep_info_name: String,
    edition: String,
    emission: RustLibraryEmission,
    extra_filename: String,
    metadata: String,
    metadata_name: String,
    lints: Vec<RustLibraryLint>,
    operation_class: OperationClass,
    output_relative_directory: String,
    output_dependency_search: bool,
    rlib_name: Option<String>,
    source_virtual_path: String,
    test_mode: bool,
    toolchain_proc_macro: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RustLibraryDependency {
    extern_name: String,
    virtual_path: String,
}

/// Closed rustc code-generation options supported by the first portable
/// library operation. Path-bearing and tool-selecting options are deliberately
/// absent from this representation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RustLibraryCodegen {
    pub(crate) codegen_units: Option<u32>,
    pub(crate) debuginfo: Option<String>,
    pub(crate) debug_assertions: Option<bool>,
    pub(crate) embed_bitcode: Option<bool>,
    pub(crate) linker_plugin_lto: Option<bool>,
    pub(crate) lto: Option<String>,
    pub(crate) opt_level: Option<String>,
    pub(crate) overflow_checks: Option<bool>,
    pub(crate) panic: Option<String>,
    pub(crate) prefer_dynamic: Option<bool>,
    pub(crate) split_debuginfo: Option<String>,
    pub(crate) strip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RustLibraryLint {
    pub(crate) level: RustLibraryLintLevel,
    pub(crate) name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RustLibraryLintLevel {
    Allow,
    Deny,
    Forbid,
    Warn,
}

/// Machine-independent rustc options retained by the portable action.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RustLibraryExecutionOptions {
    pub(crate) cap_lints: Option<String>,
    pub(crate) cargo_json_diagnostics: bool,
    pub(crate) check_cfg: Vec<String>,
    pub(crate) codegen: RustLibraryCodegen,
    pub(crate) color: Option<String>,
    pub(crate) cfg: Vec<String>,
    pub(crate) diagnostic_width: Option<u32>,
    pub(crate) lints: Vec<RustLibraryLint>,
    pub(crate) output_dependency_search: bool,
}

/// Named inputs for one portable Rust library operation.
///
/// Keeping these fields named prevents crate metadata and relative paths from
/// being exchanged accidentally at the compiler-observation boundary.
pub(crate) struct RustLibraryCandidateInput {
    pub(crate) crate_name: String,
    pub(crate) crate_type: String,
    pub(crate) dep_info_name: String,
    pub(crate) edition: String,
    pub(crate) emission: RustLibraryEmission,
    pub(crate) metadata: String,
    pub(crate) metadata_name: String,
    pub(crate) extra_filename: String,
    pub(crate) output_relative_directory: String,
    pub(crate) source_relative_path: String,
    pub(crate) test_mode: bool,
    pub(crate) toolchain_proc_macro: bool,
    pub(crate) rlib_name: Option<String>,
    pub(crate) options: RustLibraryExecutionOptions,
}

/// One exact regular source file in the captured workspace namespace.
pub(crate) struct RustLibrarySourceInput {
    pub(crate) bytes: u64,
    pub(crate) content_digest: String,
    pub(crate) path: PathBuf,
    pub(crate) repository_relative_path: String,
}

/// One exact prebuilt Rust dependency admitted to portable execution.
pub(crate) struct RustLibraryDependencyInput {
    pub(crate) artifact_name: String,
    pub(crate) bytes: u64,
    pub(crate) content_digest: String,
    pub(crate) extern_name: String,
    pub(crate) path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RustLibraryCrateType {
    Bin,
    Cdylib,
    Dylib,
    Lib,
    ProcMacro,
    Rlib,
    Staticlib,
}

/// Closed compiler-output contracts supported by portable Rust libraries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RustLibraryEmission {
    Metadata,
    MetadataAndLink,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputFrame {
    bytes: u64,
    content_digest: String,
    kind: InputKind,
    virtual_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum InputKind {
    Dependency,
    Source,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionResponse {
    action_id: String,
    capability_id: String,
    frames: Vec<ResponseFrame>,
    lease_id: String,
    protocol_version: u32,
    reason: Option<String>,
    status: ExecutionStatus,
    termination: Option<CompilerTermination>,
    worker_timing: WorkerPhaseTiming,
    workload_identity: String,
}

/// Source-free worker timing returned with the attempt it measures.
///
/// This is advisory evidence only. It never enters action identity, result
/// identity, admission, or placement authority.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerPhaseTiming {
    /// Connection acceptance through the start of the worker execution interval.
    queue_ns: u64,
    input_ns: u64,
    compiler_ns: u64,
    result_encode_ns: u64,
    /// Worker-owned execution interval containing input, compiler, and result encoding.
    elapsed_ns: u64,
    source_bytes: u64,
    result_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ExecutionStatus {
    Success,
    CompilerFailed,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum CompilerTermination {
    Exit { code: i32 },
    Signal { signal: i32 },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseFrame {
    bytes: u64,
    content_digest: String,
    mode: u32,
    slot: ResponseSlot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ResponseSlot {
    DepInfo,
    Metadata,
    Rlib,
    Stderr,
    Stdout,
}

impl ResponseSlot {
    const fn file_name(self) -> &'static str {
        match self {
            Self::DepInfo => "dep-info",
            Self::Metadata => "metadata",
            Self::Rlib => "rlib",
            Self::Stderr => "stderr",
            Self::Stdout => "stdout",
        }
    }

    const fn is_stream(self) -> bool {
        matches!(self, Self::Stderr | Self::Stdout)
    }
}

struct RequestEnvelope {
    inputs: BTreeMap<String, PathBuf>,
    request: ExecutionRequest,
    _staging: tempfile::TempDir,
}

enum ResponsePayload {
    File(PathBuf),
    Bytes(Vec<u8>),
}

struct PreparedResponseFrame {
    descriptor: ResponseFrame,
    payload: ResponsePayload,
}

pub(crate) struct StagedExecutionResult {
    staging: NativeResultStaging,
    frames: BTreeMap<ResponseSlot, PathBuf>,
    descriptors: BTreeMap<ResponseSlot, ResponseFrame>,
    inputs: Vec<InputFrame>,
    operation: RustLibraryOperation,
}

impl StagedExecutionResult {
    pub(crate) fn frame(&self, slot: DistributedResultSlot) -> Option<&Path> {
        self.frames.get(&slot.into()).map(PathBuf::as_path)
    }

    pub(crate) fn staging_path(&self) -> &Path {
        self.staging.path()
    }

    pub(crate) fn requires_durable_handoff(&self) -> bool {
        self.staging.requires_durable_handoff()
    }

    pub(crate) fn binds_candidate(&self, candidate: &RustLibraryCandidate) -> bool {
        self.operation == candidate.operation && self.inputs == candidate.input_frames()
    }

    pub(crate) fn verified_frame(&self, slot: DistributedResultSlot) -> RailResult<(&Path, &str, u64, u32)> {
        let slot = ResponseSlot::from(slot);
        let path = self
            .frames
            .get(&slot)
            .ok_or_else(|| RailError::message("distributed execution result slot is unavailable"))?;
        let descriptor = self
            .descriptors
            .get(&slot)
            .ok_or_else(|| RailError::message("distributed execution result descriptor is unavailable"))?;
        let metadata = fs::symlink_metadata(path)?;
        if !path.starts_with(self.staging.path())
            || !metadata.is_file()
            || crate::utils::is_symlink_or_reparse(&metadata)
            || metadata.len() != descriptor.bytes
            || digest_file(path, descriptor.bytes)? != descriptor.content_digest
        {
            return Err(RailError::message(
                "distributed execution result changed after private staging",
            ));
        }
        Ok((path, &descriptor.content_digest, descriptor.bytes, descriptor.mode))
    }

    pub(crate) fn read_verified_frame(&self, slot: DistributedResultSlot) -> RailResult<Vec<u8>> {
        let (path, expected_digest, expected_bytes, _) = self.verified_frame(slot)?;
        let capacity = usize::try_from(expected_bytes)
            .map_err(|_| RailError::message("distributed execution result exceeds this platform"))?;
        let mut bytes = Vec::with_capacity(capacity);
        File::open(path)?
            .take(expected_bytes.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != expected_bytes || digest_bytes(&bytes) != expected_digest {
            return Err(RailError::message(
                "distributed execution result changed while reading private staging",
            ));
        }
        Ok(bytes)
    }

    pub(crate) fn move_verified_frame_to(
        &mut self,
        slot: DistributedResultSlot,
        destination: &Path,
    ) -> RailResult<(String, u64, u32)> {
        let response_slot = ResponseSlot::from(slot);
        let (source, expected_digest, expected_bytes, mode) = {
            let (source, expected_digest, expected_bytes, mode) = self.verified_frame(slot)?;
            (source.to_path_buf(), expected_digest.to_string(), expected_bytes, mode)
        };
        if !destination.starts_with(self.staging.path()) || fs::symlink_metadata(destination).is_ok() {
            return Err(RailError::message(
                "distributed execution native staging destination is unauthorized",
            ));
        }
        let parent = destination
            .parent()
            .ok_or_else(|| RailError::message("distributed execution native staging slot has no parent"))?;
        fs::create_dir_all(parent)?;
        fs::rename(&source, destination)?;
        if digest_file(destination, expected_bytes)? != expected_digest {
            return Err(RailError::message(
                "distributed execution result changed while entering native staging",
            ));
        }
        self.frames.remove(&response_slot);
        self.descriptors.remove(&response_slot);
        Ok((expected_digest, expected_bytes, mode))
    }

    pub(crate) fn into_native_staging(self) -> RailResult<NativeResultStaging> {
        for path in self.frames.values() {
            if !path.starts_with(self.staging.path().join("distributed")) {
                return Err(RailError::message(
                    "distributed execution result escaped its disposable staging namespace",
                ));
            }
            fs::remove_file(path)?;
        }
        let distributed = self.staging.path().join("distributed");
        if fs::read_dir(&distributed)?.next().is_some() {
            return Err(RailError::message(
                "distributed execution staging contains an unauthorized entry",
            ));
        }
        fs::remove_dir(distributed)?;
        Ok(self.staging)
    }

    #[cfg(test)]
    pub(crate) fn from_test_frames(
        candidate: &RustLibraryCandidate,
        dep_info: &[u8],
        metadata: &[u8],
        rlib: &[u8],
        stdout: &[u8],
        stderr: &[u8],
    ) -> RailResult<Self> {
        let staging = NativeResultStaging::temporary()?;
        let distributed = staging.path().join("distributed");
        fs::create_dir(&distributed)?;
        let mut frames = BTreeMap::new();
        let mut descriptors = BTreeMap::new();
        let mut contents = vec![
            (ResponseSlot::DepInfo, dep_info, 0o644),
            (ResponseSlot::Metadata, metadata, 0o644),
        ];
        if candidate.operation.emission == RustLibraryEmission::MetadataAndLink {
            contents.push((ResponseSlot::Rlib, rlib, 0o644));
        }
        contents.extend([(ResponseSlot::Stderr, stderr, 0), (ResponseSlot::Stdout, stdout, 0)]);
        for (slot, bytes, mode) in contents {
            let path = distributed.join(slot.file_name());
            write_private_file(&path, bytes)?;
            frames.insert(slot, path);
            descriptors.insert(
                slot,
                ResponseFrame {
                    bytes: bytes.len() as u64,
                    content_digest: digest_bytes(bytes),
                    mode,
                    slot,
                },
            );
        }
        Ok(Self {
            staging,
            frames,
            descriptors,
            inputs: candidate.input_frames(),
            operation: candidate.operation.clone(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DistributedResultSlot {
    DepInfo,
    Metadata,
    Rlib,
    Stderr,
    Stdout,
}

impl From<DistributedResultSlot> for ResponseSlot {
    fn from(slot: DistributedResultSlot) -> Self {
        match slot {
            DistributedResultSlot::DepInfo => Self::DepInfo,
            DistributedResultSlot::Metadata => Self::Metadata,
            DistributedResultSlot::Rlib => Self::Rlib,
            DistributedResultSlot::Stderr => Self::Stderr,
            DistributedResultSlot::Stdout => Self::Stdout,
        }
    }
}

/// One closed portable operation derived from an already captured native action.
pub(crate) struct RustLibraryCandidate {
    inputs: Vec<CandidateInput>,
    operation: RustLibraryOperation,
}

struct CandidateInput {
    frame: InputFrame,
    payload: CandidateInputPayload,
}

enum CandidateInputPayload {
    Bytes(Vec<u8>),
    File(PathBuf),
}

/// Non-authoritative cost class retained by the machine installation.
///
/// It contains no source bytes, paths, crate names, or action digests. A bad
/// observation may only keep work local or waste a remote attempt; it can
/// never authorize a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlacementObservation {
    key: String,
}

#[derive(Serialize)]
struct PlacementShape<'a> {
    capability_identity: &'a str,
    cargo_json_diagnostics: bool,
    check_cfg_digest: String,
    codegen: &'a RustLibraryCodegen,
    crate_type: RustLibraryCrateType,
    edition: &'a str,
    emission: RustLibraryEmission,
    endpoint_digest: String,
    operation_class: OperationClass,
    output_dependency_search: bool,
    semantic_options_digest: String,
    source_size_class: u32,
    version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlacementHistory {
    entries: BTreeMap<String, PlacementHistoryEntry>,
    version: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlacementHistoryEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    local: Option<PlacementEstimate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote: Option<PlacementEstimate>,
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    remote_failures: u8,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    retry_after_unix_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlacementEstimate {
    deviation_ns: u64,
    mean_ns: u64,
    samples: u32,
    updated_unix_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlacementDecision {
    Delegate,
    Local(&'static str),
}

/// Aggregate, source-free observability for one installation's cost model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PlacementHistoryStatus {
    pub(crate) state: &'static str,
    pub(crate) classes: u64,
    pub(crate) local_classes: u64,
    pub(crate) local_observations: u64,
    pub(crate) remote_classes: u64,
    pub(crate) remote_observations: u64,
    pub(crate) remote_failures: u64,
    pub(crate) active_backoffs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) newest_observation_unix_secs: Option<u64>,
}

impl RustLibraryCandidate {
    fn input_frames(&self) -> Vec<InputFrame> {
        self.inputs.iter().map(|input| input.frame.clone()).collect()
    }

    fn input_bytes(&self) -> u64 {
        self.inputs
            .iter()
            .fold(0_u64, |total, input| total.saturating_add(input.frame.bytes))
    }

    pub(crate) fn same_normalized_operation(&self, other: &Self) -> bool {
        self.operation == other.operation && self.input_frames() == other.input_frames()
    }

    pub(crate) fn placement_observation(
        &self,
        capability_identity: &str,
        endpoint: &str,
    ) -> RailResult<PlacementObservation> {
        let check_cfg_digest = digest_bytes(&canonical_json(&self.operation.check_cfg)?);
        let semantic_options_digest = digest_bytes(&canonical_json(&(
            &self.operation.cap_lints,
            &self.operation.cfg,
            &self.operation.color,
            &self.operation.diagnostic_width,
            &self.operation.lints,
            &self.operation.dependencies,
        ))?);
        let endpoint_digest = digest_bytes(endpoint.as_bytes());
        let shape = PlacementShape {
            capability_identity,
            cargo_json_diagnostics: self.operation.cargo_json_diagnostics,
            check_cfg_digest,
            codegen: &self.operation.codegen,
            crate_type: self.operation.crate_type,
            edition: &self.operation.edition,
            emission: self.operation.emission,
            endpoint_digest,
            operation_class: self.operation.operation_class,
            output_dependency_search: self.operation.output_dependency_search,
            semantic_options_digest,
            source_size_class: usize::BITS.saturating_sub(1).saturating_sub(
                usize::try_from(self.input_bytes())
                    .unwrap_or(usize::MAX)
                    .leading_zeros(),
            ),
            version: PLACEMENT_HISTORY_VERSION,
        };
        Ok(PlacementObservation {
            key: format!(
                "placement-class-v1:sha256:{}",
                ContentDigest::sha256(&canonical_json(&shape)?)
            ),
        })
    }

    pub(crate) fn new(input: RustLibraryCandidateInput, source: Vec<u8>) -> RailResult<Self> {
        if source.len() as u64 > MAX_INPUT_BYTES {
            return Err(RailError::message(
                "distributed Rust library source is outside its byte bound",
            ));
        }
        let source_relative_path = input.source_relative_path.clone();
        Self::from_inputs(
            input,
            vec![CandidateInput {
                frame: InputFrame {
                    bytes: source.len() as u64,
                    content_digest: digest_bytes(&source),
                    kind: InputKind::Source,
                    virtual_path: virtual_source_path(&source_relative_path),
                },
                payload: CandidateInputPayload::Bytes(source),
            }],
            Vec::new(),
        )
    }

    pub(crate) fn from_captured_inputs(
        input: RustLibraryCandidateInput,
        mut sources: Vec<RustLibrarySourceInput>,
        dependencies: Vec<RustLibraryDependencyInput>,
    ) -> RailResult<Self> {
        sources.sort_unstable_by(|left, right| left.repository_relative_path.cmp(&right.repository_relative_path));
        let mut captured = Vec::with_capacity(sources.len().saturating_add(dependencies.len()));
        for source in sources {
            captured.push(CandidateInput {
                frame: InputFrame {
                    bytes: source.bytes,
                    content_digest: source.content_digest,
                    kind: InputKind::Source,
                    virtual_path: virtual_source_path(&source.repository_relative_path),
                },
                payload: CandidateInputPayload::File(source.path),
            });
        }
        let mut portable_dependencies = Vec::with_capacity(dependencies.len());
        for (index, dependency) in dependencies.into_iter().enumerate() {
            validate_dependency_artifact_name(&dependency.artifact_name)?;
            validate_extern_name(&dependency.extern_name)?;
            let virtual_path = format!("{VIRTUAL_DEPENDENCIES}/{index:05}/{}", dependency.artifact_name);
            portable_dependencies.push(RustLibraryDependency {
                extern_name: dependency.extern_name,
                virtual_path: virtual_path.clone(),
            });
            captured.push(CandidateInput {
                frame: InputFrame {
                    bytes: dependency.bytes,
                    content_digest: dependency.content_digest,
                    kind: InputKind::Dependency,
                    virtual_path,
                },
                payload: CandidateInputPayload::File(dependency.path),
            });
        }
        captured.sort_unstable_by(|left, right| left.frame.virtual_path.cmp(&right.frame.virtual_path));
        Self::from_inputs(input, captured, portable_dependencies)
    }

    fn from_inputs(
        input: RustLibraryCandidateInput,
        inputs: Vec<CandidateInput>,
        dependencies: Vec<RustLibraryDependency>,
    ) -> RailResult<Self> {
        let crate_type = match input.crate_type.as_str() {
            "bin" => RustLibraryCrateType::Bin,
            "cdylib" => RustLibraryCrateType::Cdylib,
            "dylib" => RustLibraryCrateType::Dylib,
            "lib" => RustLibraryCrateType::Lib,
            "proc-macro" => RustLibraryCrateType::ProcMacro,
            "rlib" => RustLibraryCrateType::Rlib,
            "staticlib" => RustLibraryCrateType::Staticlib,
            _ => return Err(RailError::message("distributed Rust library crate type is unsupported")),
        };
        let source_virtual_path = virtual_source_path(&input.source_relative_path);
        let candidate = Self {
            inputs,
            operation: RustLibraryOperation {
                cap_lints: input.options.cap_lints,
                cargo_json_diagnostics: input.options.cargo_json_diagnostics,
                check_cfg: input.options.check_cfg,
                codegen: input.options.codegen,
                color: input.options.color,
                crate_name: input.crate_name,
                crate_type,
                cfg: input.options.cfg,
                dependencies,
                diagnostic_width: input.options.diagnostic_width,
                dep_info_name: input.dep_info_name,
                edition: input.edition,
                emission: input.emission,
                extra_filename: input.extra_filename,
                metadata: input.metadata,
                metadata_name: input.metadata_name,
                lints: input.options.lints,
                operation_class: OperationClass::RustLibrary,
                output_relative_directory: input.output_relative_directory,
                output_dependency_search: input.options.output_dependency_search,
                rlib_name: input.rlib_name,
                source_virtual_path,
                test_mode: input.test_mode,
                toolchain_proc_macro: input.toolchain_proc_macro,
            },
        };
        validate_operation(&candidate.operation)?;
        validate_inputs(&candidate.input_frames(), &candidate.operation)?;
        Ok(candidate)
    }

    /// Reconstruct the same normalized operation used by the worker for the one
    /// legal local fallback before any Cargo-visible effect.
    pub(crate) fn normalized_local_command(
        &self,
        rustc: &OsStr,
        workspace: &Path,
        temporary: &Path,
    ) -> RailResult<Command> {
        let workspace = crate::utils::canonicalize_existing(workspace)?;
        let source_relative = source_relative_path(&self.operation.source_virtual_path)
            .map(Path::new)
            .ok_or_else(|| RailError::message("distributed local fallback source path is invalid"))?;
        let source = workspace.join(source_relative);
        let output_directory = workspace.join(&self.operation.output_relative_directory);
        let output_directory = crate::utils::canonicalize_existing(&output_directory)?;
        let source_metadata = fs::symlink_metadata(&source)?;
        if !source_metadata.is_file()
            || crate::utils::is_symlink_or_reparse(&source_metadata)
            || self
                .inputs
                .iter()
                .find(|input| input.frame.virtual_path == self.operation.source_virtual_path)
                .is_none_or(|input| {
                    source_metadata.len() != input.frame.bytes
                        || digest_file(&source, source_metadata.len()).ok().as_deref()
                            != Some(&input.frame.content_digest)
                })
        {
            return Err(RailError::message(
                "distributed local fallback source changed after candidate capture",
            ));
        }
        let outputs = output_paths(&self.operation, &output_directory)?;
        let dependencies = self
            .operation
            .dependencies
            .iter()
            .map(|dependency| {
                let input = self
                    .inputs
                    .iter()
                    .find(|input| input.frame.virtual_path == dependency.virtual_path)
                    .ok_or_else(|| RailError::message("distributed dependency input is unavailable"))?;
                let CandidateInputPayload::File(path) = &input.payload else {
                    return Err(RailError::message("distributed dependency has no local path authority"));
                };
                Ok((dependency.extern_name.as_str(), path.as_path()))
            })
            .collect::<RailResult<Vec<_>>>()?;
        compiler_command(CompilerCommandInput {
            rustc,
            operation: &self.operation,
            source_relative,
            outputs: &outputs,
            workspace: &workspace,
            temporary,
            dependencies: &dependencies,
            inherit_environment: true,
        })
    }
}

fn virtual_source_path(relative: &str) -> String {
    format!("{VIRTUAL_WORKSPACE}/{relative}")
}

pub(crate) fn automatic_placement(
    receipt: &crate::cache::installation::InstallationReceipt,
    observation: &PlacementObservation,
) -> PlacementDecision {
    let now = unix_seconds();
    let history = crate::cache::installation::read_distributed_placement_history(receipt)
        .ok()
        .flatten()
        .and_then(|bytes| decode_placement_history(&bytes).ok());
    history.map_or(
        PlacementDecision::Local("distributed_cost_history_unavailable"),
        |history| placement_decision_at(&history, observation, now),
    )
}

pub(crate) fn placement_history_status(
    receipt: &crate::cache::installation::InstallationReceipt,
) -> RailResult<PlacementHistoryStatus> {
    let Some(bytes) = crate::cache::installation::read_distributed_placement_history(receipt)? else {
        return Ok(PlacementHistoryStatus {
            state: "empty",
            classes: 0,
            local_classes: 0,
            local_observations: 0,
            remote_classes: 0,
            remote_observations: 0,
            remote_failures: 0,
            active_backoffs: 0,
            newest_observation_unix_secs: None,
        });
    };
    let history = decode_placement_history(&bytes)?;
    let now = unix_seconds();
    let mut status = PlacementHistoryStatus {
        state: "ready",
        classes: history.entries.len() as u64,
        local_classes: 0,
        local_observations: 0,
        remote_classes: 0,
        remote_observations: 0,
        remote_failures: 0,
        active_backoffs: 0,
        newest_observation_unix_secs: None,
    };
    for entry in history.entries.values() {
        if let Some(local) = entry.local {
            status.local_classes = status.local_classes.saturating_add(1);
            status.local_observations = status.local_observations.saturating_add(u64::from(local.samples));
        }
        if let Some(remote) = entry.remote {
            status.remote_classes = status.remote_classes.saturating_add(1);
            status.remote_observations = status.remote_observations.saturating_add(u64::from(remote.samples));
        }
        status.remote_failures = status.remote_failures.saturating_add(u64::from(entry.remote_failures));
        status.active_backoffs = status
            .active_backoffs
            .saturating_add(u64::from(entry.retry_after_unix_secs > now));
        let updated = entry_updated(entry);
        status.newest_observation_unix_secs = Some(
            status
                .newest_observation_unix_secs
                .map_or(updated, |current| current.max(updated)),
        );
    }
    Ok(status)
}

pub(crate) fn record_local_placement(
    receipt: &crate::cache::installation::InstallationReceipt,
    observation: &PlacementObservation,
    elapsed: Duration,
) {
    drop(update_placement_history(receipt, observation, |entry, now| {
        observe_estimate(&mut entry.local, elapsed, now);
    }));
}

pub(crate) fn record_remote_placement(
    receipt: &crate::cache::installation::InstallationReceipt,
    observation: &PlacementObservation,
    elapsed: Duration,
    success: bool,
) {
    drop(update_placement_history(receipt, observation, |entry, now| {
        if success {
            observe_estimate(&mut entry.remote, elapsed, now);
            entry.remote_failures = 0;
            entry.retry_after_unix_secs = 0;
        } else {
            entry.remote_failures = entry.remote_failures.saturating_add(1).min(16);
            let backoff = match entry.remote_failures {
                0 => 0,
                1 => 1,
                2 => 5,
                3 => 30,
                4 => 120,
                5 => 600,
                _ => 3_600,
            };
            entry.retry_after_unix_secs = now.saturating_add(backoff);
        }
    }));
}

fn placement_decision_at(
    history: &PlacementHistory,
    observation: &PlacementObservation,
    now: u64,
) -> PlacementDecision {
    let Some(entry) = history.entries.get(&observation.key) else {
        return PlacementDecision::Local("distributed_cost_history_unavailable");
    };
    if entry.retry_after_unix_secs > now {
        return PlacementDecision::Local("distributed_worker_backoff_active");
    }
    let (Some(local), Some(remote)) = (entry.local, entry.remote) else {
        return PlacementDecision::Local("distributed_cost_history_incomplete");
    };
    if local.samples < MIN_PLACEMENT_SAMPLES || remote.samples < MIN_PLACEMENT_SAMPLES {
        return PlacementDecision::Local("distributed_cost_history_insufficient");
    }
    if estimate_is_stale(local, now) || estimate_is_stale(remote, now) {
        return PlacementDecision::Local("distributed_cost_history_stale");
    }
    if local.mean_ns < PLACEMENT_LOCAL_FLOOR_NS {
        return PlacementDecision::Local("distributed_local_cost_below_floor");
    }
    let local_uncertainty = local.deviation_ns.saturating_mul(2).max(local.mean_ns / 20);
    let remote_uncertainty = remote.deviation_ns.saturating_mul(2).max(remote.mean_ns / 20);
    let local_lower = local.mean_ns.saturating_sub(local_uncertainty);
    let remote_upper = remote.mean_ns.saturating_add(remote_uncertainty);
    let safety_margin = PLACEMENT_MINIMUM_MARGIN_NS.max(remote_upper / 10);
    if local_lower > remote_upper.saturating_add(safety_margin) {
        PlacementDecision::Delegate
    } else {
        PlacementDecision::Local("distributed_predicted_cost_not_lower")
    }
}

fn estimate_is_stale(estimate: PlacementEstimate, now: u64) -> bool {
    estimate.updated_unix_secs > now || now.saturating_sub(estimate.updated_unix_secs) > PLACEMENT_HISTORY_MAX_AGE_SECS
}

fn observe_estimate(estimate: &mut Option<PlacementEstimate>, elapsed: Duration, now: u64) {
    let sample = u64::try_from(elapsed.as_nanos())
        .unwrap_or(u64::MAX)
        .clamp(1, MAX_PLACEMENT_SAMPLE_NS);
    match estimate {
        Some(estimate) => {
            let error = estimate.mean_ns.abs_diff(sample);
            estimate.mean_ns = weighted_quarter(estimate.mean_ns, sample);
            estimate.deviation_ns = weighted_quarter(estimate.deviation_ns, error);
            estimate.samples = estimate.samples.saturating_add(1).min(MAX_PLACEMENT_SAMPLES);
            estimate.updated_unix_secs = now;
        }
        None => {
            *estimate = Some(PlacementEstimate {
                deviation_ns: 0,
                mean_ns: sample,
                samples: 1,
                updated_unix_secs: now,
            });
        }
    }
}

fn weighted_quarter(previous: u64, sample: u64) -> u64 {
    u64::try_from((u128::from(previous) * 3 + u128::from(sample)) / 4).unwrap_or(u64::MAX)
}

fn update_placement_history(
    receipt: &crate::cache::installation::InstallationReceipt,
    observation: &PlacementObservation,
    update: impl FnOnce(&mut PlacementHistoryEntry, u64),
) -> RailResult<()> {
    let now = unix_seconds();
    crate::cache::installation::update_distributed_placement_history(receipt, |current| {
        let mut history = match current {
            Some(bytes) => match decode_placement_history(bytes) {
                Ok(history) => history,
                Err(_) => PlacementHistory {
                    entries: BTreeMap::new(),
                    version: PLACEMENT_HISTORY_VERSION,
                },
            },
            None => PlacementHistory {
                entries: BTreeMap::new(),
                version: PLACEMENT_HISTORY_VERSION,
            },
        };
        update(history.entries.entry(observation.key.clone()).or_default(), now);
        while history.entries.len() > MAX_PLACEMENT_CLASSES {
            let Some(oldest) = history
                .entries
                .iter()
                .min_by_key(|(key, entry)| (entry_updated(entry), key.as_str()))
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            history.entries.remove(&oldest);
        }
        encode_placement_history(&history)
    })
}

fn entry_updated(entry: &PlacementHistoryEntry) -> u64 {
    entry
        .local
        .into_iter()
        .chain(entry.remote)
        .map(|estimate| estimate.updated_unix_secs)
        .chain(std::iter::once(entry.retry_after_unix_secs))
        .max()
        .unwrap_or(0)
}

fn encode_placement_history(history: &PlacementHistory) -> RailResult<Vec<u8>> {
    validate_placement_history(history)?;
    let mut bytes = canonical_json(history)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn decode_placement_history(bytes: &[u8]) -> RailResult<PlacementHistory> {
    let encoded = bytes
        .strip_suffix(b"\n")
        .ok_or_else(|| RailError::message("distributed placement history is not newline terminated"))?;
    let history: PlacementHistory = serde_json::from_slice(encoded)
        .map_err(|_| RailError::message("distributed placement history is malformed"))?;
    validate_placement_history(&history)?;
    if encode_placement_history(&history)? != bytes {
        return Err(RailError::message("distributed placement history is not canonical"));
    }
    Ok(history)
}

fn validate_placement_history(history: &PlacementHistory) -> RailResult<()> {
    let valid_estimate = |estimate: PlacementEstimate| {
        (1..=MAX_PLACEMENT_SAMPLES).contains(&estimate.samples)
            && (1..=MAX_PLACEMENT_SAMPLE_NS).contains(&estimate.mean_ns)
            && estimate.deviation_ns <= MAX_PLACEMENT_SAMPLE_NS
            && estimate.updated_unix_secs > 0
    };
    if history.version != PLACEMENT_HISTORY_VERSION
        || history.entries.len() > MAX_PLACEMENT_CLASSES
        || history.entries.iter().any(|(key, entry)| {
            !valid_identity(key, "placement-class-v1:sha256:")
                || entry.local.is_some_and(|estimate| !valid_estimate(estimate))
                || entry.remote.is_some_and(|estimate| !valid_estimate(estimate))
                || entry.remote_failures > 16
        })
    {
        return Err(RailError::message("distributed placement history is invalid"));
    }
    Ok(())
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

const fn is_zero_u8(value: &u8) -> bool {
    *value == 0
}

const fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

/// Source-free client phase timing for one distributed attempt.
///
/// Every field is a phase count plus a nanosecond total. The snapshot carries
/// no source bytes, paths, crate names, digests, endpoints, or peer identity,
/// so it is safe to retain as ordinary benchmark evidence. It is advisory
/// measurement only: it never enters action, result, admission, or placement
/// authority.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub(crate) struct DistributedTiming {
    /// Whole client attempt up to the local admission boundary.
    attempt: NativePhaseMeasurement,
    /// Independent local capture of the compiler capability Cargo selected.
    capability_capture: NativePhaseMeasurement,
    /// Transport connection establishment.
    connect: NativePhaseMeasurement,
    /// Reading the machine-owned identity files and building the TLS client.
    tls_setup: NativePhaseMeasurement,
    /// TLS 1.3 mutual authentication.
    handshake: NativePhaseMeasurement,
    /// Worker capability frame read and acceptance.
    capability_exchange: NativePhaseMeasurement,
    /// One-use lease request and grant validation.
    lease: NativePhaseMeasurement,
    /// Execution request header and source frame write.
    source_transfer: NativePhaseMeasurement,
    /// Client response wait ending at the decoded response header.
    remote_execution: NativePhaseMeasurement,
    /// Digest-verified result frames written into private staging.
    result_transfer: NativePhaseMeasurement,
    /// Live revalidation and L1 admission before the first visible effect.
    admission: NativePhaseMeasurement,
    worker: WorkerPhaseTiming,
    source_bytes: u64,
    result_bytes: u64,
}

impl DistributedTiming {
    fn record_attempt(&mut self, started: Instant) {
        self.attempt.record(started);
    }

    fn absorb_response(&mut self, response: ResponseTiming) {
        self.remote_execution = response.remote_execution;
        self.result_transfer = response.result_transfer;
        self.worker = response.worker;
        self.result_bytes = response.result_bytes;
    }

    /// Record local revalidation and L1 admission from the native cache side.
    pub(crate) fn record_admission(&mut self, started: Instant) {
        self.admission.record(started);
    }
}

/// Response-decode phases owned by the reader, which may run on its own thread.
#[derive(Clone, Copy, Debug, Default)]
struct ResponseTiming {
    remote_execution: NativePhaseMeasurement,
    result_transfer: NativePhaseMeasurement,
    worker: WorkerPhaseTiming,
    result_bytes: u64,
}

pub(crate) enum LocalWorkerAttempt {
    Success(StagedExecutionResult),
    CompilerFailed {
        termination: CompilerTermination,
        result: StagedExecutionResult,
    },
    Cold(&'static str),
}

/// Result of local admission at the exact Cargo-visible effect boundary.
pub(crate) enum LocalAdmission {
    Committed(i32),
    RejectedBeforeEffect(&'static str),
    FailedAfterEffect(RailError),
}

pub(crate) enum LocalAttemptDecision {
    Completed(i32),
    CompilerFailed {
        termination: CompilerTermination,
        result: Box<StagedExecutionResult>,
    },
    Fallback(&'static str),
    OperationalFailure(RailError),
}

/// Execute the local one-shot protocol proof without granting it cache or output authority.
///
/// The caller still owns admission. Every protocol, process, capability, or
/// staging failure is deliberately collapsed to a cold outcome.
pub(crate) fn execute_local_worker(
    worker: &Path,
    rustc: &OsStr,
    candidate: &RustLibraryCandidate,
    staging: NativeResultStaging,
    cache: Option<&crate::cache::cas::LocalCas>,
    timing: &mut DistributedTiming,
) -> LocalWorkerAttempt {
    match execute_local_worker_inner(worker, rustc, candidate, staging, cache, timing) {
        Ok(DecodedExecution::Success(result)) => LocalWorkerAttempt::Success(result),
        Ok(DecodedExecution::CompilerFailed { termination, result }) => {
            LocalWorkerAttempt::CompilerFailed { termination, result }
        }
        Ok(DecodedExecution::Rejected) => LocalWorkerAttempt::Cold("distributed_execution_rejected"),
        Err(_) => LocalWorkerAttempt::Cold("distributed_execution_unavailable"),
    }
}

/// Execute one typed operation through a mutually authenticated direct worker.
/// Transport, protocol, or worker loss remains a cold decision until local
/// admission crosses the existing restore effect boundary.
pub(crate) fn execute_mutual_tls_worker(
    identity: &MutualTlsClientIdentity<'_>,
    rustc: &OsStr,
    candidate: &RustLibraryCandidate,
    staging: NativeResultStaging,
    allow_unqualified_isolation: bool,
    cache: Option<&crate::cache::cas::LocalCas>,
    timing: &mut DistributedTiming,
) -> LocalWorkerAttempt {
    match execute_mutual_tls_worker_inner(
        identity,
        rustc,
        candidate,
        staging,
        allow_unqualified_isolation,
        cache,
        timing,
    ) {
        Ok(DecodedExecution::Success(result)) => LocalWorkerAttempt::Success(result),
        Ok(DecodedExecution::CompilerFailed { termination, result }) => {
            LocalWorkerAttempt::CompilerFailed { termination, result }
        }
        Ok(DecodedExecution::Rejected) => LocalWorkerAttempt::Cold("distributed_execution_rejected"),
        Err(error) => {
            if std::env::var_os("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY").is_some() {
                eprintln!("cargo-rail native coverage: distributed transport unavailable: {error}");
            }
            LocalWorkerAttempt::Cold("distributed_transport_unavailable")
        }
    }
}
fn execute_mutual_tls_worker_inner(
    identity: &MutualTlsClientIdentity<'_>,
    rustc: &OsStr,
    candidate: &RustLibraryCandidate,
    staging: NativeResultStaging,
    allow_unqualified_isolation: bool,
    cache: Option<&crate::cache::cas::LocalCas>,
    timing: &mut DistributedTiming,
) -> RailResult<DecodedExecution> {
    let capability_started = Instant::now();
    let expected = capture_worker_capability(rustc, cache)?;
    timing.capability_capture.record(capability_started);
    let connect_started = Instant::now();
    let socket = connect_worker_endpoint(identity.endpoint)?;
    socket.set_nodelay(true)?;
    socket.set_read_timeout(Some(NETWORK_HANDSHAKE_TIMEOUT))?;
    socket.set_write_timeout(Some(NETWORK_HANDSHAKE_TIMEOUT))?;
    timing.connect.record(connect_started);
    let tls_started = Instant::now();
    let config = mutual_tls_client_config(identity)?;
    let server_name = rustls::pki_types::ServerName::try_from(identity.server_name.to_string())
        .map_err(|_| RailError::message("distributed worker TLS server name is invalid"))?;
    let connection = rustls::ClientConnection::new(config, server_name)
        .map_err(|error| RailError::message(format!("distributed worker TLS client is invalid: {error}")))?;
    let mut stream = rustls::StreamOwned::new(connection, socket);
    timing.tls_setup.record(tls_started);
    let result: RailResult<DecodedExecution> = (|| {
        // Complete mutual authentication before the first protocol frame so the
        // measured handshake phase does not silently absorb worker capability
        // latency. The frames below would drive the same handshake anyway.
        let handshake_started = Instant::now();
        while stream.conn.is_handshaking() {
            stream
                .conn
                .complete_io(&mut stream.sock)
                .map_err(|error| RailError::message(format!("distributed worker TLS handshake failed: {error}")))?;
        }
        timing.handshake.record(handshake_started);
        let capability_exchange_started = Instant::now();
        let capability: WorkerCapability =
            read_control_frame(&mut stream, CAPABILITY_MAGIC, CAPABILITY_TRAILER, "capability").map_err(|error| {
                RailError::message(format!("distributed worker capability exchange failed: {error}"))
            })?;
        timing.capability_exchange.record(capability_exchange_started);
        if capability.capability_id != identity.worker_capability_id
            || !worker_execution_environment_matches(&capability, &expected.capability)?
            || !worker_isolation_allowed(&capability, allow_unqualified_isolation)
        {
            return Err(RailError::message(format!(
                "mutually authenticated worker capability '{}' does not match selected compiler capability '{}'",
                capability.capability_id, expected.capability.capability_id
            )));
        }
        if stream.conn.alpn_protocol() != Some(b"cargo-rail-execution/3") {
            return Err(RailError::message(
                "distributed worker did not negotiate the execution protocol",
            ));
        }

        let lease_started = Instant::now();
        let workload_identity = client_workload_identity(identity)?;
        let pending_lease = format!("execution-lease-v3:sha256:{}", "0".repeat(64));
        let template = execution_request(&capability, candidate, &pending_lease, &workload_identity)?;
        let mut nonce = [0_u8; 32];
        getrandom::fill(&mut nonce)
            .map_err(|error| RailError::message(format!("distributed client nonce generation failed: {error}")))?;
        let lease_request = LeaseRequest {
            action_id: template.action_id,
            capability_id: capability.capability_id.clone(),
            client_nonce: format_sha256(nonce),
            protocol_version: PROTOCOL_VERSION,
            workload_identity: workload_identity.clone(),
        };
        validate_lease_request(&lease_request, &capability)?;
        write_control_frame(&mut stream, LEASE_REQUEST_MAGIC, LEASE_REQUEST_TRAILER, &lease_request)?;
        let grant: LeaseGrant = read_control_frame(&mut stream, LEASE_GRANT_MAGIC, LEASE_GRANT_TRAILER, "lease grant")?;
        validate_lease_grant(&grant, &lease_request)?;
        let request = execution_request(&capability, candidate, &grant.lease_id, &workload_identity)?;
        if request.action_id != grant.action_id {
            return Err(RailError::message(
                "distributed execution request changed after lease grant",
            ));
        }
        timing.lease.record(lease_started);
        let deadline = Duration::from_millis(request.limits.wall_time_ms)
            .checked_add(CLIENT_PROCESS_GRACE)
            .unwrap_or(Duration::MAX);
        stream.sock.set_read_timeout(Some(deadline))?;
        stream.sock.set_write_timeout(Some(deadline))?;
        let source_started = Instant::now();
        write_candidate_request(&mut stream, &request, candidate)?;
        timing.source_transfer.record(source_started);
        timing.source_bytes = request_input_bytes(&request);
        let mut response_timing = ResponseTiming::default();
        let decoded = read_response_into(&mut stream, &request, staging, &mut response_timing);
        timing.absorb_response(response_timing);
        let decoded = match decoded {
            Ok(decoded) => decoded,
            Err(error) => return Err(error),
        };
        let mut trailing = [0_u8; 1];
        let trailing_bytes = stream.read(&mut trailing).map_err(|error| {
            RailError::message(format!(
                "distributed worker TLS shutdown was not authenticated: {error}"
            ))
        })?;
        if trailing_bytes != 0 {
            return Err(RailError::message(
                "distributed worker sent data after the framed execution response",
            ));
        }
        Ok(decoded)
    })();
    if result.is_err() {
        settle_failed_mutual_tls_session(&mut stream);
    }
    result
}

fn settle_failed_mutual_tls_session(stream: &mut rustls::StreamOwned<rustls::ClientConnection, TcpStream>) {
    stream.conn.send_close_notify();
    drop(stream.flush());
    drop(stream.sock.shutdown(Shutdown::Write));
    let mut discarded = [0_u8; 1024];
    loop {
        match stream.sock.read(&mut discarded) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
}

/// Preserve the only legal fallback boundary: before the first visible effect.
pub(crate) fn decide_local_attempt(
    attempt: LocalWorkerAttempt,
    admit: impl FnOnce(StagedExecutionResult) -> LocalAdmission,
) -> LocalAttemptDecision {
    match attempt {
        LocalWorkerAttempt::Success(result) => match admit(result) {
            LocalAdmission::Committed(exit_code) => LocalAttemptDecision::Completed(exit_code),
            LocalAdmission::RejectedBeforeEffect(reason) => LocalAttemptDecision::Fallback(reason),
            LocalAdmission::FailedAfterEffect(error) => LocalAttemptDecision::OperationalFailure(error),
        },
        LocalWorkerAttempt::CompilerFailed { termination, result } => LocalAttemptDecision::CompilerFailed {
            termination,
            result: Box::new(result),
        },
        LocalWorkerAttempt::Cold(reason) => LocalAttemptDecision::Fallback(reason),
    }
}

#[derive(Clone, Copy)]
pub(crate) struct DistributedAdmissionAuthority<'a> {
    pub(crate) context: &'a crate::compiler::native_cache::NativeCacheContext,
    pub(crate) cas: &'a crate::cache::cas::LocalCas,
    pub(crate) session: &'a crate::compiler::native_cache::NativeCompilerSession,
    pub(crate) capture: &'a crate::compiler::native_cache::NativeActionCapture,
    pub(crate) base_action_key: &'a str,
    pub(crate) observation: &'a crate::compiler::observation::RawCompilerInvocation,
    pub(crate) output_paths: &'a crate::compiler::observation::NativeOutputPaths,
    pub(crate) candidate: &'a RustLibraryCandidate,
    pub(crate) cache_bytes_read: u64,
}

/// Execute one locally connected worker only after the caller has completed
/// L1/L2 lookup, then bind success through native L1 and restore authority.
pub(crate) fn execute_and_admit_local_worker(
    worker: &Path,
    rustc: &OsStr,
    authority: DistributedAdmissionAuthority<'_>,
) -> LocalAttemptDecision {
    let started = Instant::now();
    let mut timing = DistributedTiming::default();
    let staging = match authority.cas.native_result_staging() {
        Ok(staging) => staging,
        Err(_) => return LocalAttemptDecision::Fallback("distributed_local_staging_unavailable"),
    };
    let attempt = execute_local_worker(
        worker,
        rustc,
        authority.candidate,
        staging,
        Some(authority.cas),
        &mut timing,
    );
    timing.record_attempt(started);
    decide_local_attempt(attempt, |result| {
        crate::compiler::native_cache::admit_distributed_rust_library_result(authority, result, timing)
    })
}
pub(crate) fn execute_and_admit_mutual_tls_worker(
    identity: &MutualTlsClientIdentity<'_>,
    rustc: &OsStr,
    authority: DistributedAdmissionAuthority<'_>,
    allow_unqualified_isolation: bool,
) -> LocalAttemptDecision {
    let started = Instant::now();
    let mut timing = DistributedTiming::default();
    let staging = match authority.cas.native_result_staging() {
        Ok(staging) => staging,
        Err(_) => return LocalAttemptDecision::Fallback("distributed_local_staging_unavailable"),
    };
    let attempt = execute_mutual_tls_worker(
        identity,
        rustc,
        authority.candidate,
        staging,
        allow_unqualified_isolation,
        Some(authority.cas),
        &mut timing,
    );
    timing.record_attempt(started);
    decide_local_attempt(attempt, |result| {
        crate::compiler::native_cache::admit_distributed_rust_library_result(authority, result, timing)
    })
}

fn execute_local_worker_inner(
    worker: &Path,
    rustc: &OsStr,
    candidate: &RustLibraryCandidate,
    staging: NativeResultStaging,
    cache: Option<&crate::cache::cas::LocalCas>,
    timing: &mut DistributedTiming,
) -> RailResult<DecodedExecution> {
    let capability_started = Instant::now();
    let capability = query_local_worker_capability(worker, rustc)?;
    let expected = capture_worker_capability(rustc, cache)?;
    timing.capability_capture.record(capability_started);
    if capability != expected.capability {
        return Err(RailError::message(
            "local distributed worker capability does not match the selected compiler",
        ));
    }
    let request = local_execution_request(&capability, candidate)?;
    let mut command = Command::new(worker);
    command
        .arg("execute")
        .arg(rustc)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let Some(mut stdin) = child.stdin.take() else {
        terminate_child(&mut child);
        return Err(RailError::message("local distributed worker stdin is unavailable"));
    };
    let Some(mut stdout) = child.stdout.take() else {
        terminate_child(&mut child);
        return Err(RailError::message("local distributed worker stdout is unavailable"));
    };
    let Some(stderr) = child.stderr.take() else {
        terminate_child(&mut child);
        return Err(RailError::message("local distributed worker stderr is unavailable"));
    };
    let source_started = Instant::now();
    if let Err(error) = write_candidate_request(&mut stdin, &request, candidate) {
        terminate_child(&mut child);
        return Err(error);
    }
    timing.source_transfer.record(source_started);
    timing.source_bytes = request_input_bytes(&request);

    let expected_request = request.clone();
    let response_reader = match thread::Builder::new()
        .name("cargo-rail-distributed-response".to_string())
        .spawn(move || {
            let mut response_timing = ResponseTiming::default();
            let decoded = read_response_into(&mut stdout, &expected_request, staging, &mut response_timing);
            (decoded, response_timing)
        }) {
        Ok(reader) => reader,
        Err(error) => {
            terminate_child(&mut child);
            return Err(error.into());
        }
    };
    let stderr_reader = match thread::Builder::new()
        .name("cargo-rail-distributed-client-stderr".to_string())
        .spawn(move || capture_stream(stderr, MAX_HEADER_BYTES as u64))
    {
        Ok(reader) => reader,
        Err(error) => {
            terminate_child(&mut child);
            drop(stdin);
            drop(response_reader.join());
            return Err(error.into());
        }
    };

    let timeout = Duration::from_millis(request.limits.wall_time_ms)
        .checked_add(CLIENT_PROCESS_GRACE)
        .unwrap_or(Duration::MAX);
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                terminate_child(&mut child);
                drop(stdin);
                drop(response_reader.join());
                drop(stderr_reader.join());
                return Err(error.into());
            }
        }
        if started.elapsed() > timeout {
            terminate_child(&mut child);
            drop(stdin);
            drop(response_reader.join());
            drop(stderr_reader.join());
            return Err(RailError::message(
                "local distributed worker exceeded its client deadline",
            ));
        }
        thread::sleep(WORKER_POLL_INTERVAL);
    };
    drop(stdin);
    let (response, response_timing) = response_reader
        .join()
        .map_err(|_| RailError::message("local distributed response reader panicked"))?;
    timing.absorb_response(response_timing);
    let response = response?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| RailError::message("local distributed stderr reader panicked"))??
        .ok_or_else(|| RailError::message("local distributed worker stderr exceeded its byte bound"))?;
    if !status.success() || !stderr.is_empty() {
        return Err(RailError::message("local distributed worker process failed"));
    }
    Ok(response)
}

fn query_local_worker_capability(worker: &Path, rustc: &OsStr) -> RailResult<WorkerCapability> {
    let mut command = Command::new(worker);
    command
        .arg("capability")
        .arg(rustc)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = run_bounded_command(command, Duration::from_secs(30), MAX_HEADER_BYTES as u64)?;
    if !output.status.success() || !output.stderr.is_empty() || output.stdout.last() != Some(&b'\n') {
        return Err(RailError::message("local distributed capability query failed"));
    }
    let encoded = &output.stdout[..output.stdout.len().saturating_sub(1)];
    let capability: WorkerCapability = serde_json::from_slice(encoded)
        .map_err(|_| RailError::message("local distributed capability response is malformed"))?;
    if canonical_json(&capability)? != encoded {
        return Err(RailError::message(
            "local distributed capability response is not canonical",
        ));
    }
    validate_capability(&capability)?;
    Ok(capability)
}

fn run_bounded_command(
    mut command: Command,
    timeout: Duration,
    stream_limit: u64,
) -> RailResult<CapturedCompilerOutput> {
    let mut child = command.spawn()?;
    let Some(stdout) = child.stdout.take() else {
        terminate_child(&mut child);
        return Err(RailError::message("bounded command stdout is unavailable"));
    };
    let Some(stderr) = child.stderr.take() else {
        terminate_child(&mut child);
        return Err(RailError::message("bounded command stderr is unavailable"));
    };
    let stdout_reader = match thread::Builder::new()
        .name("cargo-rail-bounded-command-stdout".to_string())
        .spawn(move || capture_stream(stdout, stream_limit))
    {
        Ok(reader) => reader,
        Err(error) => {
            terminate_child(&mut child);
            return Err(error.into());
        }
    };
    let stderr_reader = match thread::Builder::new()
        .name("cargo-rail-bounded-command-stderr".to_string())
        .spawn(move || capture_stream(stderr, stream_limit))
    {
        Ok(reader) => reader,
        Err(error) => {
            terminate_child(&mut child);
            drop(stdout_reader.join());
            return Err(error.into());
        }
    };
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                terminate_child(&mut child);
                join_streams(stdout_reader, stderr_reader);
                return Err(error.into());
            }
        }
        if started.elapsed() > timeout {
            terminate_child(&mut child);
            join_streams(stdout_reader, stderr_reader);
            return Err(RailError::message("bounded command exceeded its deadline"));
        }
        thread::sleep(WORKER_POLL_INTERVAL);
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| RailError::message("bounded command stdout reader panicked"))??
        .ok_or_else(|| RailError::message("bounded command stdout exceeded its byte bound"))?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| RailError::message("bounded command stderr reader panicked"))??
        .ok_or_else(|| RailError::message("bounded command stderr exceeded its byte bound"))?;
    Ok(CapturedCompilerOutput { status, stdout, stderr })
}

fn local_execution_request(
    capability: &WorkerCapability,
    candidate: &RustLibraryCandidate,
) -> RailResult<ExecutionRequest> {
    let sequence = LOCAL_LEASE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let issued = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RailError::message("local distributed lease clock is unavailable"))?;
    let pending_lease = format!("execution-lease-v3:sha256:{}", "0".repeat(64));
    let workload_identity = format!(
        "workload-v1:sha256:{}",
        ContentDigest::sha256(b"local-process-qualification")
    );
    let template = execution_request(capability, candidate, &pending_lease, &workload_identity)?;
    let lease = canonical_json(&(&template.action_id, std::process::id(), sequence, issued.as_nanos()))?;
    let lease_id = format!("execution-lease-v3:sha256:{}", ContentDigest::sha256(&lease));
    execution_request(capability, candidate, &lease_id, &workload_identity)
}

fn execution_request(
    capability: &WorkerCapability,
    candidate: &RustLibraryCandidate,
    lease_id: &str,
    workload_identity: &str,
) -> RailResult<ExecutionRequest> {
    validate_capability(capability)?;
    validate_operation(&candidate.operation)?;
    let mut request = ExecutionRequest {
        action_id: String::new(),
        capability_id: capability.capability_id.clone(),
        inputs: candidate.input_frames(),
        lease_id: lease_id.to_string(),
        limits: capability.resource_limits,
        operation: candidate.operation.clone(),
        protocol_version: PROTOCOL_VERSION,
        workload_identity: workload_identity.to_string(),
    };
    request.action_id = action_identity(&request)?;
    validate_request(&request, capability)?;
    Ok(request)
}

enum Cancellation {
    Requested,
    ClientLost,
    Invalid,
}

enum CompilerRun {
    Completed(CapturedCompilerOutput),
    Cancelled(&'static str),
    Failed(&'static str),
}

struct CapturedCompilerOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Run one explicit worker role without entering the ordinary CLI.
pub(crate) fn worker_main() -> i32 {
    match run_worker_command(std::env::args_os().skip(1).collect()) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("cargo-rail distributed worker: {error}");
            2
        }
    }
}

fn run_worker_command(arguments: Vec<OsString>) -> RailResult<()> {
    match arguments.as_slice() {
        [command] if command == "protocol-version" => {
            println!("{PROTOCOL_VERSION}");
            Ok(())
        }
        [command, rustc] if command == "capability" => {
            let captured = capture_worker_capability(rustc, None)?;
            let encoded = canonical_json(&captured.capability)?;
            std::io::stdout().write_all(&encoded)?;
            std::io::stdout().write_all(b"\n")?;
            Ok(())
        }
        [command, rustc] if command == "execute" => execute_once(rustc),
        #[cfg(target_os = "linux")]
        [command, rustc] if command == "execute-sandboxed" => execute_sandboxed(rustc),
        #[cfg(target_os = "linux")]
        [command, rustc, bubblewrap, sysroot, worker, cgroup, scratch_bytes]
            if command == "execute-cgroup-bubblewrap" =>
        {
            launch_cgroup_bubblewrap(
                rustc,
                bubblewrap,
                Path::new(sysroot),
                Path::new(worker),
                Path::new(cgroup),
                scratch_bytes,
            )
        }
        #[cfg(target_os = "linux")]
        [command, probe, cgroup] if command == "probe-cgroup" => run_cgroup_probe(probe, Path::new(cgroup)),
        #[cfg(target_os = "linux")]
        [command] if command == "probe-cgroup-idle" => {
            thread::sleep(Duration::from_secs(30));
            Ok(())
        }
        [command, rustc] if command == "qualify-local-client" => qualify_local_client(rustc),
        [command, rustc, bubblewrap] if command == "qualify-bubblewrap" => qualify_bubblewrap_worker(rustc, bubblewrap),
        [
            command,
            rustc,
            bind,
            server_certificate,
            server_private_key,
            client_authority,
            max_concurrency,
        ] if command == "serve-mtls" => {
            let bind = bind
                .to_str()
                .ok_or_else(|| RailError::message("distributed worker bind address is not UTF-8"))?;
            let max_concurrency = max_concurrency
                .to_str()
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|value| (1..=MAX_NETWORK_CONCURRENCY).contains(value))
                .ok_or_else(|| RailError::message("distributed worker concurrency is outside its bound"))?;
            serve_mutual_tls(
                rustc,
                bind,
                Path::new(server_certificate),
                Path::new(server_private_key),
                Path::new(client_authority),
                max_concurrency,
            )
        }
        [
            command,
            rustc,
            bubblewrap,
            bind,
            server_certificate,
            server_private_key,
            client_authority,
            max_concurrency,
        ] if command == "serve-mtls-bubblewrap" => {
            let bind = bind
                .to_str()
                .ok_or_else(|| RailError::message("distributed worker bind address is not UTF-8"))?;
            let max_concurrency = max_concurrency
                .to_str()
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|value| (1..=MAX_NETWORK_CONCURRENCY).contains(value))
                .ok_or_else(|| RailError::message("distributed worker concurrency is outside its bound"))?;
            serve_mutual_tls_bubblewrap(
                rustc,
                bubblewrap,
                bind,
                Path::new(server_certificate),
                Path::new(server_private_key),
                Path::new(client_authority),
                max_concurrency,
            )
        }
        [
            command,
            rustc,
            endpoint,
            server_name,
            worker_capability_id,
            authority,
            client_certificate,
            client_private_key,
        ] if command == "qualify-mtls-client" => {
            let endpoint = endpoint
                .to_str()
                .ok_or_else(|| RailError::message("distributed worker endpoint is not UTF-8"))?;
            let server_name = server_name
                .to_str()
                .ok_or_else(|| RailError::message("distributed worker server name is not UTF-8"))?;
            qualify_mutual_tls_client(
                rustc,
                MutualTlsClientIdentity {
                    endpoint,
                    server_name,
                    worker_capability_id: worker_capability_id
                        .to_str()
                        .ok_or_else(|| RailError::message("distributed worker capability identity is not UTF-8"))?,
                    authority_certificate: Path::new(authority),
                    client_certificate: Path::new(client_certificate),
                    client_private_key: Path::new(client_private_key),
                },
            )
        }
        _ => Err(RailError::message(
            "expected a bounded capability, execute, qualification, or mTLS server command",
        )),
    }
}

fn qualify_mutual_tls_client(rustc: &OsStr, identity: MutualTlsClientIdentity<'_>) -> RailResult<()> {
    let staging_parent = tempfile::Builder::new()
        .prefix("cargo-rail-distributed-mtls-qualification-")
        .tempdir()?;
    let staging = NativeResultStaging::temporary_in(staging_parent.path())?;
    let candidate = RustLibraryCandidate::new(
        RustLibraryCandidateInput {
            crate_name: "cargo_rail_distributed_mtls_qualification".to_string(),
            crate_type: "rlib".to_string(),
            dep_info_name: "cargo_rail_distributed_mtls_qualification-c0dec0dec0dec0df.d".to_string(),
            edition: "2024".to_string(),
            emission: RustLibraryEmission::MetadataAndLink,
            metadata: "c0dec0dec0dec0df".to_string(),
            metadata_name: "libcargo_rail_distributed_mtls_qualification-c0dec0dec0dec0df.rmeta".to_string(),
            extra_filename: "-c0dec0dec0dec0df".to_string(),
            output_relative_directory: "target/debug/deps".to_string(),
            source_relative_path: "src/lib.rs".to_string(),
            test_mode: false,
            toolchain_proc_macro: false,
            rlib_name: Some("libcargo_rail_distributed_mtls_qualification-c0dec0dec0dec0df.rlib".to_string()),
            options: RustLibraryExecutionOptions::default(),
        },
        b"#![forbid(unsafe_code)]\npub fn mutually_authenticated() -> bool { true }\n".to_vec(),
    )?;
    let mut timing = DistributedTiming::default();
    let result = execute_mutual_tls_worker_inner(&identity, rustc, &candidate, staging, true, None, &mut timing)?;
    let DecodedExecution::Success(result) = result else {
        return Err(RailError::message(
            "mutually authenticated distributed qualification did not compile successfully",
        ));
    };
    for slot in [
        DistributedResultSlot::DepInfo,
        DistributedResultSlot::Metadata,
        DistributedResultSlot::Rlib,
        DistributedResultSlot::Stderr,
        DistributedResultSlot::Stdout,
    ] {
        result.verified_frame(slot)?;
    }
    println!("{PROTOCOL_VERSION}");
    Ok(())
}

#[cfg(target_os = "linux")]
fn qualify_bubblewrap_worker(rustc: &OsStr, bubblewrap: &OsStr) -> RailResult<()> {
    let captured = capture_bubblewrap_worker_capability(rustc, bubblewrap)?;
    qualify_worker_runtime(&captured)?;
    qualify_cgroup_enforcement(&captured)?;
    println!("{PROTOCOL_VERSION}");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn qualify_bubblewrap_worker(_rustc: &OsStr, _bubblewrap: &OsStr) -> RailResult<()> {
    Err(RailError::message(
        "distributed Bubblewrap isolation is available only on Linux",
    ))
}

#[cfg(target_os = "linux")]
fn qualify_worker_runtime(captured: &CapturedWorkerCapability) -> RailResult<()> {
    let candidate = RustLibraryCandidate::new(
        RustLibraryCandidateInput {
            crate_name: "cargo_rail_distributed_runtime_qualification".to_string(),
            crate_type: "rlib".to_string(),
            dep_info_name: "cargo_rail_distributed_runtime_qualification-c0dec0dec0dec0da.d".to_string(),
            edition: "2024".to_string(),
            emission: RustLibraryEmission::MetadataAndLink,
            metadata: "c0dec0dec0dec0da".to_string(),
            metadata_name: "libcargo_rail_distributed_runtime_qualification-c0dec0dec0dec0da.rmeta".to_string(),
            extra_filename: "-c0dec0dec0dec0da".to_string(),
            output_relative_directory: "target/debug/deps".to_string(),
            source_relative_path: "src/lib.rs".to_string(),
            test_mode: false,
            toolchain_proc_macro: false,
            rlib_name: Some("libcargo_rail_distributed_runtime_qualification-c0dec0dec0dec0da.rlib".to_string()),
            options: RustLibraryExecutionOptions::default(),
        },
        b"#![forbid(unsafe_code)]\npub fn isolated() -> bool { true }\n".to_vec(),
    )?;
    let request = local_execution_request(&captured.capability, &candidate)?;
    let mut framed = Vec::new();
    write_candidate_request(&mut framed, &request, &candidate)?;
    let envelope = read_request(&mut std::io::Cursor::new(framed))?;
    let (_cancellation_sender, cancellation_receiver) = mpsc::sync_channel(1);
    let attempt = tempfile::Builder::new()
        .prefix("cargo-rail-distributed-runtime-qualification-")
        .tempdir()?;
    let attempt_root = crate::utils::canonicalize_existing(attempt.path())?;
    let frames = match execute_request(
        captured,
        &envelope,
        &attempt_root,
        &cancellation_receiver,
        &mut WorkerPhaseTiming::default(),
    )? {
        WorkerExecution::Success(frames) => frames,
        WorkerExecution::CompilerFailed { frames, .. } => {
            let detail = frames.iter().find_map(|frame| {
                (frame.descriptor.slot == ResponseSlot::Stderr).then(|| match &frame.payload {
                    ResponsePayload::Bytes(bytes) => String::from_utf8_lossy(bytes).trim().to_string(),
                    ResponsePayload::File(_) => "sandbox returned file-backed diagnostics".to_string(),
                })
            });
            return Err(RailError::message(format!(
                "distributed worker runtime qualification compiler failed: {}",
                detail
                    .as_deref()
                    .filter(|detail| !detail.is_empty())
                    .unwrap_or("no stderr")
            )));
        }
        WorkerExecution::Rejected(reason) => {
            return Err(RailError::message(format!(
                "distributed worker runtime qualification was rejected: {reason}"
            )));
        }
    };
    if frames.len() != 5
        || frames
            .iter()
            .any(|frame| matches!(&frame.payload, ResponsePayload::File(path) if !path.starts_with(&attempt_root)))
    {
        return Err(RailError::message(
            "distributed worker runtime qualification returned an invalid result boundary",
        ));
    }
    if let WorkerRuntime::Bubblewrap { cgroup, .. } = &captured.runtime {
        cgroup.validate_idle()?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn qualify_cgroup_enforcement(captured: &CapturedWorkerCapability) -> RailResult<()> {
    let WorkerRuntime::Bubblewrap { cgroup, worker, .. } = &captured.runtime else {
        return Err(RailError::message(
            "distributed resource qualification received another runtime",
        ));
    };
    if !worker_runtime_generation_is_stable(captured) {
        return Err(RailError::message(
            "distributed worker generation changed before resource qualification",
        ));
    }

    for probe in [CgroupProbe::Cpu, CgroupProbe::Memory, CgroupProbe::Processes] {
        let resource_attempt = cgroup.create_attempt(probe.limits())?;
        let mut child = Command::new(worker)
            .arg("probe-cgroup")
            .arg(probe.name())
            .arg(&resource_attempt.path)
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env_clear()
            .spawn()?;
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(error) => {
                    drop(child.kill());
                    drop(child.wait());
                    drop(resource_attempt.finish());
                    return Err(error.into());
                }
            }
            if started.elapsed() > Duration::from_secs(10) {
                drop(child.kill());
                drop(child.wait());
                drop(resource_attempt.finish());
                return Err(RailError::message(format!(
                    "distributed {} cgroup probe exceeded its qualification deadline",
                    probe.name()
                )));
            }
            thread::sleep(WORKER_POLL_INTERVAL);
        };
        let outcome = resource_attempt.finish()?;
        let enforced = match probe {
            CgroupProbe::Cpu => {
                status.success()
                    && outcome.cpu_throttles > 0
                    && outcome.memory_oom_kills == 0
                    && outcome.process_limit_hits == 0
            }
            CgroupProbe::Memory => !status.success() && outcome.memory_oom_kills > 0,
            CgroupProbe::Processes => {
                status.success() && outcome.process_limit_hits > 0 && outcome.memory_oom_kills == 0
            }
        };
        if !enforced {
            return Err(RailError::message(format!(
                "distributed {} cgroup limit did not produce exact kernel enforcement evidence",
                probe.name()
            )));
        }
    }
    cgroup.validate_idle()
}

fn serve_mutual_tls(
    rustc: &OsStr,
    bind: &str,
    server_certificate: &Path,
    server_private_key: &Path,
    client_authority: &Path,
    max_concurrency: u32,
) -> RailResult<()> {
    serve_mutual_tls_with_capability(
        capture_worker_capability(rustc, None)?,
        bind,
        server_certificate,
        server_private_key,
        client_authority,
        max_concurrency,
    )
}

#[cfg(target_os = "linux")]
fn serve_mutual_tls_bubblewrap(
    rustc: &OsStr,
    bubblewrap: &OsStr,
    bind: &str,
    server_certificate: &Path,
    server_private_key: &Path,
    client_authority: &Path,
    max_concurrency: u32,
) -> RailResult<()> {
    let captured = capture_bubblewrap_worker_capability(rustc, bubblewrap)?;
    qualify_worker_runtime(&captured)?;
    serve_mutual_tls_with_capability(
        captured,
        bind,
        server_certificate,
        server_private_key,
        client_authority,
        max_concurrency,
    )
}

#[cfg(not(target_os = "linux"))]
fn serve_mutual_tls_bubblewrap(
    _rustc: &OsStr,
    _bubblewrap: &OsStr,
    _bind: &str,
    _server_certificate: &Path,
    _server_private_key: &Path,
    _client_authority: &Path,
    _max_concurrency: u32,
) -> RailResult<()> {
    Err(RailError::message(
        "distributed Bubblewrap isolation is available only on Linux",
    ))
}

fn serve_mutual_tls_with_capability(
    captured: CapturedWorkerCapability,
    bind: &str,
    server_certificate: &Path,
    server_private_key: &Path,
    client_authority: &Path,
    max_concurrency: u32,
) -> RailResult<()> {
    let captured = Arc::new(captured);
    let config = mutual_tls_server_config(server_certificate, server_private_key, client_authority)?;
    let listener = TcpListener::bind(bind)?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    let startup = canonical_json(&serde_json::json!({
      "address": address.to_string(),
      "capability_id": captured.capability.capability_id,
      "event": "worker_ready",
      "isolation": captured.capability.isolation,
      "isolation_identity": captured.capability.isolation_identity,
      "max_concurrency": max_concurrency,
      "protocol_version": PROTOCOL_VERSION,
      "resource_limits": captured.capability.resource_limits,
      "transport": "mutual_tls_1_3",
    }))?;
    std::io::stdout().write_all(&startup)?;
    std::io::stdout().write_all(b"\n")?;
    std::io::stdout().flush()?;

    let active = Arc::new(AtomicU64::new(0));
    let draining = Arc::new(AtomicBool::new(false));
    register_worker_drain_signals(&draining)?;
    while !draining.load(Ordering::Acquire) {
        let socket = match listener.accept() {
            Ok((socket, _)) => socket,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(WORKER_DRAIN_POLL_INTERVAL);
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        socket.set_nonblocking(false)?;
        let accepted = Instant::now();
        if active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < u64::from(max_concurrency)).then_some(current.saturating_add(1))
            })
            .is_err()
        {
            drop(socket.shutdown(Shutdown::Both));
            continue;
        }
        let active_connection = ActiveConnection::new(Arc::clone(&active));
        let captured = Arc::clone(&captured);
        let config = Arc::clone(&config);
        drop(
            thread::Builder::new()
                .name("cargo-rail-distributed-mtls".to_string())
                .spawn(move || {
                    drop(serve_mutual_tls_connection(
                        socket,
                        accepted,
                        config,
                        &captured,
                        active_connection,
                    ));
                }),
        );
    }
    drop(listener);
    write_worker_event(&serde_json::json!({
      "active_connections": active.load(Ordering::Acquire),
      "event": "worker_draining",
      "protocol_version": PROTOCOL_VERSION,
    }))?;
    let deadline = Instant::now()
        .checked_add(WORKER_DRAIN_TIMEOUT)
        .ok_or_else(|| RailError::message("distributed worker drain deadline overflowed"))?;
    while active.load(Ordering::Acquire) != 0 {
        if Instant::now() >= deadline {
            return Err(RailError::message(
                "distributed worker did not drain its bounded active connections",
            ));
        }
        thread::sleep(WORKER_DRAIN_POLL_INTERVAL);
    }
    write_worker_event(&serde_json::json!({
      "active_connections": 0,
      "event": "worker_stopped",
      "protocol_version": PROTOCOL_VERSION,
    }))?;
    Ok(())
}

#[cfg(unix)]
fn register_worker_drain_signals(draining: &Arc<AtomicBool>) -> RailResult<()> {
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(draining))?;
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(draining))?;
    Ok(())
}

#[cfg(not(unix))]
fn register_worker_drain_signals(_draining: &Arc<AtomicBool>) -> RailResult<()> {
    Ok(())
}

struct ActiveConnection(Option<Arc<AtomicU64>>);

impl ActiveConnection {
    fn new(active: Arc<AtomicU64>) -> Self {
        Self(Some(active))
    }

    fn release(&mut self) {
        if let Some(active) = self.0.take() {
            active.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl Drop for ActiveConnection {
    fn drop(&mut self) {
        self.release();
    }
}

struct ActiveMutualTlsConnection {
    // Fields drop in declaration order. Releasing capacity before closing the
    // socket makes a peer-observed disconnect an admission-ready boundary.
    active: ActiveConnection,
    stream: rustls::StreamOwned<rustls::ServerConnection, TcpStream>,
}

fn serve_mutual_tls_connection(
    socket: TcpStream,
    accepted: Instant,
    config: Arc<rustls::ServerConfig>,
    captured: &CapturedWorkerCapability,
    active: ActiveConnection,
) -> RailResult<()> {
    socket.set_nodelay(true)?;
    socket.set_read_timeout(Some(NETWORK_HANDSHAKE_TIMEOUT))?;
    socket.set_write_timeout(Some(NETWORK_HANDSHAKE_TIMEOUT))?;
    let cancellation_socket = socket.try_clone()?;
    let connection = rustls::ServerConnection::new(config)
        .map_err(|error| RailError::message(format!("distributed worker TLS server is invalid: {error}")))?;
    let mut connection = ActiveMutualTlsConnection {
        active,
        stream: rustls::StreamOwned::new(connection, socket),
    };
    let mut stream = &mut connection.stream;
    write_control_frame(stream, CAPABILITY_MAGIC, CAPABILITY_TRAILER, &captured.capability)?;
    let peer_workload_identity = peer_workload_identity(stream.conn.peer_certificates())?;
    let lease_request: LeaseRequest =
        read_control_frame(&mut stream, LEASE_REQUEST_MAGIC, LEASE_REQUEST_TRAILER, "lease request")?;
    if stream.conn.alpn_protocol() != Some(b"cargo-rail-execution/3") {
        return Err(RailError::message(
            "distributed client did not negotiate the execution protocol",
        ));
    }
    validate_lease_request(&lease_request, &captured.capability)?;
    if lease_request.workload_identity != peer_workload_identity {
        return Err(RailError::message(
            "distributed execution lease workload does not match the authenticated client certificate",
        ));
    }
    let grant = grant_connection_lease(&lease_request)?;
    write_control_frame(&mut stream, LEASE_GRANT_MAGIC, LEASE_GRANT_TRAILER, &grant)?;
    let envelope = read_request(&mut stream)?;
    validate_request(&envelope.request, &captured.capability)?;
    if envelope.request.action_id != grant.action_id
        || envelope.request.capability_id != grant.capability_id
        || envelope.request.lease_id != grant.lease_id
        || envelope.request.workload_identity != grant.workload_identity
    {
        return Err(RailError::message(
            "distributed execution request does not match its one-use connection lease",
        ));
    }

    let deadline = Duration::from_millis(envelope.request.limits.wall_time_ms)
        .checked_add(CLIENT_PROCESS_GRACE)
        .unwrap_or(Duration::MAX);
    stream.sock.set_read_timeout(Some(deadline))?;
    stream.sock.set_write_timeout(Some(deadline))?;
    let stopped = Arc::new(AtomicBool::new(false));
    let (cancellation_sender, cancellation_receiver) = mpsc::sync_channel(1);
    let monitor_stopped = Arc::clone(&stopped);
    let monitor = thread::Builder::new()
        .name("cargo-rail-distributed-client-liveness".to_string())
        .spawn(move || monitor_client_connection(cancellation_socket, &monitor_stopped, cancellation_sender))?;
    let attempt = tempfile::Builder::new()
        .prefix("cargo-rail-distributed-network-attempt-")
        .tempdir()?;
    let attempt_root = crate::utils::canonicalize_existing(attempt.path())?;
    let execution_started = Instant::now();
    let mut worker_timing = WorkerPhaseTiming {
        queue_ns: elapsed_nanos_between(accepted, execution_started),
        source_bytes: request_input_bytes(&envelope.request),
        ..WorkerPhaseTiming::default()
    };
    let prepared = execute_request(
        captured,
        &envelope,
        &attempt_root,
        &cancellation_receiver,
        &mut worker_timing,
    )?;
    stopped.store(true, Ordering::Release);
    monitor
        .join()
        .map_err(|_| RailError::message("distributed client liveness monitor panicked"))?;
    let prepared = if worker_runtime_generation_is_stable(captured) {
        prepared
    } else {
        WorkerExecution::Rejected("compiler_generation_changed")
    };
    worker_timing.result_bytes = prepared.result_bytes();
    worker_timing.elapsed_ns = elapsed_nanos(execution_started);
    let response = match prepared {
        WorkerExecution::Success(frames) => successful_response(&envelope.request, frames, worker_timing),
        WorkerExecution::CompilerFailed { termination, frames } => {
            compiler_failure_response(&envelope.request, termination, frames, worker_timing)
        }
        WorkerExecution::Rejected(reason) => rejected_response(&envelope.request, reason, worker_timing),
    };
    let event = serde_json::json!({
      "action_id": &envelope.request.action_id,
      "capability_id": &envelope.request.capability_id,
      "compiler_ns": worker_timing.compiler_ns,
      "elapsed_ns": worker_timing.elapsed_ns,
      "event": "execution_finished",
      "input_ns": worker_timing.input_ns,
      "protocol_version": PROTOCOL_VERSION,
      "queue_ns": worker_timing.queue_ns,
      "reason": response.0.reason.as_deref(),
      "result_bytes": worker_timing.result_bytes,
      "result_encode_ns": worker_timing.result_encode_ns,
      "source_bytes": worker_timing.source_bytes,
      "status": response.0.status,
      "workload_identity": &envelope.request.workload_identity,
    });
    write_worker_event(&event)?;
    write_response(stream, &envelope.request, &response.0, &response.1)?;
    stream.flush()?;
    connection.active.release();
    stream.conn.send_close_notify();
    stream.flush()?;
    Ok(())
}

fn write_worker_event(event: &serde_json::Value) -> RailResult<()> {
    let encoded = canonical_json(event)?;
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    let result = (|| -> std::io::Result<()> {
        stdout.write_all(&encoded)?;
        stdout.write_all(b"\n")?;
        stdout.flush()
    })();
    match result {
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        result => result.map_err(Into::into),
    }
}

/// Watch the authenticated client for loss or cancellation while rustc runs.
///
/// The poll interval also bounds how long the connection handler waits to join
/// this thread once execution finishes, so it sits on the measured critical
/// path of every successful attempt. A coarse interval stalled each attempt by
/// up to its full length after the compiler had already exited.
fn monitor_client_connection(socket: TcpStream, stopped: &AtomicBool, cancellation: mpsc::SyncSender<Cancellation>) {
    if socket.set_read_timeout(Some(WORKER_POLL_INTERVAL)).is_err() {
        drop(cancellation.send(Cancellation::ClientLost));
        return;
    }
    let mut byte = [0_u8; 1];
    while !stopped.load(Ordering::Acquire) {
        match socket.peek(&mut byte) {
            Ok(0) => {
                drop(cancellation.send(Cancellation::ClientLost));
                return;
            }
            Ok(_) => {
                drop(cancellation.send(Cancellation::Requested));
                return;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted
                ) => {}
            Err(_) => {
                drop(cancellation.send(Cancellation::ClientLost));
                return;
            }
        }
    }
}

fn grant_connection_lease(request: &LeaseRequest) -> RailResult<LeaseGrant> {
    let mut nonce = [0_u8; 32];
    getrandom::fill(&mut nonce)
        .map_err(|error| RailError::message(format!("distributed worker lease generation failed: {error}")))?;
    let lease = canonical_json(&(
        &request.action_id,
        &request.capability_id,
        &request.client_nonce,
        &request.workload_identity,
        nonce,
    ))?;
    Ok(LeaseGrant {
        action_id: request.action_id.clone(),
        capability_id: request.capability_id.clone(),
        lease_id: format!("execution-lease-v3:sha256:{}", ContentDigest::sha256(&lease)),
        protocol_version: PROTOCOL_VERSION,
        workload_identity: request.workload_identity.clone(),
    })
}

fn validate_lease_request(request: &LeaseRequest, capability: &WorkerCapability) -> RailResult<()> {
    if request.protocol_version != PROTOCOL_VERSION
        || request.capability_id != capability.capability_id
        || !valid_identity(&request.action_id, "execution-action-v3:sha256:")
        || !valid_identity(&request.client_nonce, "sha256:")
        || !valid_identity(&request.workload_identity, "workload-v1:sha256:")
    {
        return Err(RailError::message("distributed execution lease request is invalid"));
    }
    Ok(())
}

fn validate_lease_grant(grant: &LeaseGrant, request: &LeaseRequest) -> RailResult<()> {
    if grant.protocol_version != PROTOCOL_VERSION
        || grant.action_id != request.action_id
        || grant.capability_id != request.capability_id
        || !valid_identity(&grant.lease_id, "execution-lease-v3:sha256:")
        || grant.workload_identity != request.workload_identity
        || !valid_identity(&grant.workload_identity, "workload-v1:sha256:")
    {
        return Err(RailError::message("distributed execution lease grant is invalid"));
    }
    Ok(())
}

fn connect_worker_endpoint(endpoint: &str) -> RailResult<TcpStream> {
    let address = endpoint
        .parse::<SocketAddr>()
        .map_err(|_| RailError::message("distributed worker endpoint must be an explicit socket address"))?;
    TcpStream::connect_timeout(&address, NETWORK_HANDSHAKE_TIMEOUT).map_err(Into::into)
}

fn mutual_tls_client_config(identity: &MutualTlsClientIdentity<'_>) -> RailResult<Arc<rustls::ClientConfig>> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let roots = certificate_roots(identity.authority_certificate)?;
    let certificates = certificate_chain(identity.client_certificate)?;
    let private_key = private_key(identity.client_private_key)?;
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|error| RailError::message(format!("distributed TLS protocol policy is invalid: {error}")))?
        .with_root_certificates(roots)
        .with_client_auth_cert(certificates, private_key)
        .map_err(|error| RailError::message(format!("distributed TLS client identity is invalid: {error}")))?;
    config.alpn_protocols = vec![b"cargo-rail-execution/3".to_vec()];
    Ok(Arc::new(config))
}

fn client_workload_identity(identity: &MutualTlsClientIdentity<'_>) -> RailResult<String> {
    let certificates = certificate_chain(identity.client_certificate)?;
    certificates
        .first()
        .map(certificate_workload_identity)
        .ok_or_else(|| RailError::message("distributed TLS client identity contains no leaf certificate"))
}

fn peer_workload_identity(certificates: Option<&[rustls::pki_types::CertificateDer<'static>]>) -> RailResult<String> {
    certificates
        .and_then(|chain| chain.first())
        .map(certificate_workload_identity)
        .ok_or_else(|| RailError::message("distributed TLS peer certificate is unavailable after authentication"))
}

fn certificate_workload_identity(certificate: &rustls::pki_types::CertificateDer<'_>) -> String {
    format!("workload-v1:sha256:{}", ContentDigest::sha256(certificate.as_ref()))
}

pub(crate) fn validate_mutual_tls_client_identity(
    server_name: &str,
    worker_capability_id: &str,
    authority_certificate: &Path,
    client_certificate: &Path,
    client_private_key: &Path,
) -> RailResult<()> {
    if !worker_capability_identity_is_valid(worker_capability_id) {
        return Err(RailError::message("distributed worker capability identity is invalid"));
    }
    mutual_tls_client_config(&MutualTlsClientIdentity {
        endpoint: "0.0.0.0:0",
        server_name,
        worker_capability_id,
        authority_certificate,
        client_certificate,
        client_private_key,
    })?;
    Ok(())
}

fn mutual_tls_server_config(
    certificate: &Path,
    private_key_path: &Path,
    client_authority: &Path,
) -> RailResult<Arc<rustls::ServerConfig>> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let roots = certificate_roots(client_authority)?;
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(Arc::new(roots), Arc::clone(&provider))
        .build()
        .map_err(|error| RailError::message(format!("distributed TLS client authority is invalid: {error}")))?;
    let certificates = certificate_chain(certificate)?;
    let private_key = private_key(private_key_path)?;
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|error| RailError::message(format!("distributed TLS protocol policy is invalid: {error}")))?
        .with_client_cert_verifier(verifier)
        .with_single_cert(certificates, private_key)
        .map_err(|error| RailError::message(format!("distributed TLS server identity is invalid: {error}")))?;
    config.alpn_protocols = vec![b"cargo-rail-execution/3".to_vec()];
    Ok(Arc::new(config))
}

fn certificate_roots(path: &Path) -> RailResult<rustls::RootCertStore> {
    let certificates = certificate_chain(path)?;
    let mut roots = rustls::RootCertStore::empty();
    for certificate in certificates {
        roots
            .add(certificate)
            .map_err(|error| RailError::message(format!("distributed TLS authority is invalid: {error}")))?;
    }
    if roots.is_empty() {
        return Err(RailError::message("distributed TLS authority contains no certificates"));
    }
    Ok(roots)
}

fn certificate_chain(path: &Path) -> RailResult<Vec<rustls::pki_types::CertificateDer<'static>>> {
    use rustls::pki_types::pem::PemObject as _;

    let bytes = read_identity_file(path, 256 * 1024, false)?;
    let certificates = rustls::pki_types::CertificateDer::pem_slice_iter(&bytes)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| RailError::message(format!("distributed TLS certificate is malformed: {error}")))?;
    if certificates.is_empty() || certificates.len() > 16 {
        return Err(RailError::message(
            "distributed TLS certificate chain is outside its bound",
        ));
    }
    Ok(certificates)
}

fn private_key(path: &Path) -> RailResult<rustls::pki_types::PrivateKeyDer<'static>> {
    use rustls::pki_types::pem::PemObject as _;

    let bytes = read_identity_file(path, 64 * 1024, true)?;
    rustls::pki_types::PrivateKeyDer::from_pem_slice(&bytes)
        .map_err(|error| RailError::message(format!("distributed TLS private key is malformed: {error}")))
}

fn read_identity_file(path: &Path, maximum: u64, private: bool) -> RailResult<Zeroizing<Vec<u8>>> {
    let metadata = fs::symlink_metadata(path)?;
    if !path.is_absolute()
        || !metadata.is_file()
        || crate::utils::is_symlink_or_reparse(&metadata)
        || metadata.len() == 0
        || metadata.len() > maximum
    {
        return Err(RailError::message(
            "distributed TLS identity file is not a bounded real file",
        ));
    }
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt as _;

        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(RailError::message(
                "distributed TLS private key permissions are not private",
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = private;
    let mut file = File::open(path)?;
    if !crate::utils::private_file_matches_path(&file, path, metadata.len())? {
        return Err(RailError::message(
            "distributed TLS identity file changed before it was opened",
        ));
    }
    let capacity = usize::try_from(metadata.len())
        .map_err(|_| RailError::message("distributed TLS identity file exceeds this platform"))?;
    let mut bytes = Zeroizing::new(Vec::with_capacity(capacity));
    (&mut file).take(maximum.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.len() || !crate::utils::private_file_matches_path(&file, path, metadata.len())? {
        return Err(RailError::message(
            "distributed TLS identity file changed while it was read",
        ));
    }
    Ok(bytes)
}

fn write_control_frame<T: Serialize>(
    writer: &mut impl Write,
    magic: &[u8; 8],
    trailer: &[u8; 8],
    value: &T,
) -> RailResult<()> {
    let encoded = canonical_json(value)?;
    write_sized_header(writer, magic, &encoded)?;
    writer.write_all(trailer)?;
    writer.flush()?;
    Ok(())
}

fn read_control_frame<T: for<'de> Deserialize<'de> + Serialize>(
    reader: &mut impl Read,
    magic: &[u8; 8],
    trailer: &[u8; 8],
    role: &str,
) -> RailResult<T> {
    read_magic(reader, magic, role)?;
    let encoded = read_sized_bytes(reader, MAX_HEADER_BYTES, role)?;
    let value = serde_json::from_slice(&encoded)
        .map_err(|_| RailError::message(format!("distributed execution {role} is malformed")))?;
    if canonical_json(&value)? != encoded {
        return Err(RailError::message(format!(
            "distributed execution {role} is not canonical"
        )));
    }
    read_magic(reader, trailer, role)?;
    Ok(value)
}

fn qualify_local_client(rustc: &OsStr) -> RailResult<()> {
    let worker = crate::utils::canonicalize_existing(&std::env::current_exe()?)?;
    let staging_parent = tempfile::Builder::new()
        .prefix("cargo-rail-distributed-client-qualification-")
        .tempdir()?;
    let staging = NativeResultStaging::temporary_in(staging_parent.path())?;
    let candidate = RustLibraryCandidate::new(
        RustLibraryCandidateInput {
            crate_name: "cargo_rail_distributed_qualification".to_string(),
            crate_type: "rlib".to_string(),
            dep_info_name: "cargo_rail_distributed_qualification-c0dec0dec0dec0de.d".to_string(),
            edition: "2024".to_string(),
            emission: RustLibraryEmission::MetadataAndLink,
            metadata: "c0dec0dec0dec0de".to_string(),
            metadata_name: "libcargo_rail_distributed_qualification-c0dec0dec0dec0de.rmeta".to_string(),
            extra_filename: "-c0dec0dec0dec0de".to_string(),
            output_relative_directory: "target/debug/deps".to_string(),
            source_relative_path: "src/lib.rs".to_string(),
            test_mode: false,
            toolchain_proc_macro: false,
            rlib_name: Some("libcargo_rail_distributed_qualification-c0dec0dec0dec0de.rlib".to_string()),
            options: RustLibraryExecutionOptions::default(),
        },
        b"#![forbid(unsafe_code)]\npub fn qualified() -> bool { true }\n".to_vec(),
    )?;
    let mut timing = DistributedTiming::default();
    let LocalWorkerAttempt::Success(result) =
        execute_local_worker(&worker, rustc, &candidate, staging, None, &mut timing)
    else {
        return Err(RailError::message(
            "local distributed client qualification did not produce a successful staged result",
        ));
    };
    for slot in [
        DistributedResultSlot::DepInfo,
        DistributedResultSlot::Metadata,
        DistributedResultSlot::Rlib,
        DistributedResultSlot::Stderr,
        DistributedResultSlot::Stdout,
    ] {
        let path = result
            .frame(slot)
            .ok_or_else(|| RailError::message("local distributed client qualification lost a result slot"))?;
        let metadata = fs::symlink_metadata(path)?;
        if !path.starts_with(result.staging_path())
            || !metadata.is_file()
            || crate::utils::is_symlink_or_reparse(&metadata)
        {
            return Err(RailError::message(
                "local distributed client qualification result escaped private staging",
            ));
        }
    }
    println!("{PROTOCOL_VERSION}");
    Ok(())
}

/// Capture the exact local compiler capability the worker must match.
///
/// `cache` is the caller's already open local cache. Supplying it selects the
/// existing revalidating sysroot identity memo instead of rehashing the whole
/// sysroot on every attempt; the memo is only trusted when the exact sysroot
/// evidence still matches, so this is the same authority the native cache
/// session already uses for the same fact. Contexts without a local cache, such
/// as the worker itself, capture once per process and pass `None`.
fn capture_worker_capability(
    rustc: &OsStr,
    cache: Option<&crate::cache::cas::LocalCas>,
) -> RailResult<CapturedWorkerCapability> {
    capture_worker_capability_for_runtime(rustc, WorkerRuntime::ProcessOnly, cache)
}

#[cfg(target_os = "linux")]
impl CgroupV2Root {
    fn prepare() -> RailResult<Self> {
        if fs::read_dir("/proc/self/task")?.count() != 1 {
            return Err(RailError::message(
                "distributed cgroup authority must be captured before the worker creates threads",
            ));
        }
        let root = current_unified_cgroup_path()?;
        require_cgroup_controllers(&root, ["cpu", "memory", "pids"])?;
        let supervisor = root.join("cargo-rail-supervisor");
        let attempts = root.join("cargo-rail-attempts");
        ensure_cgroup_directory(&supervisor)?;
        ensure_cgroup_directory(&attempts)?;
        if !read_cgroup_value(&supervisor.join("cgroup.procs"))?.is_empty()
            || !read_cgroup_value(&attempts.join("cgroup.procs"))?.is_empty()
        {
            return Err(RailError::message(
                "distributed cgroup authority contains another live owner",
            ));
        }
        cleanup_stale_cgroup_attempts(&attempts)?;
        write_cgroup_value(&supervisor.join("cgroup.procs"), &std::process::id().to_string())?;
        if current_unified_cgroup_path()? != supervisor {
            return Err(RailError::message(
                "distributed worker did not enter its delegated supervisor cgroup",
            ));
        }
        enable_cgroup_controllers(&root, ["cpu", "memory", "pids"])?;
        enable_cgroup_controllers(&attempts, ["cpu", "memory", "pids"])?;
        Ok(Self { attempts })
    }

    fn create_attempt(&self, limits: ExecutionLimits) -> RailResult<CgroupV2Attempt> {
        validate_limits(limits)?;
        let mut nonce = [0_u8; 32];
        getrandom::fill(&mut nonce)
            .map_err(|error| RailError::message(format!("distributed cgroup nonce generation failed: {error}")))?;
        let path = self.attempts.join(format!("attempt-{}", ContentDigest::sha256(&nonce)));
        fs::create_dir(&path)?;
        if let Err(error) = configure_cgroup_attempt(&path, limits) {
            drop(fs::remove_dir(&path));
            return Err(error);
        }
        Ok(CgroupV2Attempt { path, armed: true })
    }

    fn validate_idle(&self) -> RailResult<()> {
        if !read_cgroup_value(&self.attempts.join("cgroup.procs"))?.is_empty() {
            return Err(RailError::message(
                "distributed cgroup authority retained an execution attempt",
            ));
        }
        for entry in fs::read_dir(&self.attempts)? {
            if entry?.file_type()?.is_dir() {
                return Err(RailError::message(
                    "distributed cgroup authority retained an execution attempt",
                ));
            }
        }
        let supervisor = self
            .attempts
            .parent()
            .ok_or_else(|| RailError::message("distributed cgroup attempts have no parent"))?
            .join("cargo-rail-supervisor");
        if current_unified_cgroup_path()? != supervisor
            || read_cgroup_value(&supervisor.join("cgroup.procs"))? != std::process::id().to_string()
        {
            return Err(RailError::message(
                "distributed cgroup supervisor did not retain exact ownership",
            ));
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl CgroupV2Attempt {
    fn finish(mut self) -> RailResult<CgroupV2Outcome> {
        kill_cgroup(&self.path)?;
        wait_for_empty_cgroup(&self.path)?;
        let outcome = CgroupV2Outcome {
            cpu_throttles: read_cgroup_event(&self.path, "cpu.stat", "nr_throttled")?,
            memory_oom_kills: read_cgroup_event(&self.path, "memory.events.local", "oom_kill")?,
            process_limit_hits: read_cgroup_event(&self.path, "pids.events.local", "max")?,
        };
        fs::remove_dir(&self.path)?;
        self.armed = false;
        Ok(outcome)
    }
}

#[cfg(target_os = "linux")]
impl Drop for CgroupV2Attempt {
    fn drop(&mut self) {
        if self.armed {
            drop(kill_cgroup(&self.path));
            drop(wait_for_empty_cgroup(&self.path));
            drop(fs::remove_dir(&self.path));
        }
    }
}

#[cfg(target_os = "linux")]
fn current_unified_cgroup_path() -> RailResult<PathBuf> {
    let membership = fs::read_to_string("/proc/self/cgroup")?;
    let mut memberships = membership.lines().filter_map(|line| line.strip_prefix("0::"));
    let relative = memberships
        .next()
        .ok_or_else(|| RailError::message("distributed worker is not in a unified cgroup-v2 hierarchy"))?;
    if memberships.next().is_some() || !relative.starts_with('/') {
        return Err(RailError::message(
            "distributed worker cgroup-v2 membership is ambiguous",
        ));
    }
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")?;
    let mut mounts = mountinfo.lines().filter_map(|line| {
        let (before, after) = line.split_once(" - ")?;
        (after.split_whitespace().next() == Some("cgroup2")).then(|| {
            let fields = before.split_whitespace().collect::<Vec<_>>();
            (fields.get(3).copied(), fields.get(4).copied())
        })
    });
    let Some((Some("/"), Some(mount))) = mounts.next() else {
        return Err(RailError::message(
            "distributed worker has no host-rooted cgroup-v2 mount",
        ));
    };
    if mounts.next().is_some() {
        return Err(RailError::message(
            "distributed worker cgroup-v2 mount authority is ambiguous",
        ));
    }
    let mount = decode_mountinfo_path(mount)?;
    Ok(crate::utils::canonicalize_existing(
        &mount.join(relative.trim_start_matches('/')),
    )?)
}

#[cfg(target_os = "linux")]
fn decode_mountinfo_path(encoded: &str) -> RailResult<PathBuf> {
    use std::os::unix::ffi::OsStringExt as _;

    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            let Some(octal) = bytes.get(index + 1..index + 4) else {
                return Err(RailError::message("distributed cgroup mount path escape is truncated"));
            };
            if !octal.iter().all(u8::is_ascii_digit) || octal.iter().any(|byte| *byte > b'7') {
                return Err(RailError::message("distributed cgroup mount path escape is invalid"));
            }
            decoded.push((octal[0] - b'0') * 64 + (octal[1] - b'0') * 8 + (octal[2] - b'0'));
            index += 4;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    Ok(PathBuf::from(OsString::from_vec(decoded)))
}

#[cfg(target_os = "linux")]
fn ensure_cgroup_directory(path: &Path) -> RailResult<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) || !path.join("cgroup.procs").is_file() {
        return Err(RailError::message(
            "distributed cgroup authority is not a real cgroup-v2 directory",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn cleanup_stale_cgroup_attempts(attempts: &Path) -> RailResult<()> {
    for entry in fs::read_dir(attempts)? {
        let entry = entry?;
        let metadata = entry.file_type()?;
        if !metadata.is_dir() {
            continue;
        }
        if !entry.file_name().to_str().is_some_and(valid_cgroup_attempt_name) {
            return Err(RailError::message(
                "distributed cgroup attempts authority contains an unknown child",
            ));
        }
        let path = entry.path();
        kill_cgroup(&path)?;
        wait_for_empty_cgroup(&path)?;
        fs::remove_dir(path)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn require_cgroup_controllers<const N: usize>(root: &Path, expected: [&str; N]) -> RailResult<()> {
    let controllers = read_cgroup_value(&root.join("cgroup.controllers"))?;
    if expected
        .iter()
        .any(|expected| !controllers.split_whitespace().any(|actual| actual == *expected))
    {
        return Err(RailError::message(
            "distributed worker cgroup delegation lacks cpu, memory, or pids authority",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn enable_cgroup_controllers<const N: usize>(root: &Path, expected: [&str; N]) -> RailResult<()> {
    let command = expected.map(|controller| format!("+{controller}"));
    write_cgroup_value(&root.join("cgroup.subtree_control"), &command.join(" "))?;
    let enabled = read_cgroup_value(&root.join("cgroup.subtree_control"))?;
    if expected
        .iter()
        .any(|expected| !enabled.split_whitespace().any(|actual| actual == *expected))
    {
        return Err(RailError::message(
            "distributed worker failed to enable its delegated cgroup controllers",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn configure_cgroup_attempt(path: &Path, limits: ExecutionLimits) -> RailResult<()> {
    let values = [
        (
            "cpu.max",
            format!("{} {}", limits.cpu_quota_micros, limits.cpu_period_micros),
        ),
        ("memory.max", limits.memory_bytes.to_string()),
        ("memory.swap.max", "0".to_string()),
        ("memory.oom.group", "1".to_string()),
        ("pids.max", limits.max_processes.to_string()),
        ("cgroup.max.depth", "0".to_string()),
        ("cgroup.max.descendants", "0".to_string()),
    ];
    for (name, value) in values {
        let control = path.join(name);
        write_cgroup_value(&control, &value)?;
        if read_cgroup_value(&control)? != value {
            return Err(RailError::message(format!(
                "distributed cgroup control '{name}' did not retain its exact value"
            )));
        }
    }
    let zswap = path.join("memory.zswap.writeback");
    if zswap.exists() {
        write_cgroup_value(&zswap, "0")?;
        if read_cgroup_value(&zswap)? != "0" {
            return Err(RailError::message("distributed cgroup did not disable zswap writeback"));
        }
    }
    for required in [
        "cgroup.kill",
        "cgroup.procs",
        "cpu.stat",
        "memory.events.local",
        "pids.events.local",
    ] {
        if !path.join(required).is_file() {
            return Err(RailError::message(format!(
                "distributed cgroup is missing required control '{required}'"
            )));
        }
    }
    validate_cgroup_attempt_controls(path, limits)
}

#[cfg(target_os = "linux")]
fn validate_cgroup_attempt_controls(path: &Path, limits: ExecutionLimits) -> RailResult<()> {
    let expected = [
        (
            "cpu.max",
            format!("{} {}", limits.cpu_quota_micros, limits.cpu_period_micros),
        ),
        ("memory.max", limits.memory_bytes.to_string()),
        ("memory.swap.max", "0".to_string()),
        ("memory.oom.group", "1".to_string()),
        ("pids.max", limits.max_processes.to_string()),
        ("cgroup.max.depth", "0".to_string()),
        ("cgroup.max.descendants", "0".to_string()),
    ];
    for (name, expected) in expected {
        if read_cgroup_value(&path.join(name))? != expected {
            return Err(RailError::message(format!(
                "distributed cgroup control '{name}' changed after configuration"
            )));
        }
    }
    let zswap = path.join("memory.zswap.writeback");
    if zswap.exists() && read_cgroup_value(&zswap)? != "0" {
        return Err(RailError::message(
            "distributed cgroup zswap authority changed after configuration",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn write_cgroup_value(path: &Path, value: &str) -> RailResult<()> {
    let mut file = OpenOptions::new().write(true).open(path)?;
    file.write_all(value.as_bytes())?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn read_cgroup_value(path: &Path) -> RailResult<String> {
    Ok(fs::read_to_string(path)?.trim().to_string())
}

#[cfg(target_os = "linux")]
fn read_cgroup_event(path: &Path, file: &str, key: &str) -> RailResult<u64> {
    parse_cgroup_event(&read_cgroup_value(&path.join(file))?, key)
        .ok_or_else(|| RailError::message(format!("distributed cgroup event '{key}' is unavailable")))
}

#[cfg(target_os = "linux")]
fn parse_cgroup_event(events: &str, key: &str) -> Option<u64> {
    events.lines().find_map(|line| {
        let (name, value) = line.split_once(' ')?;
        (name == key).then(|| value.parse::<u64>().ok()).flatten()
    })
}

#[cfg(target_os = "linux")]
fn kill_cgroup(path: &Path) -> RailResult<()> {
    write_cgroup_value(&path.join("cgroup.kill"), "1")
}

#[cfg(target_os = "linux")]
fn wait_for_empty_cgroup(path: &Path) -> RailResult<()> {
    for _ in 0..1_000 {
        let events = read_cgroup_value(&path.join("cgroup.events"))?;
        if events.lines().any(|line| line == "populated 0") {
            return Ok(());
        }
        thread::sleep(WORKER_POLL_INTERVAL);
    }
    Err(RailError::message(
        "distributed cgroup remained populated after termination",
    ))
}

#[cfg(target_os = "linux")]
fn valid_cgroup_attempt_name(name: &str) -> bool {
    name.strip_prefix("attempt-").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

#[cfg(target_os = "linux")]
fn capture_bubblewrap_worker_capability(rustc: &OsStr, bubblewrap: &OsStr) -> RailResult<CapturedWorkerCapability> {
    let cgroup = CgroupV2Root::prepare()?;
    let current = std::env::current_dir()?;
    let executable = crate::executable::resolve_executable_path(bubblewrap, &current)?;
    let executable = crate::utils::canonicalize_existing(&executable)?;
    let metadata = fs::symlink_metadata(&executable)?;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    if !metadata.is_file()
        || crate::utils::is_symlink_or_reparse(&metadata)
        || metadata.uid() != 0
        || metadata.permissions().mode() & 0o6022 != 0
    {
        return Err(RailError::message(
            "distributed Bubblewrap executable must be a root-owned, non-setid, non-writable regular file",
        ));
    }
    let generation = crate::utils::stable_file_generation(&executable)
        .ok_or_else(|| RailError::message("distributed Bubblewrap executable has no stable file generation"))?;
    let version = exact_command_output(&executable, &["--version"], "Bubblewrap version identity")?;
    let version = String::from_utf8(version)
        .map_err(|_| RailError::message("distributed Bubblewrap version identity is not UTF-8"))?;
    if !version.trim().starts_with("bubblewrap 0.") {
        return Err(RailError::message(
            "distributed Bubblewrap version is outside the qualified major-version contract",
        ));
    }
    let executable_digest = digest_file(&executable, metadata.len())?;
    let worker = crate::utils::canonicalize_existing(&std::env::current_exe()?)?;
    let worker_metadata = fs::symlink_metadata(&worker)?;
    if !worker_metadata.is_file() || crate::utils::is_symlink_or_reparse(&worker_metadata) {
        return Err(RailError::message(
            "distributed worker executable is not a real regular file",
        ));
    }
    let worker_generation = crate::utils::stable_file_generation(&worker)
        .ok_or_else(|| RailError::message("distributed worker executable has no stable file generation"))?;
    let worker_digest = digest_file(&worker, worker_metadata.len())?;
    let isolation = canonical_json(&(
        "bubblewrap-linux-v2",
        version.trim(),
        executable_digest,
        worker_digest,
        worker_execution_limits(),
        "empty-root;ro-system-runtime;ro-toolchain;ro-worker;bounded-tmpfs-attempt;cgroup-v2-cpu-memory-pids;no-swap;no-zswap-writeback;private-proc-dev;no-network;no-host-ipc;no-host-pids;no-host-uts;no-nested-userns;no-capabilities;new-session;die-with-parent",
    ))?;
    capture_worker_capability_for_runtime(
        rustc,
        WorkerRuntime::Bubblewrap {
            cgroup,
            executable,
            generation,
            worker,
            worker_generation,
        },
        None,
    )
    .and_then(|mut captured| {
        captured.capability.isolation = WorkerIsolation::BubblewrapLinuxV2;
        captured.capability.isolation_identity = format!("isolation-v2:sha256:{}", ContentDigest::sha256(&isolation));
        captured.capability.filesystem_contract = "bubblewrap-bounded-tmpfs-v2".to_string();
        captured.capability.capability_id = capability_identity(&captured.capability)?;
        validate_capability(&captured.capability)?;
        Ok(captured)
    })
}

fn capture_worker_capability_for_runtime(
    rustc: &OsStr,
    runtime: WorkerRuntime,
    cache: Option<&crate::cache::cas::LocalCas>,
) -> RailResult<CapturedWorkerCapability> {
    let current = std::env::current_dir()?;
    let rustc = crate::executable::resolve_executable_path(rustc, &current)?;
    let rustc = crate::utils::canonicalize_existing(&rustc)?;
    let rustc_metadata = fs::symlink_metadata(&rustc)?;
    if !rustc_metadata.is_file() || crate::utils::is_symlink_or_reparse(&rustc_metadata) {
        return Err(RailError::message(
            "distributed worker rustc is not a real regular file",
        ));
    }
    let rustc_generation = crate::utils::stable_file_generation(&rustc)
        .ok_or_else(|| RailError::message("distributed worker rustc has no stable file generation"))?;
    let verbose = exact_command_output(&rustc, &["-vV"], "rustc verbose identity")?;
    let verbose =
        String::from_utf8(verbose).map_err(|_| RailError::message("distributed worker rustc identity is not UTF-8"))?;
    let host_target = verbose
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .filter(|target| !target.is_empty())
        .ok_or_else(|| RailError::message("distributed worker rustc identity has no host target"))?
        .to_string();
    let sysroot = exact_command_output(&rustc, &["--print=sysroot"], "rustc sysroot")?;
    let sysroot =
        String::from_utf8(sysroot).map_err(|_| RailError::message("distributed worker sysroot is not UTF-8"))?;
    let sysroot = crate::utils::canonicalize_existing(Path::new(sysroot.trim()))?;
    let memo = cache
        .and_then(|cache| crate::compiler::collector::compiler_sysroot_memo_path_in(cache, &sysroot, &host_target));
    let (sysroot_identity, _) =
        crate::compiler::collector::compiler_sysroot_fingerprint(&sysroot, &host_target, memo.as_deref())?;
    let rustc_content_digest = digest_file(&rustc, rustc_metadata.len())?;
    let mut capability = WorkerCapability {
        architecture: std::env::consts::ARCH.to_string(),
        capability_id: String::new(),
        endianness: if cfg!(target_endian = "little") {
            "little".to_string()
        } else {
            "big".to_string()
        },
        environment_contract: worker_environment_identity()?,
        filesystem_contract: "private-real-directory-v1".to_string(),
        host_target,
        isolation: WorkerIsolation::ProcessOnlyUnqualified,
        isolation_identity: process_isolation_identity()?,
        operating_system: std::env::consts::OS.to_string(),
        operation_classes: vec![OperationClass::RustLibrary],
        platform_family: std::env::consts::FAMILY.to_string(),
        protocol_version: PROTOCOL_VERSION,
        resource_limits: worker_execution_limits(),
        rustc_content_digest,
        rustc_verbose_version: verbose,
        sysroot_identity,
        working_directory_contract: "canonical-workspace-relative-remapped-v1".to_string(),
    };
    capability.capability_id = capability_identity(&capability)?;
    validate_capability(&capability)?;
    Ok(CapturedWorkerCapability {
        capability,
        rustc,
        rustc_generation,
        runtime,
        #[cfg(target_os = "linux")]
        sysroot,
    })
}

fn process_isolation_identity() -> RailResult<String> {
    let encoded = canonical_json(&(
        "process-only-unqualified-v2",
        worker_execution_limits(),
        MAX_TOTAL_OUTPUT_BYTES,
    ))?;
    Ok(format!("isolation-v2:sha256:{}", ContentDigest::sha256(&encoded)))
}

fn worker_environment_identity() -> RailResult<String> {
    let environment = BTreeMap::from([
        ("TEMP".to_string(), "<attempt-temp>".to_string()),
        ("TMP".to_string(), "<attempt-temp>".to_string()),
        ("TMPDIR".to_string(), "<attempt-temp>".to_string()),
    ]);
    #[cfg(windows)]
    let environment = {
        let mut environment = environment;
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            environment.insert(
                "SystemRoot".to_string(),
                format!("sha256:{}", ContentDigest::sha256(system_root.as_encoded_bytes())),
            );
        }
        environment
    };
    let encoded = canonical_json(&environment)?;
    Ok(format!("environment-v1:sha256:{}", ContentDigest::sha256(&encoded)))
}

fn capability_identity(capability: &WorkerCapability) -> RailResult<String> {
    let encoded = canonical_json(&(
        &capability.architecture,
        &capability.endianness,
        &capability.environment_contract,
        &capability.filesystem_contract,
        &capability.host_target,
        capability.isolation,
        &capability.isolation_identity,
        &capability.operating_system,
        &capability.operation_classes,
        &capability.platform_family,
        capability.protocol_version,
        capability.resource_limits,
        &capability.rustc_content_digest,
        &capability.rustc_verbose_version,
        &capability.sysroot_identity,
        &capability.working_directory_contract,
    ))?;
    Ok(format!(
        "worker-capability-v3:sha256:{}",
        ContentDigest::sha256(&encoded)
    ))
}

fn worker_execution_environment_matches(worker: &WorkerCapability, selected: &WorkerCapability) -> RailResult<bool> {
    validate_capability(worker)?;
    validate_capability(selected)?;
    Ok(worker.architecture == selected.architecture
        && worker.endianness == selected.endianness
        && worker.environment_contract == selected.environment_contract
        && worker.host_target == selected.host_target
        && worker.operating_system == selected.operating_system
        && worker.operation_classes == selected.operation_classes
        && worker.platform_family == selected.platform_family
        && worker.protocol_version == selected.protocol_version
        && worker.resource_limits == selected.resource_limits
        && worker.rustc_content_digest == selected.rustc_content_digest
        && worker.rustc_verbose_version == selected.rustc_verbose_version
        && worker.sysroot_identity == selected.sysroot_identity
        && worker.working_directory_contract == selected.working_directory_contract)
}

fn worker_isolation_allowed(capability: &WorkerCapability, allow_unqualified: bool) -> bool {
    match capability.isolation {
        WorkerIsolation::BubblewrapLinuxV2 => true,
        WorkerIsolation::ProcessOnlyUnqualified => allow_unqualified,
    }
}

fn validate_capability(capability: &WorkerCapability) -> RailResult<()> {
    let isolation_valid = match capability.isolation {
        WorkerIsolation::ProcessOnlyUnqualified => {
            capability.filesystem_contract == "private-real-directory-v1"
                && capability.isolation_identity == process_isolation_identity()?
        }
        WorkerIsolation::BubblewrapLinuxV2 => {
            capability.operating_system == "linux"
                && capability.filesystem_contract == "bubblewrap-bounded-tmpfs-v2"
                && valid_identity(&capability.isolation_identity, "isolation-v2:sha256:")
        }
    };
    if capability.protocol_version != PROTOCOL_VERSION
        || capability.operation_classes != [OperationClass::RustLibrary]
        || !isolation_valid
        || validate_limits(capability.resource_limits).is_err()
        || capability.architecture.is_empty()
        || capability.endianness.is_empty()
        || capability.environment_contract.is_empty()
        || capability.host_target.is_empty()
        || capability.operating_system.is_empty()
        || capability.platform_family.is_empty()
        || capability.rustc_verbose_version.is_empty()
        || !valid_identity(&capability.rustc_content_digest, "sha256:")
        || !valid_identity(&capability.sysroot_identity, "sha256:")
        || capability.working_directory_contract != "canonical-workspace-relative-remapped-v1"
        || capability.capability_id != capability_identity(capability)?
    {
        return Err(RailError::message("distributed worker capability is invalid"));
    }
    Ok(())
}

fn exact_command_output(program: &Path, arguments: &[&str], role: &str) -> RailResult<Vec<u8>> {
    let output = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| RailError::message(format!("failed to execute {role}: {error}")))?;
    if !output.status.success() || output.stdout.len() > MAX_HEADER_BYTES || output.stderr.len() > MAX_HEADER_BYTES {
        return Err(RailError::message(format!("distributed worker {role} query failed")));
    }
    Ok(output.stdout)
}

fn digest_file(path: &Path, expected_bytes: u64) -> RailResult<String> {
    let before = fs::symlink_metadata(path)?;
    if !before.is_file()
        || crate::utils::is_symlink_or_reparse(&before)
        || before.len() != expected_bytes
        || expected_bytes > MAX_TOTAL_OUTPUT_BYTES.saturating_mul(8)
    {
        return Err(RailError::message(format!(
            "distributed worker file '{}' is outside its bounded regular-file contract",
            path.display()
        )));
    }
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut read_bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        read_bytes = read_bytes.saturating_add(read as u64);
        if read_bytes > expected_bytes {
            return Err(RailError::message("distributed worker file grew while hashing"));
        }
        hasher.update(&buffer[..read]);
    }
    let after = fs::symlink_metadata(path)?;
    if read_bytes != expected_bytes
        || before.len() != after.len()
        || before.modified()? != after.modified()?
        || crate::utils::is_symlink_or_reparse(&after)
    {
        return Err(RailError::message("distributed worker file changed while hashing"));
    }
    Ok(format_sha256(hasher.finalize().into()))
}

fn format_sha256(bytes: [u8; 32]) -> String {
    let mut encoded = String::with_capacity(71);
    encoded.push_str("sha256:");
    for byte in bytes {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn canonical_json<T: Serialize>(value: &T) -> RailResult<Vec<u8>> {
    serde_json::to_vec(value).map_err(Into::into)
}

fn execute_once(rustc: &OsStr) -> RailResult<()> {
    let captured = capture_worker_capability(rustc, None)?;
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let envelope = read_request(&mut reader)?;
    drop(reader);
    validate_request(&envelope.request, &captured.capability)?;

    let (cancellation_sender, cancellation_receiver) = mpsc::sync_channel(1);
    let lease = envelope.request.lease_id.clone();
    let _cancellation_reader = thread::Builder::new()
        .name("cargo-rail-execution-cancellation".to_string())
        .spawn(move || {
            let cancellation = read_cancellation(stdin, &lease);
            drop(cancellation_sender.send(cancellation));
        })?;

    let attempt = tempfile::Builder::new()
        .prefix("cargo-rail-distributed-attempt-")
        .tempdir()?;
    let attempt_root = crate::utils::canonicalize_existing(attempt.path())?;
    let execution_started = Instant::now();
    let mut worker_timing = WorkerPhaseTiming {
        source_bytes: request_input_bytes(&envelope.request),
        ..WorkerPhaseTiming::default()
    };
    let prepared = execute_request(
        &captured,
        &envelope,
        &attempt_root,
        &cancellation_receiver,
        &mut worker_timing,
    )?;
    worker_timing.result_bytes = prepared.result_bytes();
    worker_timing.elapsed_ns = elapsed_nanos(execution_started);
    let response = match prepared {
        WorkerExecution::Success(frames) => successful_response(&envelope.request, frames, worker_timing),
        WorkerExecution::CompilerFailed { termination, frames } => {
            compiler_failure_response(&envelope.request, termination, frames, worker_timing)
        }
        WorkerExecution::Rejected(reason) => rejected_response(&envelope.request, reason, worker_timing),
    };
    let mut stdout = std::io::stdout().lock();
    write_response(&mut stdout, &envelope.request, &response.0, &response.1)?;
    stdout.flush()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn execute_sandboxed(rustc: &OsStr) -> RailResult<()> {
    validate_sandbox_runtime()?;
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let capability: WorkerCapability =
        read_control_frame(&mut reader, CAPABILITY_MAGIC, CAPABILITY_TRAILER, "sandbox capability")?;
    validate_capability(&capability)?;
    if capability.isolation != WorkerIsolation::BubblewrapLinuxV2 {
        return Err(RailError::message(
            "distributed sandbox received an unqualified isolation capability",
        ));
    }
    let envelope = read_request_with_staging_parent(&mut reader, Some(Path::new(VIRTUAL_ROOT)))?;
    drop(reader);
    validate_request(&envelope.request, &capability)?;
    let selected = capture_worker_capability(rustc, None)?;
    if !worker_execution_environment_matches(&capability, &selected.capability)? {
        return Err(RailError::message(
            "distributed sandbox compiler does not match its outer capability",
        ));
    }
    let captured = CapturedWorkerCapability {
        capability,
        rustc: selected.rustc,
        rustc_generation: selected.rustc_generation,
        runtime: WorkerRuntime::ProcessOnly,
        sysroot: selected.sysroot,
    };
    let (cancellation_sender, cancellation_receiver) = mpsc::sync_channel(1);
    let lease = envelope.request.lease_id.clone();
    let _cancellation_reader = thread::Builder::new()
        .name("cargo-rail-sandbox-cancellation".to_string())
        .spawn(move || {
            let cancellation = read_cancellation(stdin, &lease);
            drop(cancellation_sender.send(cancellation));
        })?;
    let attempt = Path::new(VIRTUAL_ROOT);
    let execution_started = Instant::now();
    let mut worker_timing = WorkerPhaseTiming {
        source_bytes: request_input_bytes(&envelope.request),
        ..WorkerPhaseTiming::default()
    };
    let prepared = execute_request_in_process(
        &captured,
        &envelope,
        attempt,
        &cancellation_receiver,
        &mut worker_timing,
    )?;
    worker_timing.result_bytes = prepared.result_bytes();
    worker_timing.elapsed_ns = elapsed_nanos(execution_started);
    let response = match prepared {
        WorkerExecution::Success(frames) => successful_response(&envelope.request, frames, worker_timing),
        WorkerExecution::CompilerFailed { termination, frames } => {
            compiler_failure_response(&envelope.request, termination, frames, worker_timing)
        }
        WorkerExecution::Rejected(reason) => rejected_response(&envelope.request, reason, worker_timing),
    };
    let mut stdout = std::io::stdout().lock();
    write_response(&mut stdout, &envelope.request, &response.0, &response.1)?;
    stdout.flush()?;
    Ok(())
}

enum WorkerExecution {
    Success(Vec<PreparedResponseFrame>),
    CompilerFailed {
        termination: CompilerTermination,
        frames: Vec<PreparedResponseFrame>,
    },
    Rejected(&'static str),
}

impl WorkerExecution {
    fn result_bytes(&self) -> u64 {
        let frames = match self {
            Self::Success(frames) | Self::CompilerFailed { frames, .. } => frames,
            Self::Rejected(_) => return 0,
        };
        frames
            .iter()
            .fold(0_u64, |total, frame| total.saturating_add(frame.descriptor.bytes))
    }
}

fn execute_request(
    captured: &CapturedWorkerCapability,
    envelope: &RequestEnvelope,
    attempt: &Path,
    cancellation: &Receiver<Cancellation>,
    timing: &mut WorkerPhaseTiming,
) -> RailResult<WorkerExecution> {
    match &captured.runtime {
        WorkerRuntime::ProcessOnly => execute_request_in_process(captured, envelope, attempt, cancellation, timing),
        #[cfg(target_os = "linux")]
        WorkerRuntime::Bubblewrap { .. } => {
            execute_bounded_bubblewrap_request(captured, envelope, attempt, cancellation, timing)
        }
    }
}

#[cfg(target_os = "linux")]
fn execute_bounded_bubblewrap_request(
    captured: &CapturedWorkerCapability,
    envelope: &RequestEnvelope,
    attempt: &Path,
    cancellation: &Receiver<Cancellation>,
    timing: &mut WorkerPhaseTiming,
) -> RailResult<WorkerExecution> {
    let WorkerRuntime::Bubblewrap {
        cgroup,
        executable,
        worker,
        ..
    } = &captured.runtime
    else {
        return Err(RailError::message(
            "distributed bounded execution received another runtime",
        ));
    };
    if !worker_runtime_generation_is_stable(captured) || !captured.rustc.starts_with(&captured.sysroot) {
        return Ok(WorkerExecution::Rejected("compiler_generation_changed"));
    }

    let resource_attempt = cgroup.create_attempt(envelope.request.limits)?;
    let mut command = Command::new(worker);
    command
        .arg("execute-cgroup-bubblewrap")
        .arg(&captured.rustc)
        .arg(executable)
        .arg(&captured.sysroot)
        .arg(worker)
        .arg(&resource_attempt.path)
        .arg(envelope.request.limits.scratch_bytes.to_string())
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    let mut child = command.spawn()?;
    let mut resource_attempt = Some(resource_attempt);
    let Some(mut stdin) = child.stdin.take() else {
        terminate_bounded_sandbox(&mut child, &mut resource_attempt);
        return Err(RailError::message("distributed sandbox stdin is unavailable"));
    };
    let Some(mut stdout) = child.stdout.take() else {
        terminate_bounded_sandbox(&mut child, &mut resource_attempt);
        return Err(RailError::message("distributed sandbox stdout is unavailable"));
    };
    let Some(stderr) = child.stderr.take() else {
        terminate_bounded_sandbox(&mut child, &mut resource_attempt);
        return Err(RailError::message("distributed sandbox stderr is unavailable"));
    };
    if let Err(error) = write_control_frame(&mut stdin, CAPABILITY_MAGIC, CAPABILITY_TRAILER, &captured.capability)
        .and_then(|()| write_envelope_request(&mut stdin, envelope))
    {
        terminate_bounded_sandbox(&mut child, &mut resource_attempt);
        return Err(error);
    }

    let expected = envelope.request.clone();
    let staging = NativeResultStaging::temporary_in(attempt)?;
    let response_reader = match thread::Builder::new()
        .name("cargo-rail-sandbox-response".to_string())
        .spawn(move || {
            let mut response_timing = ResponseTiming::default();
            let response = read_magic(&mut stdout, SANDBOX_READY_MAGIC, "sandbox readiness")
                .and_then(|()| read_response_into(&mut stdout, &expected, staging, &mut response_timing));
            (response, response_timing)
        }) {
        Ok(reader) => reader,
        Err(error) => {
            terminate_bounded_sandbox(&mut child, &mut resource_attempt);
            return Err(error.into());
        }
    };
    let stderr_reader = match thread::Builder::new()
        .name("cargo-rail-sandbox-stderr".to_string())
        .spawn(move || capture_stream(stderr, MAX_HEADER_BYTES as u64))
    {
        Ok(reader) => reader,
        Err(error) => {
            terminate_bounded_sandbox(&mut child, &mut resource_attempt);
            drop(stdin);
            drop(response_reader.join());
            return Err(error.into());
        }
    };

    let started = Instant::now();
    let mut rejected = None;
    let status = loop {
        match cancellation.try_recv() {
            Ok(Cancellation::Requested) => rejected = Some("execution_cancelled"),
            Ok(Cancellation::ClientLost) | Err(TryRecvError::Disconnected) => {
                rejected = Some("execution_client_lost");
            }
            Ok(Cancellation::Invalid) => rejected = Some("execution_cancellation_invalid"),
            Err(TryRecvError::Empty) => {}
        }
        if rejected.is_none() && started.elapsed() > Duration::from_millis(envelope.request.limits.wall_time_ms) {
            rejected = Some("execution_time_limit_exceeded");
        }
        if rejected.is_some() {
            drop(child.kill());
            break child.wait()?;
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                terminate_bounded_sandbox(&mut child, &mut resource_attempt);
                drop(stdin);
                drop(response_reader.join());
                drop(stderr_reader.join());
                return Err(error.into());
            }
        }
        thread::sleep(WORKER_POLL_INTERVAL);
    };
    drop(stdin);
    let outcome = resource_attempt
        .take()
        .ok_or_else(|| RailError::message("distributed sandbox lost its cgroup authority"))?
        .finish()?;
    let (response, response_timing) = response_reader
        .join()
        .map_err(|_| RailError::message("distributed sandbox response reader panicked"))?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| RailError::message("distributed sandbox stderr reader panicked"))??
        .ok_or_else(|| RailError::message("distributed sandbox stderr exceeded its byte bound"))?;

    if let Some(reason) = rejected {
        return Ok(WorkerExecution::Rejected(reason));
    }
    if outcome.memory_oom_kills > 0 {
        return Ok(WorkerExecution::Rejected("execution_memory_limit_exceeded"));
    }
    if outcome.process_limit_hits > 0 {
        return Ok(WorkerExecution::Rejected("execution_process_limit_exceeded"));
    }
    if !status.success() || !stderr.is_empty() {
        return Err(RailError::message(format!(
            "distributed bounded sandbox failed: {}",
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    let decoded = response?;
    timing.input_ns = response_timing.worker.input_ns;
    timing.compiler_ns = response_timing.worker.compiler_ns;
    timing.result_encode_ns = response_timing.worker.result_encode_ns;
    decoded_execution_into_worker(decoded, attempt)
}

#[cfg(target_os = "linux")]
fn decoded_execution_into_worker(decoded: DecodedExecution, attempt: &Path) -> RailResult<WorkerExecution> {
    let (termination, result) = match decoded {
        DecodedExecution::Success(result) => (None, result),
        DecodedExecution::CompilerFailed { termination, result } => (Some(termination), result),
        DecodedExecution::Rejected => return Ok(WorkerExecution::Rejected("sandbox_execution_rejected")),
    };
    let result_root = attempt.join("sandbox-result");
    fs::create_dir(&result_root)?;
    let StagedExecutionResult {
        staging,
        frames,
        descriptors,
        ..
    } = result;
    let mut prepared = Vec::with_capacity(frames.len());
    for (slot, descriptor) in descriptors {
        let source = frames
            .get(&slot)
            .ok_or_else(|| RailError::message("distributed sandbox result lost a frame"))?;
        if !source.starts_with(staging.path()) {
            return Err(RailError::message("distributed sandbox result escaped trusted staging"));
        }
        let destination = result_root.join(slot.file_name());
        fs::rename(source, &destination)?;
        prepared.push(PreparedResponseFrame {
            descriptor,
            payload: ResponsePayload::File(destination),
        });
    }
    match termination {
        Some(termination) => Ok(WorkerExecution::CompilerFailed {
            termination,
            frames: prepared,
        }),
        None => Ok(WorkerExecution::Success(prepared)),
    }
}

#[cfg(target_os = "linux")]
fn terminate_bounded_sandbox(child: &mut Child, resource_attempt: &mut Option<CgroupV2Attempt>) {
    drop(child.kill());
    if let Some(resource_attempt) = resource_attempt.take() {
        drop(resource_attempt.finish());
    }
    drop(child.wait());
}

fn execute_request_in_process(
    captured: &CapturedWorkerCapability,
    envelope: &RequestEnvelope,
    attempt: &Path,
    cancellation: &Receiver<Cancellation>,
    timing: &mut WorkerPhaseTiming,
) -> RailResult<WorkerExecution> {
    if !worker_runtime_generation_is_stable(captured) {
        return Ok(WorkerExecution::Rejected("compiler_generation_changed"));
    }
    let input_started = Instant::now();
    let workspace_directory = attempt.join("workspace");
    let output_directory = workspace_directory.join(&envelope.request.operation.output_relative_directory);
    let temporary_directory = attempt.join("tmp");
    fs::create_dir(&workspace_directory)?;
    fs::create_dir_all(&output_directory)?;
    fs::create_dir(&temporary_directory)?;
    let source_relative = source_relative_path(&envelope.request.operation.source_virtual_path)
        .ok_or_else(|| RailError::message("distributed execution source path is invalid"))?;
    let mut staged_dependencies = BTreeMap::new();
    for frame in &envelope.request.inputs {
        let staged = envelope
            .inputs
            .get(&frame.virtual_path)
            .ok_or_else(|| RailError::message("distributed execution input staging is incomplete"))?;
        let destination = match frame.kind {
            InputKind::Source => workspace_directory.join(
                source_relative_path(&frame.virtual_path)
                    .ok_or_else(|| RailError::message("distributed execution source input path is invalid"))?,
            ),
            InputKind::Dependency => {
                let relative = frame
                    .virtual_path
                    .strip_prefix(VIRTUAL_DEPENDENCIES)
                    .and_then(|relative| relative.strip_prefix('/'))
                    .ok_or_else(|| RailError::message("distributed execution dependency input path is invalid"))?;
                attempt.join("dependencies").join(relative)
            }
        };
        copy_staged_input(staged, &destination, frame)?;
        if frame.kind == InputKind::Dependency {
            staged_dependencies.insert(frame.virtual_path.as_str(), destination);
        }
    }
    let outputs = output_paths(&envelope.request.operation, &output_directory)?;
    let dependencies = envelope
        .request
        .operation
        .dependencies
        .iter()
        .map(|dependency| {
            staged_dependencies
                .get(dependency.virtual_path.as_str())
                .map(|path| (dependency.extern_name.as_str(), path.as_path()))
                .ok_or_else(|| RailError::message("distributed execution dependency staging is incomplete"))
        })
        .collect::<RailResult<Vec<_>>>()?;
    let command = worker_compiler_command(
        captured,
        &envelope.request.operation,
        Path::new(source_relative),
        &outputs,
        &workspace_directory,
        &temporary_directory,
        &dependencies,
    )?;
    timing.input_ns = elapsed_nanos(input_started);
    let compiler_started = Instant::now();
    let run = run_compiler(
        command,
        cancellation,
        envelope.request.limits.max_stream_bytes,
        Duration::from_millis(envelope.request.limits.wall_time_ms),
    )?;
    timing.compiler_ns = elapsed_nanos(compiler_started);
    let encode_started = Instant::now();
    let CapturedCompilerOutput {
        status,
        mut stdout,
        mut stderr,
    } = match run {
        CompilerRun::Completed(output) => output,
        CompilerRun::Cancelled(reason) | CompilerRun::Failed(reason) => return Ok(WorkerExecution::Rejected(reason)),
    };
    rebind_compiler_stream(&mut stdout, &workspace_directory, attempt)?;
    rebind_compiler_stream(&mut stderr, &workspace_directory, attempt)?;
    if !status.success() {
        let frames = vec![
            prepare_bytes_frame(ResponseSlot::Stderr, stderr, envelope.request.limits.max_stream_bytes)?,
            prepare_bytes_frame(ResponseSlot::Stdout, stdout, envelope.request.limits.max_stream_bytes)?,
        ];
        timing.result_encode_ns = elapsed_nanos(encode_started);
        return Ok(WorkerExecution::CompilerFailed {
            termination: compiler_termination(&status),
            frames,
        });
    }
    let mut allowed_targets = vec![outputs.dep_info.as_path(), outputs.metadata.as_path()];
    if let Some(rlib) = &outputs.rlib {
        allowed_targets.push(rlib);
    }
    validate_and_rebind_dep_info(
        &outputs.dep_info,
        &workspace_directory,
        attempt,
        &envelope.request.inputs,
        &envelope.request.operation.source_virtual_path,
        &allowed_targets,
    )?;
    let mut frames = vec![
        prepare_file_frame(
            ResponseSlot::DepInfo,
            outputs.dep_info,
            envelope.request.limits.max_output_bytes,
        )?,
        prepare_file_frame(
            ResponseSlot::Metadata,
            outputs.metadata,
            envelope.request.limits.max_output_bytes,
        )?,
    ];
    if let Some(rlib) = outputs.rlib {
        frames.push(prepare_file_frame(
            ResponseSlot::Rlib,
            rlib,
            envelope.request.limits.max_output_bytes,
        )?);
    }
    frames.extend([
        prepare_bytes_frame(ResponseSlot::Stderr, stderr, envelope.request.limits.max_stream_bytes)?,
        prepare_bytes_frame(ResponseSlot::Stdout, stdout, envelope.request.limits.max_stream_bytes)?,
    ]);
    let total = frames.iter().try_fold(0_u64, |total, frame| {
        total
            .checked_add(frame.descriptor.bytes)
            .ok_or_else(|| RailError::message("distributed execution result size overflowed"))
    })?;
    if total > MAX_TOTAL_OUTPUT_BYTES {
        return Ok(WorkerExecution::Rejected("result_size_limit_exceeded"));
    }
    timing.result_encode_ns = elapsed_nanos(encode_started);
    Ok(WorkerExecution::Success(frames))
}

fn elapsed_nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn elapsed_nanos_between(started: Instant, finished: Instant) -> u64 {
    u64::try_from(finished.saturating_duration_since(started).as_nanos()).unwrap_or(u64::MAX)
}

fn rebind_compiler_stream(stream: &mut Vec<u8>, workspace: &Path, attempt: &Path) -> RailResult<()> {
    *stream = rebind_path_spellings(stream, workspace, VIRTUAL_WORKSPACE);
    if physical_worker_root_remains(stream, attempt) {
        return Err(RailError::message(
            "distributed compiler stream retained its worker root",
        ));
    }
    Ok(())
}

fn compiler_termination(status: &ExitStatus) -> CompilerTermination {
    if let Some(code) = status.code() {
        return CompilerTermination::Exit { code };
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;

        if let Some(signal) = status.signal() {
            return CompilerTermination::Signal { signal };
        }
    }
    CompilerTermination::Unknown
}

struct CompilerCommandInput<'a> {
    rustc: &'a OsStr,
    operation: &'a RustLibraryOperation,
    source_relative: &'a Path,
    outputs: &'a OutputPaths,
    workspace: &'a Path,
    temporary: &'a Path,
    dependencies: &'a [(&'a str, &'a Path)],
    inherit_environment: bool,
}

fn compiler_command(input: CompilerCommandInput<'_>) -> RailResult<Command> {
    let CompilerCommandInput {
        rustc,
        operation,
        source_relative,
        outputs,
        workspace,
        temporary,
        dependencies,
        inherit_environment,
    } = input;
    let output_directory = outputs
        .dep_info
        .parent()
        .ok_or_else(|| RailError::message("distributed compiler dep-info has no output directory"))?;
    let mut command = Command::new(rustc);
    command.arg(source_relative).args([
        "--crate-name",
        &operation.crate_name,
        "--crate-type",
        match operation.crate_type {
            RustLibraryCrateType::Bin => "bin",
            RustLibraryCrateType::Cdylib => "cdylib",
            RustLibraryCrateType::Dylib => "dylib",
            RustLibraryCrateType::Lib => "lib",
            RustLibraryCrateType::ProcMacro => "proc-macro",
            RustLibraryCrateType::Rlib => "rlib",
            RustLibraryCrateType::Staticlib => "staticlib",
        },
        "--edition",
        &operation.edition,
    ]);
    if operation.test_mode {
        command.arg("--test");
    }
    if operation.cargo_json_diagnostics {
        command
            .arg("--error-format=json")
            .arg("--json=diagnostic-rendered-ansi,artifacts,future-incompat");
    }
    let emit = if let Some(rlib) = &outputs.rlib {
        format!(
            "dep-info={},metadata={},link={}",
            outputs.dep_info.display(),
            outputs.metadata.display(),
            rlib.display()
        )
    } else {
        format!(
            "dep-info={},metadata={}",
            outputs.dep_info.display(),
            outputs.metadata.display()
        )
    };
    command
        .arg("--out-dir")
        .arg(output_directory)
        .arg("--emit")
        .arg(emit)
        .args(rust_library_codegen_arguments(&operation.codegen));
    for cfg in &operation.cfg {
        command.arg("--cfg").arg(cfg);
    }
    for check_cfg in &operation.check_cfg {
        command.arg("--check-cfg").arg(check_cfg);
    }
    if let Some(cap_lints) = &operation.cap_lints {
        command.arg("--cap-lints").arg(cap_lints);
    }
    if let Some(color) = &operation.color {
        command.arg("--color").arg(color);
    }
    if let Some(width) = operation.diagnostic_width {
        command.arg("--diagnostic-width").arg(width.to_string());
    }
    for lint in &operation.lints {
        command
            .arg(match lint.level {
                RustLibraryLintLevel::Allow => "--allow",
                RustLibraryLintLevel::Deny => "--deny",
                RustLibraryLintLevel::Forbid => "--forbid",
                RustLibraryLintLevel::Warn => "--warn",
            })
            .arg(&lint.name);
    }
    for (extern_name, path) in dependencies {
        command.arg("--extern").arg(format!("{extern_name}={}", path.display()));
    }
    if operation.toolchain_proc_macro {
        command.arg("--extern").arg("proc_macro");
    }
    command
        .arg(format!("-Cmetadata={}", operation.metadata))
        .arg(format!("-Cextra-filename={}", operation.extra_filename));
    if operation.output_dependency_search {
        command
            .arg("-L")
            .arg(format!("dependency={}", output_directory.display()));
    }
    command
        .arg("--remap-path-prefix")
        .arg(format!("{}={VIRTUAL_WORKSPACE}", workspace.display()))
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !inherit_environment {
        command.env_clear();
    }
    command
        .env("TEMP", temporary)
        .env("TMP", temporary)
        .env("TMPDIR", temporary);
    #[cfg(windows)]
    {
        let system_root = std::env::var_os("SystemRoot")
            .ok_or_else(|| RailError::message("distributed Windows worker has no SystemRoot authority"))?;
        command.env("SystemRoot", system_root);
    }
    Ok(command)
}

fn worker_runtime_generation_is_stable(captured: &CapturedWorkerCapability) -> bool {
    if crate::utils::stable_file_generation(&captured.rustc).as_ref() != Some(&captured.rustc_generation) {
        return false;
    }
    match &captured.runtime {
        WorkerRuntime::ProcessOnly => true,
        #[cfg(target_os = "linux")]
        WorkerRuntime::Bubblewrap {
            executable,
            generation,
            worker,
            worker_generation,
            ..
        } => {
            crate::utils::stable_file_generation(executable).as_ref() == Some(generation)
                && crate::utils::stable_file_generation(worker).as_ref() == Some(worker_generation)
        }
    }
}

fn worker_compiler_command(
    captured: &CapturedWorkerCapability,
    operation: &RustLibraryOperation,
    source_relative: &Path,
    outputs: &OutputPaths,
    workspace: &Path,
    temporary: &Path,
    dependencies: &[(&str, &Path)],
) -> RailResult<Command> {
    match &captured.runtime {
        WorkerRuntime::ProcessOnly => compiler_command(CompilerCommandInput {
            rustc: captured.rustc.as_os_str(),
            operation,
            source_relative,
            outputs,
            workspace,
            temporary,
            dependencies,
            inherit_environment: false,
        }),
        #[cfg(target_os = "linux")]
        WorkerRuntime::Bubblewrap { .. } => Err(RailError::message(
            "distributed Bubblewrap execution bypassed its bounded supervisor",
        )),
    }
}

#[cfg(target_os = "linux")]
fn launch_cgroup_bubblewrap(
    rustc: &OsStr,
    bubblewrap: &OsStr,
    sysroot: &Path,
    worker: &Path,
    cgroup: &Path,
    scratch_bytes: &OsStr,
) -> RailResult<()> {
    use std::os::unix::process::CommandExt as _;

    let scratch_bytes = scratch_bytes
        .to_str()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value == worker_execution_limits().scratch_bytes)
        .ok_or_else(|| RailError::message("distributed sandbox scratch authority is invalid"))?;
    enter_cgroup_attempt(cgroup, worker_execution_limits())?;

    let rustc = crate::utils::canonicalize_existing(Path::new(rustc))?;
    let bubblewrap = crate::utils::canonicalize_existing(Path::new(bubblewrap))?;
    let sysroot = crate::utils::canonicalize_existing(sysroot)?;
    let worker = crate::utils::canonicalize_existing(worker)?;
    if !rustc.starts_with(&sysroot) || worker != crate::utils::canonicalize_existing(&std::env::current_exe()?)? {
        return Err(RailError::message(
            "distributed sandbox launcher executable authority changed",
        ));
    }
    std::io::stdout().write_all(SANDBOX_READY_MAGIC)?;
    std::io::stdout().flush()?;
    let mut command = bounded_bubblewrap_command(&bubblewrap, &sysroot, &worker, scratch_bytes)?;
    command
        .arg("--")
        .arg(VIRTUAL_WORKER)
        .arg("execute-sandboxed")
        .arg(rustc);
    Err(command.exec().into())
}

#[cfg(target_os = "linux")]
fn enter_cgroup_attempt(cgroup: &Path, limits: ExecutionLimits) -> RailResult<()> {
    let supervisor = current_unified_cgroup_path()?;
    if supervisor.file_name() != Some(OsStr::new("cargo-rail-supervisor")) {
        return Err(RailError::message(
            "distributed sandbox launcher is outside its supervisor cgroup",
        ));
    }
    let delegated = supervisor
        .parent()
        .ok_or_else(|| RailError::message("distributed sandbox supervisor has no delegated parent"))?;
    let attempts = crate::utils::canonicalize_existing(&delegated.join("cargo-rail-attempts"))?;
    let cgroup = crate::utils::canonicalize_existing(cgroup)?;
    cgroup
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|name| valid_cgroup_attempt_name(name))
        .ok_or_else(|| RailError::message("distributed sandbox attempt cgroup name is invalid"))?;
    if cgroup.parent() != Some(attempts.as_path()) || !read_cgroup_value(&cgroup.join("cgroup.procs"))?.is_empty() {
        return Err(RailError::message(
            "distributed sandbox attempt cgroup is outside its exact authority",
        ));
    }
    validate_cgroup_attempt_controls(&cgroup, limits)?;
    write_cgroup_value(&cgroup.join("cgroup.procs"), &std::process::id().to_string())?;
    if current_unified_cgroup_path()? != cgroup {
        return Err(RailError::message(
            "distributed sandbox launcher did not enter its attempt cgroup",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_cgroup_probe(probe: &OsStr, cgroup: &Path) -> RailResult<()> {
    let probe = match probe.to_str() {
        Some("cpu") => CgroupProbe::Cpu,
        Some("memory") => CgroupProbe::Memory,
        Some("processes") => CgroupProbe::Processes,
        _ => return Err(RailError::message("distributed cgroup probe is invalid")),
    };
    let limits = probe.limits();
    enter_cgroup_attempt(cgroup, limits)?;
    match probe {
        CgroupProbe::Cpu => {
            let started = Instant::now();
            let mut value = 0_u64;
            while started.elapsed() < Duration::from_millis(750) {
                value = std::hint::black_box(value.wrapping_add(1));
            }
            Ok(())
        }
        CgroupProbe::Memory => {
            let mut allocations = Vec::new();
            loop {
                let mut allocation = vec![0_u8; 1024 * 1024];
                for offset in (0..allocation.len()).step_by(4096) {
                    allocation[offset] = 0xa5;
                }
                allocations.push(allocation);
                std::hint::black_box(&allocations);
            }
        }
        CgroupProbe::Processes => {
            let executable = crate::utils::canonicalize_existing(&std::env::current_exe()?)?;
            let mut children = Vec::new();
            let mut limited = false;
            for _ in 0..limits.max_processes.saturating_add(2) {
                match Command::new(&executable)
                    .arg("probe-cgroup-idle")
                    .current_dir("/")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .env_clear()
                    .spawn()
                {
                    Ok(child) => children.push(child),
                    Err(_) => {
                        limited = true;
                        break;
                    }
                }
            }
            for child in &mut children {
                drop(child.kill());
            }
            for mut child in children {
                drop(child.wait());
            }
            if limited {
                Ok(())
            } else {
                Err(RailError::message(
                    "distributed process cgroup probe exceeded its task authority",
                ))
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn bounded_bubblewrap_command(
    bubblewrap: &Path,
    sysroot: &Path,
    worker: &Path,
    scratch_bytes: u64,
) -> RailResult<Command> {
    if scratch_bytes != worker_execution_limits().scratch_bytes {
        return Err(RailError::message(
            "distributed Bubblewrap scratch limit changed after validation",
        ));
    }
    let mut command = Command::new(bubblewrap);
    command.args([
        "--unshare-all",
        "--unshare-user",
        "--disable-userns",
        "--cap-drop",
        "ALL",
        "--new-session",
        "--die-with-parent",
        "--hostname",
        "cargo-rail-worker",
        "--clearenv",
    ]);
    for system_root in ["/usr", "/bin", "/lib", "/lib64", "/sbin"] {
        let system_root = Path::new(system_root);
        if system_root.is_dir() {
            command.arg("--ro-bind").arg(system_root).arg(system_root);
        }
    }
    let loader_cache = Path::new("/etc/ld.so.cache");
    if loader_cache.is_file() {
        command.arg("--ro-bind").arg(loader_cache).arg(loader_cache);
    }
    command
        .arg("--ro-bind")
        .arg(sysroot)
        .arg(sysroot)
        .args([
            "--dir",
            "/cargo-rail",
            "--dir",
            "/cargo-rail/exec",
            "--dir",
            VIRTUAL_ROOT,
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--remount-ro",
            "/",
            "--size",
        ])
        .arg(scratch_bytes.to_string())
        .arg("--tmpfs")
        .arg(VIRTUAL_ROOT)
        .arg("--ro-bind")
        .arg(worker)
        .arg(VIRTUAL_WORKER)
        .arg("--chdir")
        .arg("/")
        .current_dir("/")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .env_clear();
    Ok(command)
}

#[cfg(target_os = "linux")]
fn validate_sandbox_runtime() -> RailResult<()> {
    let environment = std::env::vars_os().collect::<BTreeMap<_, _>>();
    if environment != BTreeMap::from([(OsString::from("PWD"), OsString::from("/"))]) {
        return Err(RailError::message(
            "distributed sandbox runtime environment is not exact",
        ));
    }
    if Path::new("/etc/passwd").exists() {
        return Err(RailError::message(
            "distributed sandbox runtime exposed the host account database",
        ));
    }
    if Path::new("/sys").exists() {
        return Err(RailError::message(
            "distributed sandbox runtime exposed the host system filesystem",
        ));
    }
    if fs::read_to_string("/proc/sys/kernel/hostname")?.trim() != "cargo-rail-worker" {
        return Err(RailError::message("distributed sandbox runtime retained the host name"));
    }
    let status = fs::read_to_string("/proc/self/status")?;
    if status.lines().find_map(|line| line.strip_prefix("CapEff:\t")) != Some("0000000000000000") {
        return Err(RailError::message(
            "distributed sandbox retained effective capabilities",
        ));
    }
    let routes = fs::read_to_string("/proc/net/route")?;
    if routes.lines().skip(1).any(|line| {
        let mut fields = line.split_whitespace();
        let interface = fields.next();
        let destination = fields.next();
        interface != Some("lo") && destination == Some("00000000")
    }) {
        return Err(RailError::message(
            "distributed sandbox retained a non-loopback default route",
        ));
    }

    let root = crate::utils::canonicalize_existing(Path::new(VIRTUAL_ROOT))?;
    if root != Path::new(VIRTUAL_ROOT) {
        return Err(RailError::message(
            "distributed sandbox writable root changed through indirection",
        ));
    }
    let entries = fs::read_dir(&root)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if entries != BTreeSet::from([OsString::from("worker")]) {
        return Err(RailError::message(
            "distributed sandbox writable root was not empty at entry",
        ));
    }
    let worker = fs::symlink_metadata(VIRTUAL_WORKER)?;
    if !worker.is_file() || crate::utils::is_symlink_or_reparse(&worker) {
        return Err(RailError::message(
            "distributed sandbox worker mount is not a regular file",
        ));
    }

    let mountinfo = fs::read_to_string("/proc/self/mountinfo")?;
    let mut filesystem_root_read_only = false;
    let mut scratch_size = None;
    for line in mountinfo.lines() {
        let Some((before, after)) = line.split_once(" - ") else {
            continue;
        };
        let fields = before.split_whitespace().collect::<Vec<_>>();
        let Some(mountpoint) = fields.get(4) else {
            continue;
        };
        let mountpoint = decode_mountinfo_path(mountpoint)?;
        let options = fields.get(5).copied().unwrap_or_default();
        if mountpoint == Path::new("/") {
            filesystem_root_read_only = options.split(',').any(|option| option == "ro");
        }
        if mountpoint == root {
            let mut after = after.split_whitespace();
            if after.next() != Some("tmpfs") || !options.split(',').any(|option| option == "rw") {
                return Err(RailError::message(
                    "distributed sandbox writable root is not a writable tmpfs",
                ));
            }
            let _source = after.next();
            scratch_size = after
                .next()
                .and_then(|options| options.split(',').find_map(parse_tmpfs_size_option));
        }
    }
    if !filesystem_root_read_only || scratch_size != Some(worker_execution_limits().scratch_bytes) {
        return Err(RailError::message(
            "distributed sandbox filesystem limits are not exact",
        ));
    }
    let probe = root.join("write-proof");
    write_private_file(&probe, b"bounded tmpfs")?;
    fs::remove_file(probe)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn parse_tmpfs_size_option(option: &str) -> Option<u64> {
    let value = option.strip_prefix("size=")?;
    let suffix = value.chars().next_back()?;
    let (number, multiplier) = match suffix {
        'k' | 'K' => (value.strip_suffix(suffix)?, 1024_u64),
        'm' | 'M' => (value.strip_suffix(suffix)?, 1024_u64.pow(2)),
        'g' | 'G' => (value.strip_suffix(suffix)?, 1024_u64.pow(3)),
        character if character.is_ascii_digit() => (value, 1),
        _ => return None,
    };
    number.parse::<u64>().ok()?.checked_mul(multiplier)
}

fn rust_library_codegen_arguments(codegen: &RustLibraryCodegen) -> Vec<String> {
    let mut arguments = Vec::with_capacity(13);
    if let Some(opt_level) = &codegen.opt_level {
        arguments.push(format!("-Copt-level={opt_level}"));
    }
    if let Some(embed_bitcode) = codegen.embed_bitcode {
        arguments.push(format!("-Cembed-bitcode={}", if embed_bitcode { "yes" } else { "no" }));
    }
    if let Some(debug_assertions) = codegen.debug_assertions {
        arguments.push(format!(
            "-Cdebug-assertions={}",
            if debug_assertions { "yes" } else { "no" }
        ));
    }
    if let Some(overflow_checks) = codegen.overflow_checks {
        arguments.push(format!(
            "-Coverflow-checks={}",
            if overflow_checks { "yes" } else { "no" }
        ));
    }
    if let Some(panic) = &codegen.panic {
        arguments.push(format!("-Cpanic={panic}"));
    }
    if let Some(prefer_dynamic) = codegen.prefer_dynamic {
        arguments.push(format!(
            "-Cprefer-dynamic={}",
            if prefer_dynamic { "yes" } else { "no" }
        ));
    }
    if let Some(codegen_units) = codegen.codegen_units {
        arguments.push(format!("-Ccodegen-units={codegen_units}"));
    }
    if let Some(lto) = &codegen.lto {
        arguments.push(format!("-Clto={lto}"));
    }
    if let Some(linker_plugin_lto) = codegen.linker_plugin_lto {
        arguments.push(format!(
            "-Clinker-plugin-lto={}",
            if linker_plugin_lto { "yes" } else { "no" }
        ));
    }
    if let Some(debuginfo) = &codegen.debuginfo {
        arguments.push(format!("-Cdebuginfo={debuginfo}"));
    }
    if let Some(split_debuginfo) = &codegen.split_debuginfo {
        arguments.push(format!("-Csplit-debuginfo={split_debuginfo}"));
    }
    if let Some(strip) = &codegen.strip {
        arguments.push(format!("-Cstrip={strip}"));
    }
    arguments
}

fn run_compiler(
    mut command: Command,
    cancellation: &Receiver<Cancellation>,
    stream_limit: u64,
    wall_time: Duration,
) -> RailResult<CompilerRun> {
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| RailError::message("distributed compiler stdout is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| RailError::message("distributed compiler stderr is unavailable"))?;
    let stdout_reader = thread::Builder::new()
        .name("cargo-rail-distributed-stdout".to_string())
        .spawn(move || capture_stream(stdout, stream_limit))?;
    let stderr_reader = thread::Builder::new()
        .name("cargo-rail-distributed-stderr".to_string())
        .spawn(move || capture_stream(stderr, stream_limit))?;
    let started = Instant::now();
    let status = loop {
        match cancellation.try_recv() {
            Ok(Cancellation::Requested) => {
                terminate_child(&mut child);
                join_streams(stdout_reader, stderr_reader);
                return Ok(CompilerRun::Cancelled("execution_cancelled"));
            }
            Ok(Cancellation::ClientLost) => {
                terminate_child(&mut child);
                join_streams(stdout_reader, stderr_reader);
                return Ok(CompilerRun::Cancelled("execution_client_lost"));
            }
            Ok(Cancellation::Invalid) => {
                terminate_child(&mut child);
                join_streams(stdout_reader, stderr_reader);
                return Ok(CompilerRun::Cancelled("execution_cancellation_invalid"));
            }
            Err(TryRecvError::Disconnected) => {
                terminate_child(&mut child);
                join_streams(stdout_reader, stderr_reader);
                return Ok(CompilerRun::Cancelled("execution_client_lost"));
            }
            Err(TryRecvError::Empty) => {}
        }
        if started.elapsed() > wall_time {
            terminate_child(&mut child);
            join_streams(stdout_reader, stderr_reader);
            return Ok(CompilerRun::Cancelled("execution_time_limit_exceeded"));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                terminate_child(&mut child);
                join_streams(stdout_reader, stderr_reader);
                return Err(error.into());
            }
        }
        thread::sleep(WORKER_POLL_INTERVAL);
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| RailError::message("distributed compiler stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| RailError::message("distributed compiler stderr reader panicked"))??;
    let (Some(stdout), Some(stderr)) = (stdout, stderr) else {
        return Ok(CompilerRun::Failed("compiler_stream_limit_exceeded"));
    };
    Ok(CompilerRun::Completed(CapturedCompilerOutput {
        status,
        stdout,
        stderr,
    }))
}

fn capture_stream(mut stream: impl Read, limit: u64) -> std::io::Result<Option<Vec<u8>>> {
    let capacity = usize::try_from(limit.min(64 * 1024)).unwrap_or(64 * 1024);
    let mut captured = Some(Vec::with_capacity(capacity));
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        bytes = bytes.saturating_add(read as u64);
        if bytes <= limit {
            if let Some(captured) = captured.as_mut() {
                captured.extend_from_slice(&buffer[..read]);
            }
        } else {
            captured = None;
        }
    }
    Ok(captured)
}

fn terminate_child(child: &mut Child) {
    drop(child.kill());
    drop(child.wait());
}

fn join_streams(
    stdout: thread::JoinHandle<std::io::Result<Option<Vec<u8>>>>,
    stderr: thread::JoinHandle<std::io::Result<Option<Vec<u8>>>>,
) {
    drop(stdout.join());
    drop(stderr.join());
}

fn validate_and_rebind_dep_info(
    dep_info: &Path,
    current_directory: &Path,
    attempt: &Path,
    inputs: &[InputFrame],
    source_virtual_path: &str,
    allowed_targets: &[&Path],
) -> RailResult<()> {
    let (target, dependencies) = crate::compiler::observation::makefile_dependency_paths(dep_info, current_directory)?;
    if !allowed_targets.contains(&target.as_path()) {
        return Err(RailError::message(format!(
            "distributed compiler dep-info target '{}' is outside its authorized output set",
            target.display()
        )));
    }
    let authorized = inputs
        .iter()
        .filter(|input| input.kind == InputKind::Source)
        .map(|input| {
            source_relative_path(&input.virtual_path)
                .map(|relative| current_directory.join(relative))
                .ok_or_else(|| RailError::message("distributed source input path is invalid"))
        })
        .collect::<RailResult<BTreeSet<_>>>()?;
    if dependencies.is_empty()
        || dependencies.iter().any(|dependency| !authorized.contains(dependency))
        || !dependencies.iter().any(|dependency| {
            source_relative_path(source_virtual_path)
                .is_some_and(|relative| dependency == &current_directory.join(relative))
        })
    {
        return Err(RailError::message(
            "distributed compiler dep-info selected an unauthorized source input",
        ));
    }
    let before = fs::read(dep_info)?;
    let portable = rebind_path_spellings(&before, current_directory, VIRTUAL_WORKSPACE);
    let portable = rebind_path_spellings(&portable, attempt, VIRTUAL_ROOT);
    if physical_worker_root_remains(&portable, attempt) {
        return Err(RailError::message(
            "distributed compiler dep-info retained its worker root",
        ));
    }
    fs::write(dep_info, portable)?;
    Ok(())
}

fn copy_staged_input(source: &Path, destination: &Path, frame: &InputFrame) -> RailResult<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| RailError::message("distributed execution input has no parent directory"))?;
    fs::create_dir_all(parent)?;
    let mut input = File::open(source)?;
    let mut output = create_private_file(destination)?;
    let copied = std::io::copy(&mut input, &mut output)?;
    drop(output);
    if copied != frame.bytes || digest_file(destination, frame.bytes)? != frame.content_digest {
        return Err(RailError::message(
            "distributed execution staged input changed before compiler execution",
        ));
    }
    Ok(())
}

fn physical_worker_root_remains(bytes: &[u8], attempt: &Path) -> bool {
    attempt != Path::new(VIRTUAL_ROOT)
        && path_spellings(attempt)
            .iter()
            .any(|spelling| bytes.windows(spelling.len()).any(|window| window == spelling))
}

fn rebind_path_spellings(bytes: &[u8], path: &Path, replacement: &str) -> Vec<u8> {
    path_spellings(path).iter().fold(bytes.to_vec(), |current, spelling| {
        replace_bytes(&current, spelling, replacement.as_bytes())
    })
}

fn path_spellings(path: &Path) -> Vec<Vec<u8>> {
    let mut literals = vec![path.as_os_str().as_encoded_bytes().to_vec()];
    if let Some(path) = path.to_str() {
        let forward = path.replace('\\', "/");
        literals.push(forward.as_bytes().to_vec());
        literals.push(forward.replace('/', "\\").into_bytes());
    }
    literals.retain(|spelling| !spelling.is_empty());
    literals.sort();
    literals.dedup();

    let mut spellings = literals.clone();
    for literal in literals {
        spellings.push(json_string_contents(&literal));
        spellings.push(escape_dep_info_path(&literal));
    }
    spellings.sort_unstable_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
    spellings.dedup();
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

fn replace_bytes(input: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
    if needle.is_empty() {
        return input.to_vec();
    }
    let mut output = Vec::with_capacity(input.len());
    let mut remaining = input;
    while let Some(index) = remaining.windows(needle.len()).position(|window| window == needle) {
        output.extend_from_slice(&remaining[..index]);
        output.extend_from_slice(replacement);
        remaining = &remaining[index + needle.len()..];
    }
    output.extend_from_slice(remaining);
    output
}

struct OutputPaths {
    dep_info: PathBuf,
    metadata: PathBuf,
    rlib: Option<PathBuf>,
}

fn output_paths(operation: &RustLibraryOperation, output_directory: &Path) -> RailResult<OutputPaths> {
    validate_operation(operation)?;
    Ok(OutputPaths {
        dep_info: output_directory.join(&operation.dep_info_name),
        metadata: output_directory.join(&operation.metadata_name),
        rlib: operation.rlib_name.as_ref().map(|name| output_directory.join(name)),
    })
}

fn validate_request(request: &ExecutionRequest, capability: &WorkerCapability) -> RailResult<()> {
    validate_capability(capability)?;
    validate_operation(&request.operation)?;
    validate_inputs(&request.inputs, &request.operation)?;
    validate_limits(request.limits)?;
    if request.protocol_version != PROTOCOL_VERSION
        || request.capability_id != capability.capability_id
        || request.limits != capability.resource_limits
        || !valid_identity(&request.lease_id, "execution-lease-v3:sha256:")
        || !valid_identity(&request.workload_identity, "workload-v1:sha256:")
        || request.action_id != action_identity(request)?
    {
        return Err(RailError::message("distributed execution request is invalid"));
    }
    Ok(())
}

fn source_relative_path(virtual_path: &str) -> Option<&str> {
    let relative = virtual_path.strip_prefix(VIRTUAL_WORKSPACE)?.strip_prefix('/')?;
    validate_source_relative_path(relative).ok()?;
    Some(relative)
}

fn validate_source_relative_path(relative: &str) -> RailResult<()> {
    if relative.is_empty()
        || relative.len() > MAX_HEADER_BYTES
        || relative.contains('\\')
        || crate::source::RepositoryPath::new(Path::new(relative)).is_err()
        || Path::new(relative).extension() != Some(OsStr::new("rs"))
    {
        return Err(RailError::message(
            "distributed Rust source path is not a canonical repository-relative Rust path",
        ));
    }
    Ok(())
}

fn validate_source_input_relative_path(relative: &str) -> RailResult<()> {
    if relative.is_empty()
        || relative.len() > MAX_HEADER_BYTES
        || relative.contains('\\')
        || relative.as_bytes().contains(&0)
        || crate::source::RepositoryPath::new(Path::new(relative)).is_err()
    {
        return Err(RailError::message(
            "distributed source input is not a canonical repository-relative path",
        ));
    }
    Ok(())
}

fn validate_inputs(inputs: &[InputFrame], operation: &RustLibraryOperation) -> RailResult<()> {
    if inputs.is_empty() || inputs.len() > MAX_INPUT_ENTRIES {
        return Err(RailError::message(
            "distributed execution input set exceeds its entry bound",
        ));
    }
    let mut path_bytes = 0usize;
    let mut total_bytes = 0u64;
    let mut source_count = 0usize;
    let mut dependency_count = 0usize;
    for (index, input) in inputs.iter().enumerate() {
        if index > 0 && inputs[index - 1].virtual_path >= input.virtual_path {
            return Err(RailError::message(
                "distributed execution inputs are not strictly ordered",
            ));
        }
        path_bytes = path_bytes
            .checked_add(input.virtual_path.len())
            .ok_or_else(|| RailError::message("distributed execution input path size overflowed"))?;
        total_bytes = total_bytes
            .checked_add(input.bytes)
            .ok_or_else(|| RailError::message("distributed execution input size overflowed"))?;
        if input.bytes > MAX_INPUT_BYTES || !valid_identity(&input.content_digest, "sha256:") {
            return Err(RailError::message("distributed execution input descriptor is invalid"));
        }
        match input.kind {
            InputKind::Source => {
                source_count = source_count.saturating_add(1);
                let relative = source_relative_path(&input.virtual_path)
                    .ok_or_else(|| RailError::message("distributed execution source input path is invalid"))?;
                validate_source_input_relative_path(relative)?;
                let output = Path::new(&operation.output_relative_directory);
                if Path::new(relative).starts_with(output) {
                    return Err(RailError::message(
                        "distributed execution source input overlaps its output namespace",
                    ));
                }
            }
            InputKind::Dependency => {
                dependency_count = dependency_count.saturating_add(1);
                let relative = input
                    .virtual_path
                    .strip_prefix(VIRTUAL_DEPENDENCIES)
                    .and_then(|relative| relative.strip_prefix('/'))
                    .ok_or_else(|| RailError::message("distributed execution dependency input path is invalid"))?;
                validate_source_input_relative_path(relative)?;
                let name = Path::new(relative)
                    .file_name()
                    .and_then(OsStr::to_str)
                    .ok_or_else(|| RailError::message("distributed execution dependency input has no file name"))?;
                validate_dependency_artifact_name(name)?;
            }
        }
    }
    if path_bytes > MAX_INPUT_PATH_BYTES
        || total_bytes > MAX_TOTAL_INPUT_BYTES
        || source_count == 0
        || dependency_count != operation.dependencies.len()
        || !inputs
            .iter()
            .any(|input| input.kind == InputKind::Source && input.virtual_path == operation.source_virtual_path)
    {
        return Err(RailError::message("distributed execution input set is invalid"));
    }
    for dependency in &operation.dependencies {
        validate_extern_name(&dependency.extern_name)?;
        let matching = inputs
            .iter()
            .filter(|input| input.kind == InputKind::Dependency && input.virtual_path == dependency.virtual_path)
            .count();
        if matching != 1 {
            return Err(RailError::message(
                "distributed execution dependency authority is incomplete",
            ));
        }
    }
    if operation
        .dependencies
        .iter()
        .map(|dependency| dependency.extern_name.as_str())
        .collect::<BTreeSet<_>>()
        .len()
        != operation.dependencies.len()
    {
        return Err(RailError::message("distributed execution repeats an extern name"));
    }
    Ok(())
}

fn validate_dependency_artifact_name(name: &str) -> RailResult<()> {
    let path = Path::new(name);
    if name.is_empty()
        || name.len() > 255
        || path.file_name() != Some(OsStr::new(name))
        || !matches!(path.extension().and_then(OsStr::to_str), Some("rmeta" | "rlib"))
    {
        return Err(RailError::message(
            "distributed Rust dependency artifact name is invalid",
        ));
    }
    Ok(())
}

fn validate_extern_name(name: &str) -> RailResult<()> {
    let parts = name.split(':').collect::<Vec<_>>();
    let Some((crate_name, modifiers)) = parts.split_last() else {
        return Err(RailError::message("distributed Rust extern name is invalid"));
    };
    let mut seen = BTreeSet::new();
    for modifier in modifiers {
        if !matches!(*modifier, "force" | "noprelude" | "nounused" | "priv") || !seen.insert(*modifier) {
            return Err(RailError::message("distributed Rust extern modifier is invalid"));
        }
    }
    let crate_name = crate_name.as_bytes();
    if crate_name.is_empty()
        || crate_name.len() > MAX_CRATE_NAME_BYTES
        || !(crate_name[0].is_ascii_alphabetic() || crate_name[0] == b'_')
        || !crate_name
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
    {
        return Err(RailError::message("distributed Rust extern name is invalid"));
    }
    Ok(())
}

fn validate_operation(operation: &RustLibraryOperation) -> RailResult<()> {
    let crate_name = operation.crate_name.as_bytes();
    let crate_name_valid = !crate_name.is_empty()
        && crate_name.len() <= MAX_CRATE_NAME_BYTES
        && (crate_name[0].is_ascii_alphabetic() || crate_name[0] == b'_')
        && crate_name
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_');
    let metadata_valid = !operation.metadata.is_empty()
        && operation.metadata.len() <= MAX_METADATA_BYTES
        && operation.metadata.bytes().all(|byte| byte.is_ascii_hexdigit());
    let extra = operation.extra_filename.as_bytes();
    let extra_valid = extra.len() > 1
        && extra.len() <= MAX_EXTRA_FILENAME_BYTES
        && extra[0] == b'-'
        && extra[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-'));
    let check_cfg_bytes = operation
        .check_cfg
        .iter()
        .try_fold(0usize, |total, value| total.checked_add(value.len()));
    let check_cfg_valid = operation.check_cfg.len() <= MAX_CHECK_CFG_ENTRIES
        && check_cfg_bytes.is_some_and(|bytes| bytes <= MAX_CHECK_CFG_BYTES)
        && operation.check_cfg.iter().all(|value| {
            !value.is_empty()
                && !value.as_bytes().contains(&0)
                && value.bytes().all(|byte| !byte.is_ascii_control() || byte == b'\t')
        })
        && operation.check_cfg.iter().collect::<BTreeSet<_>>().len() == operation.check_cfg.len();
    let cfg_valid = bounded_rustc_values(&operation.cfg, MAX_CFG_ENTRIES, MAX_CFG_BYTES);
    let lints_valid = operation.lints.len() <= MAX_LINT_ENTRIES
        && operation
            .lints
            .iter()
            .try_fold(0usize, |total, lint| total.checked_add(lint.name.len()))
            .is_some_and(|bytes| bytes <= MAX_LINT_BYTES)
        && operation.lints.iter().all(|lint| {
            !lint.name.is_empty()
                && lint.name.len() <= 256
                && lint
                    .name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':'))
        });
    if operation.operation_class != OperationClass::RustLibrary
        || !crate_name_valid
        || !matches!(operation.edition.as_str(), "2015" | "2018" | "2021" | "2024")
        || !metadata_valid
        || !extra_valid
        || !check_cfg_valid
        || !cfg_valid
        || !lints_valid
        || operation
            .cap_lints
            .as_deref()
            .is_some_and(|value| !matches!(value, "allow" | "warn" | "deny" | "forbid"))
        || operation
            .color
            .as_deref()
            .is_some_and(|value| !matches!(value, "auto" | "always" | "never"))
        || operation
            .diagnostic_width
            .is_some_and(|width| width == 0 || width > 65_535)
        || !valid_rust_library_codegen(&operation.codegen)
        || source_relative_path(&operation.source_virtual_path).is_none()
        || operation.dependencies.len() > MAX_INPUT_ENTRIES
        || operation
            .dependencies
            .iter()
            .any(|dependency| validate_extern_name(&dependency.extern_name).is_err())
        || validate_repository_relative_directory(&operation.output_relative_directory).is_err()
        || validate_output_file_name(&operation.dep_info_name, "d").is_err()
        || validate_output_file_name(&operation.metadata_name, "rmeta").is_err()
        || operation
            .rlib_name
            .as_deref()
            .is_some_and(|name| validate_output_file_name(name, "rlib").is_err())
        || match operation.emission {
            RustLibraryEmission::Metadata => operation.rlib_name.is_some(),
            RustLibraryEmission::MetadataAndLink => {
                operation.rlib_name.is_none()
                    || !matches!(
                        operation.crate_type,
                        RustLibraryCrateType::Lib | RustLibraryCrateType::Rlib
                    )
                    || operation.test_mode
            }
        }
        || operation.toolchain_proc_macro && operation.crate_type != RustLibraryCrateType::ProcMacro
    {
        return Err(RailError::message("distributed Rust library operation is invalid"));
    }
    Ok(())
}

fn validate_output_file_name(name: &str, extension: &str) -> RailResult<()> {
    let path = Path::new(name);
    if name.is_empty()
        || name.len() > 255
        || path.file_name() != Some(OsStr::new(name))
        || path.extension() != Some(OsStr::new(extension))
    {
        return Err(RailError::message("distributed compiler output file name is invalid"));
    }
    Ok(())
}

fn bounded_rustc_values(values: &[String], maximum_entries: usize, maximum_bytes: usize) -> bool {
    values.len() <= maximum_entries
        && values
            .iter()
            .try_fold(0usize, |total, value| total.checked_add(value.len()))
            .is_some_and(|bytes| bytes <= maximum_bytes)
        && values.iter().all(|value| {
            !value.is_empty()
                && !value.as_bytes().contains(&0)
                && value.bytes().all(|byte| !byte.is_ascii_control() || byte == b'\t')
        })
}

fn valid_rust_library_codegen(codegen: &RustLibraryCodegen) -> bool {
    codegen
        .opt_level
        .as_deref()
        .is_none_or(|value| matches!(value, "0" | "1" | "2" | "3" | "s" | "z"))
        && codegen
            .debuginfo
            .as_deref()
            .is_none_or(|value| matches!(value, "0" | "1" | "2" | "line-directives-only" | "line-tables-only"))
        && codegen
            .split_debuginfo
            .as_deref()
            .is_none_or(|value| matches!(value, "off" | "packed" | "unpacked"))
        && codegen
            .strip
            .as_deref()
            .is_none_or(|value| matches!(value, "none" | "debuginfo" | "symbols"))
        && codegen.codegen_units.is_none_or(|units| units > 0 && units <= 4096)
        && codegen
            .panic
            .as_deref()
            .is_none_or(|value| matches!(value, "abort" | "unwind"))
        && codegen
            .lto
            .as_deref()
            .is_none_or(|value| matches!(value, "fat" | "no" | "off" | "thin" | "yes"))
}

fn validate_repository_relative_directory(relative: &str) -> RailResult<()> {
    if relative.is_empty()
        || relative.len() > MAX_HEADER_BYTES
        || relative.contains('\\')
        || crate::source::RepositoryPath::new(Path::new(relative)).is_err()
    {
        return Err(RailError::message(
            "distributed output directory is not a canonical repository-relative path",
        ));
    }
    Ok(())
}

fn validate_limits(limits: ExecutionLimits) -> RailResult<()> {
    if limits.cpu_period_micros == 0
        || limits.cpu_period_micros > MAX_CPU_PERIOD_MICROS
        || limits.cpu_quota_micros == 0
        || limits.cpu_quota_micros > MAX_CPU_QUOTA_MICROS
        || limits.max_output_bytes == 0
        || limits.max_output_bytes > MAX_OUTPUT_BYTES
        || limits.max_processes == 0
        || limits.max_processes > MAX_PROCESSES
        || limits.max_stream_bytes == 0
        || limits.max_stream_bytes > MAX_STREAM_BYTES
        || limits.memory_bytes == 0
        || limits.memory_bytes > MAX_MEMORY_BYTES
        || limits.scratch_bytes == 0
        || limits.scratch_bytes > MAX_SCRATCH_BYTES
        || limits.wall_time_ms == 0
        || limits.wall_time_ms > MAX_WALL_TIME_MS
    {
        return Err(RailError::message("distributed execution resource limits are invalid"));
    }
    Ok(())
}

fn action_identity(request: &ExecutionRequest) -> RailResult<String> {
    let encoded = canonical_json(&(
        &request.capability_id,
        &request.inputs,
        request.limits,
        &request.operation,
        request.protocol_version,
    ))?;
    Ok(format!(
        "execution-action-v3:sha256:{}",
        ContentDigest::sha256(&encoded)
    ))
}

fn valid_identity(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }) && value.len() <= MAX_IDENTITY_BYTES
}

pub(crate) fn worker_capability_identity_is_valid(value: &str) -> bool {
    valid_identity(value, "worker-capability-v3:sha256:")
}

fn read_request(reader: &mut impl Read) -> RailResult<RequestEnvelope> {
    read_request_with_staging_parent(reader, None)
}

fn read_request_with_staging_parent(
    reader: &mut impl Read,
    staging_parent: Option<&Path>,
) -> RailResult<RequestEnvelope> {
    read_magic(reader, REQUEST_MAGIC, "request")?;
    let header = read_sized_bytes(reader, MAX_HEADER_BYTES, "request header")?;
    let request: ExecutionRequest = serde_json::from_slice(&header)
        .map_err(|_| RailError::message("distributed execution request header is malformed"))?;
    if canonical_json(&request)? != header {
        return Err(RailError::message(
            "distributed execution request header is not canonical",
        ));
    }
    validate_inputs(&request.inputs, &request.operation)?;
    let mut staging_builder = tempfile::Builder::new();
    staging_builder.prefix("cargo-rail-distributed-input-");
    let staging = match staging_parent {
        Some(parent) => staging_builder.tempdir_in(parent)?,
        None => staging_builder.tempdir()?,
    };
    let mut inputs = BTreeMap::new();
    for (index, frame) in request.inputs.iter().enumerate() {
        let path = staging.path().join(format!("input-{index:05}"));
        let mut file = create_private_file(&path)?;
        let mut hasher = Sha256::new();
        let mut remaining = frame.bytes;
        let mut buffer = [0_u8; 64 * 1024];
        while remaining > 0 {
            let maximum = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
            let read = reader.read(&mut buffer[..maximum])?;
            if read == 0 {
                return Err(RailError::message(
                    "distributed execution request ended inside an input frame",
                ));
            }
            file.write_all(&buffer[..read])?;
            hasher.update(&buffer[..read]);
            remaining = remaining.saturating_sub(read as u64);
        }
        drop(file);
        if format_sha256(hasher.finalize().into()) != frame.content_digest {
            return Err(RailError::message(
                "distributed execution request input failed digest validation",
            ));
        }
        inputs.insert(frame.virtual_path.clone(), path);
    }
    read_magic(reader, REQUEST_TRAILER, "request trailer")?;
    Ok(RequestEnvelope {
        inputs,
        request,
        _staging: staging,
    })
}

fn write_candidate_request(
    writer: &mut impl Write,
    request: &ExecutionRequest,
    candidate: &RustLibraryCandidate,
) -> RailResult<()> {
    if request.inputs != candidate.input_frames() {
        return Err(RailError::message(
            "distributed execution request inputs changed before transfer",
        ));
    }
    let header = canonical_json(request)?;
    write_sized_header(writer, REQUEST_MAGIC, &header)?;
    for input in &candidate.inputs {
        match &input.payload {
            CandidateInputPayload::Bytes(bytes) => {
                if bytes.len() as u64 != input.frame.bytes || digest_bytes(bytes) != input.frame.content_digest {
                    return Err(RailError::message(
                        "distributed execution in-memory input changed before transfer",
                    ));
                }
                writer.write_all(bytes)?;
            }
            CandidateInputPayload::File(path) => copy_exact_input_file(writer, path, &input.frame)?,
        }
    }
    writer.write_all(REQUEST_TRAILER)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
fn write_request(writer: &mut impl Write, request: &ExecutionRequest, source: &[u8]) -> RailResult<()> {
    let [frame] = request.inputs.as_slice() else {
        return Err(RailError::message("test request does not contain one input"));
    };
    if source.len() as u64 != frame.bytes || digest_bytes(source) != frame.content_digest {
        return Err(RailError::message("test request input does not match its descriptor"));
    }
    let header = canonical_json(request)?;
    write_sized_header(writer, REQUEST_MAGIC, &header)?;
    writer.write_all(source)?;
    writer.write_all(REQUEST_TRAILER)?;
    writer.flush()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn write_envelope_request(writer: &mut impl Write, envelope: &RequestEnvelope) -> RailResult<()> {
    let header = canonical_json(&envelope.request)?;
    write_sized_header(writer, REQUEST_MAGIC, &header)?;
    for frame in &envelope.request.inputs {
        let path = envelope
            .inputs
            .get(&frame.virtual_path)
            .ok_or_else(|| RailError::message("distributed execution input staging is incomplete"))?;
        copy_exact_input_file(writer, path, frame)?;
    }
    writer.write_all(REQUEST_TRAILER)?;
    writer.flush()?;
    Ok(())
}

fn copy_exact_input_file(writer: &mut impl Write, path: &Path, frame: &InputFrame) -> RailResult<()> {
    let before = fs::symlink_metadata(path)?;
    if !before.is_file() || crate::utils::is_symlink_or_reparse(&before) || before.len() != frame.bytes {
        return Err(RailError::message(
            "distributed execution input changed before transfer",
        ));
    }
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut copied = 0u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        copied = copied.saturating_add(read as u64);
        if copied > frame.bytes {
            return Err(RailError::message("distributed execution input grew during transfer"));
        }
        writer.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
    }
    let after = fs::symlink_metadata(path)?;
    if copied != frame.bytes
        || before.len() != after.len()
        || before.modified()? != after.modified()?
        || crate::utils::is_symlink_or_reparse(&after)
        || format_sha256(hasher.finalize().into()) != frame.content_digest
    {
        return Err(RailError::message(
            "distributed execution input changed during transfer",
        ));
    }
    Ok(())
}

fn request_input_bytes(request: &ExecutionRequest) -> u64 {
    request
        .inputs
        .iter()
        .fold(0_u64, |total, input| total.saturating_add(input.bytes))
}

fn read_cancellation(mut reader: std::io::Stdin, expected_lease: &str) -> Cancellation {
    let result: RailResult<Cancellation> = (|| {
        let mut magic = [0_u8; 8];
        match reader.read_exact(&mut magic) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(Cancellation::ClientLost),
            Err(error) => return Err(error.into()),
        }
        if &magic != CANCEL_MAGIC {
            return Ok(Cancellation::Invalid);
        }
        let lease = read_sized_bytes(&mut reader, MAX_IDENTITY_BYTES, "cancellation lease")?;
        if lease != expected_lease.as_bytes() {
            return Ok(Cancellation::Invalid);
        }
        read_magic(&mut reader, CANCEL_TRAILER, "cancellation trailer")?;
        Ok(Cancellation::Requested)
    })();
    result.unwrap_or(Cancellation::Invalid)
}

fn read_magic(reader: &mut impl Read, expected: &[u8; 8], role: &str) -> RailResult<()> {
    let mut actual = [0_u8; 8];
    reader.read_exact(&mut actual)?;
    if &actual != expected {
        return Err(RailError::message(format!(
            "distributed execution {role} has invalid framing"
        )));
    }
    Ok(())
}

fn read_sized_bytes(reader: &mut impl Read, maximum: usize, role: &str) -> RailResult<Vec<u8>> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length)?;
    let length = usize::try_from(u32::from_le_bytes(length))
        .map_err(|_| RailError::message(format!("distributed execution {role} length exceeds this platform")))?;
    if length == 0 || length > maximum {
        return Err(RailError::message(format!(
            "distributed execution {role} length is invalid"
        )));
    }
    let mut bytes = vec![0_u8; length];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn write_sized_header(writer: &mut impl Write, magic: &[u8; 8], header: &[u8]) -> RailResult<()> {
    let length = u32::try_from(header.len())
        .ok()
        .filter(|length| *length > 0 && header.len() <= MAX_HEADER_BYTES)
        .ok_or_else(|| RailError::message("distributed execution header length is invalid"))?;
    writer.write_all(magic)?;
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(header)?;
    Ok(())
}

fn digest_bytes(bytes: &[u8]) -> String {
    format!("sha256:{}", ContentDigest::sha256(bytes))
}

#[cfg(any(test, target_os = "linux"))]
fn write_private_file(path: &Path, bytes: &[u8]) -> RailResult<()> {
    let mut file = create_private_file(path)?;
    file.write_all(bytes)?;
    Ok(())
}

fn create_private_file(path: &Path) -> RailResult<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

fn prepare_file_frame(slot: ResponseSlot, path: PathBuf, maximum: u64) -> RailResult<PreparedResponseFrame> {
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_file()
        || crate::utils::is_symlink_or_reparse(&metadata)
        || metadata.len() > maximum
        || metadata.len() > MAX_OUTPUT_BYTES
    {
        return Err(RailError::message(
            "distributed execution output is outside its bounded regular-file contract",
        ));
    }
    let descriptor = ResponseFrame {
        bytes: metadata.len(),
        content_digest: digest_file(&path, metadata.len())?,
        mode: 0o644,
        slot,
    };
    Ok(PreparedResponseFrame {
        descriptor,
        payload: ResponsePayload::File(path),
    })
}

fn prepare_bytes_frame(slot: ResponseSlot, bytes: Vec<u8>, maximum: u64) -> RailResult<PreparedResponseFrame> {
    if bytes.len() as u64 > maximum || bytes.len() as u64 > MAX_STREAM_BYTES {
        return Err(RailError::message(
            "distributed execution stream exceeds its byte bound",
        ));
    }
    let descriptor = ResponseFrame {
        bytes: bytes.len() as u64,
        content_digest: digest_bytes(&bytes),
        mode: 0,
        slot,
    };
    Ok(PreparedResponseFrame {
        descriptor,
        payload: ResponsePayload::Bytes(bytes),
    })
}

fn successful_response(
    request: &ExecutionRequest,
    frames: Vec<PreparedResponseFrame>,
    worker_timing: WorkerPhaseTiming,
) -> (ExecutionResponse, Vec<PreparedResponseFrame>) {
    let descriptors = frames.iter().map(|frame| frame.descriptor.clone()).collect();
    (
        ExecutionResponse {
            action_id: request.action_id.clone(),
            capability_id: request.capability_id.clone(),
            frames: descriptors,
            lease_id: request.lease_id.clone(),
            protocol_version: PROTOCOL_VERSION,
            reason: None,
            status: ExecutionStatus::Success,
            termination: Some(CompilerTermination::Exit { code: 0 }),
            worker_timing,
            workload_identity: request.workload_identity.clone(),
        },
        frames,
    )
}

fn compiler_failure_response(
    request: &ExecutionRequest,
    termination: CompilerTermination,
    frames: Vec<PreparedResponseFrame>,
    worker_timing: WorkerPhaseTiming,
) -> (ExecutionResponse, Vec<PreparedResponseFrame>) {
    let descriptors = frames.iter().map(|frame| frame.descriptor.clone()).collect();
    (
        ExecutionResponse {
            action_id: request.action_id.clone(),
            capability_id: request.capability_id.clone(),
            frames: descriptors,
            lease_id: request.lease_id.clone(),
            protocol_version: PROTOCOL_VERSION,
            reason: None,
            status: ExecutionStatus::CompilerFailed,
            termination: Some(termination),
            worker_timing,
            workload_identity: request.workload_identity.clone(),
        },
        frames,
    )
}

fn rejected_response(
    request: &ExecutionRequest,
    reason: &'static str,
    worker_timing: WorkerPhaseTiming,
) -> (ExecutionResponse, Vec<PreparedResponseFrame>) {
    (
        ExecutionResponse {
            action_id: request.action_id.clone(),
            capability_id: request.capability_id.clone(),
            frames: Vec::new(),
            lease_id: request.lease_id.clone(),
            protocol_version: PROTOCOL_VERSION,
            reason: Some(reason.to_string()),
            status: ExecutionStatus::Rejected,
            termination: None,
            worker_timing,
            workload_identity: request.workload_identity.clone(),
        },
        Vec::new(),
    )
}

fn write_response(
    writer: &mut impl Write,
    expected: &ExecutionRequest,
    response: &ExecutionResponse,
    frames: &[PreparedResponseFrame],
) -> RailResult<()> {
    if response.frames != frames.iter().map(|frame| frame.descriptor.clone()).collect::<Vec<_>>() {
        return Err(RailError::message(
            "distributed execution response descriptors changed before framing",
        ));
    }
    if response.action_id != expected.action_id
        || response.capability_id != expected.capability_id
        || response.lease_id != expected.lease_id
        || response.workload_identity != expected.workload_identity
    {
        return Err(RailError::message(
            "distributed execution response does not match its request authority",
        ));
    }
    validate_response_header(response, &expected.operation)?;
    let header = canonical_json(response)?;
    write_sized_header(writer, RESPONSE_MAGIC, &header)?;
    for frame in frames {
        match &frame.payload {
            ResponsePayload::File(path) => copy_exact_file(writer, path, frame.descriptor.bytes)?,
            ResponsePayload::Bytes(bytes) => writer.write_all(bytes)?,
        }
    }
    writer.write_all(RESPONSE_TRAILER)?;
    Ok(())
}

fn copy_exact_file(writer: &mut impl Write, path: &Path, expected: u64) -> RailResult<()> {
    let before = fs::symlink_metadata(path)?;
    if !before.is_file() || crate::utils::is_symlink_or_reparse(&before) || before.len() != expected {
        return Err(RailError::message(
            "distributed execution response file changed before transfer",
        ));
    }
    let file = File::open(path)?;
    let copied = std::io::copy(&mut file.take(expected.saturating_add(1)), writer)?;
    let after = fs::symlink_metadata(path)?;
    if copied != expected || before.len() != after.len() || before.modified()? != after.modified()? {
        return Err(RailError::message(
            "distributed execution response file changed during transfer",
        ));
    }
    Ok(())
}

fn validate_response_header(response: &ExecutionResponse, operation: &RustLibraryOperation) -> RailResult<()> {
    if response.protocol_version != PROTOCOL_VERSION
        || !valid_identity(&response.action_id, "execution-action-v3:sha256:")
        || !valid_identity(&response.capability_id, "worker-capability-v3:sha256:")
        || !valid_identity(&response.lease_id, "execution-lease-v3:sha256:")
        || !valid_identity(&response.workload_identity, "workload-v1:sha256:")
    {
        return Err(RailError::message(
            "distributed execution response authority is invalid",
        ));
    }
    match response.status {
        ExecutionStatus::Success => {
            let expected = match operation.emission {
                RustLibraryEmission::Metadata => &[
                    ResponseSlot::DepInfo,
                    ResponseSlot::Metadata,
                    ResponseSlot::Stderr,
                    ResponseSlot::Stdout,
                ][..],
                RustLibraryEmission::MetadataAndLink => &[
                    ResponseSlot::DepInfo,
                    ResponseSlot::Metadata,
                    ResponseSlot::Rlib,
                    ResponseSlot::Stderr,
                    ResponseSlot::Stdout,
                ][..],
            };
            if response.termination != Some(CompilerTermination::Exit { code: 0 }) || response.reason.is_some() {
                return Err(RailError::message(
                    "distributed execution success response is incomplete",
                ));
            }
            validate_response_frames(response, expected)?;
        }
        ExecutionStatus::CompilerFailed => {
            if response.termination.is_none()
                || response.termination == Some(CompilerTermination::Exit { code: 0 })
                || response.reason.is_some()
            {
                return Err(RailError::message("distributed compiler failure response is invalid"));
            }
            validate_response_frames(response, &[ResponseSlot::Stderr, ResponseSlot::Stdout])?;
        }
        ExecutionStatus::Rejected => {
            if response.termination.is_some()
                || !response.frames.is_empty()
                || response.reason.as_deref().is_none_or(|reason| !valid_reason(reason))
            {
                return Err(RailError::message(
                    "distributed execution rejection response is invalid",
                ));
            }
        }
    }
    Ok(())
}

fn validate_worker_timing(response: &ExecutionResponse, expected: &ExecutionRequest) -> RailResult<()> {
    let timing = response.worker_timing;
    let measured = timing
        .input_ns
        .checked_add(timing.compiler_ns)
        .and_then(|total| total.checked_add(timing.result_encode_ns))
        .ok_or_else(|| RailError::message("distributed worker timing overflowed"))?;
    let result_bytes = response.frames.iter().try_fold(0_u64, |total, frame| {
        total
            .checked_add(frame.bytes)
            .ok_or_else(|| RailError::message("distributed worker result size overflowed"))
    })?;
    if timing.elapsed_ns == 0
        || timing.elapsed_ns > MAX_PLACEMENT_SAMPLE_NS
        || timing.queue_ns > MAX_PLACEMENT_SAMPLE_NS
        || measured > timing.elapsed_ns
        || timing.source_bytes != request_input_bytes(expected)
        || timing.result_bytes != result_bytes
    {
        return Err(RailError::message("distributed worker timing is invalid"));
    }
    Ok(())
}

fn validate_response_frames(response: &ExecutionResponse, expected: &[ResponseSlot]) -> RailResult<()> {
    if response.frames.len() != expected.len()
        || !response
            .frames
            .iter()
            .map(|frame| frame.slot)
            .eq(expected.iter().copied())
    {
        return Err(RailError::message("distributed execution response slot set is invalid"));
    }
    let mut total = 0_u64;
    for frame in &response.frames {
        total = total
            .checked_add(frame.bytes)
            .ok_or_else(|| RailError::message("distributed execution response size overflowed"))?;
        if !valid_identity(&frame.content_digest, "sha256:")
            || frame.bytes
                > if frame.slot.is_stream() {
                    MAX_STREAM_BYTES
                } else {
                    MAX_OUTPUT_BYTES
                }
            || frame.mode != if frame.slot.is_stream() { 0 } else { 0o644 }
        {
            return Err(RailError::message("distributed execution response frame is invalid"));
        }
    }
    if total > MAX_TOTAL_OUTPUT_BYTES {
        return Err(RailError::message(
            "distributed execution response exceeds its total byte bound",
        ));
    }
    Ok(())
}

fn valid_reason(reason: &str) -> bool {
    !reason.is_empty() && reason.len() <= 128 && reason.bytes().all(|byte| byte.is_ascii_lowercase() || byte == b'_')
}

enum DecodedExecution {
    Success(StagedExecutionResult),
    CompilerFailed {
        termination: CompilerTermination,
        result: StagedExecutionResult,
    },
    Rejected,
}

fn read_response_into(
    reader: &mut impl Read,
    expected: &ExecutionRequest,
    staging: NativeResultStaging,
    timing: &mut ResponseTiming,
) -> RailResult<DecodedExecution> {
    let remote_started = Instant::now();
    read_magic(reader, RESPONSE_MAGIC, "response")?;
    let header = read_sized_bytes(reader, MAX_HEADER_BYTES, "response header")?;
    let response: ExecutionResponse = serde_json::from_slice(&header)
        .map_err(|_| RailError::message("distributed execution response header is malformed"))?;
    if canonical_json(&response)? != header {
        return Err(RailError::message(
            "distributed execution response header is not canonical",
        ));
    }
    validate_response_header(&response, &expected.operation)?;
    if response.action_id != expected.action_id
        || response.capability_id != expected.capability_id
        || response.lease_id != expected.lease_id
        || response.workload_identity != expected.workload_identity
    {
        return Err(RailError::message(
            "distributed execution response does not match its request authority",
        ));
    }
    validate_worker_timing(&response, expected)?;
    timing.worker = response.worker_timing;
    timing.remote_execution.record(remote_started);
    if response.status == ExecutionStatus::Rejected {
        read_magic(reader, RESPONSE_TRAILER, "response trailer")?;
        read_end(reader, "response")?;
        return Ok(DecodedExecution::Rejected);
    }
    let transfer_started = Instant::now();
    let distributed = staging.path().join("distributed");
    fs::create_dir(&distributed)?;
    let mut frames = BTreeMap::new();
    for frame in &response.frames {
        let path = distributed.join(frame.slot.file_name());
        let mut file = create_private_file(&path)?;
        let mut hasher = Sha256::new();
        let mut remaining = frame.bytes;
        let mut buffer = [0_u8; 64 * 1024];
        while remaining > 0 {
            let maximum = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
            let read = reader.read(&mut buffer[..maximum])?;
            if read == 0 {
                return Err(RailError::message(
                    "distributed execution response ended inside a frame",
                ));
            }
            file.write_all(&buffer[..read])?;
            hasher.update(&buffer[..read]);
            remaining = remaining.saturating_sub(read as u64);
        }
        drop(file);
        if format_sha256(hasher.finalize().into()) != frame.content_digest {
            return Err(RailError::message(
                "distributed execution response frame failed digest validation",
            ));
        }
        frames.insert(frame.slot, path);
    }
    read_magic(reader, RESPONSE_TRAILER, "response trailer")?;
    read_end(reader, "response")?;
    timing.result_transfer.record(transfer_started);
    timing.result_bytes = response
        .frames
        .iter()
        .fold(0_u64, |total, frame| total.saturating_add(frame.bytes));
    let descriptors = response
        .frames
        .iter()
        .cloned()
        .map(|frame| (frame.slot, frame))
        .collect();
    let result = StagedExecutionResult {
        staging,
        frames,
        descriptors,
        inputs: expected.inputs.clone(),
        operation: expected.operation.clone(),
    };
    match response.status {
        ExecutionStatus::Success => Ok(DecodedExecution::Success(result)),
        ExecutionStatus::CompilerFailed => {
            let termination = response
                .termination
                .ok_or_else(|| RailError::message("distributed compiler failure has no termination state"))?;
            Ok(DecodedExecution::CompilerFailed { termination, result })
        }
        ExecutionStatus::Rejected => Err(RailError::message(
            "distributed execution rejection crossed the artifact staging boundary",
        )),
    }
}

fn read_end(reader: &mut impl Read, role: &str) -> RailResult<()> {
    let mut trailing = [0_u8; 1];
    loop {
        match reader.read(&mut trailing) {
            Ok(0) => return Ok(()),
            Ok(_) => {
                return Err(RailError::message(format!(
                    "distributed execution {role} has trailing bytes"
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    const SOURCE: &[u8] = b"pub fn answer() -> u8 { 42 }\n";

    fn fixed_identity(prefix: &str, digit: char) -> String {
        format!("{prefix}{}", digit.to_string().repeat(64))
    }

    fn capability() -> RailResult<WorkerCapability> {
        let mut capability = WorkerCapability {
            architecture: "test-architecture".to_string(),
            capability_id: String::new(),
            endianness: "little".to_string(),
            environment_contract: fixed_identity("environment-v1:sha256:", '1'),
            filesystem_contract: "private-real-directory-v1".to_string(),
            host_target: "test-target".to_string(),
            isolation: WorkerIsolation::ProcessOnlyUnqualified,
            isolation_identity: process_isolation_identity()?,
            operating_system: "test-os".to_string(),
            operation_classes: vec![OperationClass::RustLibrary],
            platform_family: "test-family".to_string(),
            protocol_version: PROTOCOL_VERSION,
            resource_limits: worker_execution_limits(),
            rustc_content_digest: fixed_identity("sha256:", '2'),
            rustc_verbose_version: "rustc test\nhost: test-target\n".to_string(),
            sysroot_identity: fixed_identity("sha256:", '3'),
            working_directory_contract: "canonical-workspace-relative-remapped-v1".to_string(),
        };
        capability.capability_id = capability_identity(&capability)?;
        Ok(capability)
    }

    fn request_for(source: &[u8]) -> RailResult<ExecutionRequest> {
        let capability = capability()?;
        let mut request = ExecutionRequest {
            action_id: String::new(),
            capability_id: capability.capability_id,
            inputs: vec![InputFrame {
                bytes: source.len() as u64,
                content_digest: digest_bytes(source),
                kind: InputKind::Source,
                virtual_path: format!("{VIRTUAL_WORKSPACE}/src/lib.rs"),
            }],
            lease_id: fixed_identity("execution-lease-v3:sha256:", '4'),
            limits: worker_execution_limits(),
            operation: RustLibraryOperation {
                cap_lints: None,
                cargo_json_diagnostics: false,
                check_cfg: Vec::new(),
                codegen: RustLibraryCodegen::default(),
                color: None,
                crate_name: "distributed_fixture".to_string(),
                crate_type: RustLibraryCrateType::Rlib,
                cfg: Vec::new(),
                dependencies: Vec::new(),
                diagnostic_width: None,
                dep_info_name: "distributed_fixture-0123456789abcdef.d".to_string(),
                edition: "2024".to_string(),
                emission: RustLibraryEmission::MetadataAndLink,
                extra_filename: "-0123456789abcdef".to_string(),
                metadata: "0123456789abcdef".to_string(),
                metadata_name: "libdistributed_fixture-0123456789abcdef.rmeta".to_string(),
                lints: Vec::new(),
                operation_class: OperationClass::RustLibrary,
                output_relative_directory: "target/debug/deps".to_string(),
                output_dependency_search: false,
                rlib_name: Some("libdistributed_fixture-0123456789abcdef.rlib".to_string()),
                source_virtual_path: format!("{VIRTUAL_WORKSPACE}/src/lib.rs"),
                test_mode: false,
                toolchain_proc_macro: false,
            },
            protocol_version: PROTOCOL_VERSION,
            workload_identity: fixed_identity("workload-v1:sha256:", '5'),
        };
        request.action_id = action_identity(&request)?;
        Ok(request)
    }

    fn success_frames(root: &Path) -> RailResult<Vec<PreparedResponseFrame>> {
        let files = [
            (ResponseSlot::DepInfo, b"portable dep-info".as_slice()),
            (ResponseSlot::Metadata, b"metadata bytes".as_slice()),
            (ResponseSlot::Rlib, b"rlib bytes".as_slice()),
        ];
        let mut frames = Vec::new();
        for (slot, bytes) in files {
            let path = root.join(slot.file_name());
            write_private_file(&path, bytes)?;
            frames.push(prepare_file_frame(slot, path, MAX_OUTPUT_BYTES)?);
        }
        frames.push(prepare_bytes_frame(
            ResponseSlot::Stderr,
            b"warning".to_vec(),
            MAX_STREAM_BYTES,
        )?);
        frames.push(prepare_bytes_frame(
            ResponseSlot::Stdout,
            b"compiler output".to_vec(),
            MAX_STREAM_BYTES,
        )?);
        Ok(frames)
    }

    fn response_bytes(request: &ExecutionRequest) -> RailResult<Vec<u8>> {
        let payloads = tempfile::tempdir()?;
        let frames = success_frames(payloads.path())?;
        let timing = worker_timing(request, &frames);
        let (response, frames) = successful_response(request, frames, timing);
        let mut encoded = Vec::new();
        write_response(&mut encoded, request, &response, &frames)?;
        Ok(encoded)
    }

    fn worker_timing(request: &ExecutionRequest, frames: &[PreparedResponseFrame]) -> WorkerPhaseTiming {
        WorkerPhaseTiming {
            queue_ns: 1,
            input_ns: 2,
            compiler_ns: 3,
            result_encode_ns: 4,
            elapsed_ns: 10,
            source_bytes: request_input_bytes(request),
            result_bytes: frames
                .iter()
                .fold(0_u64, |total, frame| total.saturating_add(frame.descriptor.bytes)),
        }
    }

    fn placement_candidate(source: &[u8]) -> RailResult<RustLibraryCandidate> {
        RustLibraryCandidate::new(
            RustLibraryCandidateInput {
                crate_name: "placement_fixture".to_string(),
                crate_type: "rlib".to_string(),
                dep_info_name: "placement_fixture-0123456789abcdef.d".to_string(),
                edition: "2024".to_string(),
                emission: RustLibraryEmission::MetadataAndLink,
                metadata: "0123456789abcdef".to_string(),
                metadata_name: "libplacement_fixture-0123456789abcdef.rmeta".to_string(),
                extra_filename: "-0123456789abcdef".to_string(),
                output_relative_directory: "target/debug/deps".to_string(),
                source_relative_path: "src/lib.rs".to_string(),
                test_mode: false,
                toolchain_proc_macro: false,
                rlib_name: Some("libplacement_fixture-0123456789abcdef.rlib".to_string()),
                options: RustLibraryExecutionOptions::default(),
            },
            source.to_vec(),
        )
    }

    fn estimate(mean_ns: u64, deviation_ns: u64, samples: u32, updated_unix_secs: u64) -> PlacementEstimate {
        PlacementEstimate {
            deviation_ns,
            mean_ns,
            samples,
            updated_unix_secs,
        }
    }

    fn empty_staged_result() -> RailResult<StagedExecutionResult> {
        let request = request_for(SOURCE)?;
        Ok(StagedExecutionResult {
            staging: NativeResultStaging::temporary()?,
            frames: BTreeMap::new(),
            descriptors: BTreeMap::new(),
            inputs: request.inputs,
            operation: request.operation,
        })
    }

    #[test]
    fn request_framing_round_trips_only_canonical_bounded_input() {
        let result: RailResult<()> = (|| {
            let request = request_for(SOURCE)?;
            validate_request(&request, &capability()?)?;
            let mut encoded = Vec::new();
            write_request(&mut encoded, &request, SOURCE)?;

            let decoded = read_request(&mut Cursor::new(&encoded))?;
            assert_eq!(decoded.request, request);
            let source = decoded
                .inputs
                .get(&format!("{VIRTUAL_WORKSPACE}/src/lib.rs"))
                .ok_or_else(|| RailError::message("decoded request lost its source input"))?;
            assert_eq!(fs::read(source)?, SOURCE);

            let staging_parent = tempfile::tempdir()?;
            let decoded = read_request_with_staging_parent(&mut Cursor::new(&encoded), Some(staging_parent.path()))?;
            assert!(
                decoded
                    .inputs
                    .values()
                    .all(|path| path.starts_with(staging_parent.path()))
            );

            let header_length = u32::from_le_bytes(
                encoded[8..12]
                    .try_into()
                    .map_err(|_| RailError::message("test request frame did not contain its length"))?,
            ) as usize;
            let header_end = 12 + header_length;
            let mut corrupted = encoded.clone();
            corrupted[header_end] ^= 1;
            assert!(read_request(&mut Cursor::new(corrupted)).is_err());
            encoded.insert(header_end, b' ');
            encoded[8..12].copy_from_slice(
                &u32::try_from(header_length + 1)
                    .map_err(|_| RailError::message("test request header length overflowed"))?
                    .to_le_bytes(),
            );
            assert!(read_request(&mut Cursor::new(encoded)).is_err());
            Ok(())
        })();
        result.unwrap();
    }

    #[test]
    fn input_authority_binds_modules_and_exact_dependencies_and_rejects_hostile_sets() {
        let result: RailResult<()> = (|| {
            let mut request = request_for(SOURCE)?;
            let dependency_path = format!("{VIRTUAL_DEPENDENCIES}/libdependency-0123456789abcdef.rmeta");
            request.inputs.insert(
                0,
                InputFrame {
                    bytes: 17,
                    content_digest: fixed_identity("sha256:", '7'),
                    kind: InputKind::Dependency,
                    virtual_path: dependency_path.clone(),
                },
            );
            request.inputs.push(InputFrame {
                bytes: 19,
                content_digest: fixed_identity("sha256:", '8'),
                kind: InputKind::Source,
                virtual_path: format!("{VIRTUAL_WORKSPACE}/src/module.rs"),
            });
            request.operation.dependencies.push(RustLibraryDependency {
                extern_name: "dependency".to_string(),
                virtual_path: dependency_path,
            });
            request.action_id = action_identity(&request)?;
            validate_request(&request, &capability()?)?;

            let identity = request.action_id.clone();
            let mut changed_module = request.clone();
            changed_module.inputs[2].content_digest = fixed_identity("sha256:", '9');
            changed_module.action_id = action_identity(&changed_module)?;
            assert_ne!(changed_module.action_id, identity);

            let mut repeated = request.clone();
            repeated.inputs.push(repeated.inputs[2].clone());
            assert!(validate_inputs(&repeated.inputs, &repeated.operation).is_err());

            let mut traversal = request.clone();
            traversal.inputs[2].virtual_path = format!("{VIRTUAL_WORKSPACE}/src/../secret");
            traversal
                .inputs
                .sort_by(|left, right| left.virtual_path.cmp(&right.virtual_path));
            assert!(validate_inputs(&traversal.inputs, &traversal.operation).is_err());

            let mut incomplete = request.clone();
            incomplete.inputs.remove(0);
            assert!(validate_inputs(&incomplete.inputs, &incomplete.operation).is_err());

            let mut oversized = request.clone();
            for index in 0..4 {
                oversized.inputs.push(InputFrame {
                    bytes: MAX_INPUT_BYTES,
                    content_digest: fixed_identity("sha256:", char::from(b'a' + index)),
                    kind: InputKind::Source,
                    virtual_path: format!("{VIRTUAL_WORKSPACE}/src/oversized-{index}.rs"),
                });
            }
            oversized
                .inputs
                .sort_by(|left, right| left.virtual_path.cmp(&right.virtual_path));
            assert!(validate_inputs(&oversized.inputs, &oversized.operation).is_err());
            Ok(())
        })();
        result.unwrap();
    }

    #[test]
    fn action_identity_excludes_lease_and_workload_but_binds_operation_and_inputs() {
        let result: RailResult<()> = (|| {
            let request = request_for(SOURCE)?;
            let mut retried = request.clone();
            retried.lease_id = fixed_identity("execution-lease-v3:sha256:", '5');
            assert_eq!(action_identity(&request)?, action_identity(&retried)?);

            let mut another_workload = request.clone();
            another_workload.workload_identity = fixed_identity("workload-v1:sha256:", '6');
            assert_eq!(action_identity(&request)?, action_identity(&another_workload)?);

            let mut changed_operation = request.clone();
            changed_operation.operation.edition = "2021".to_string();
            assert_ne!(action_identity(&request)?, action_identity(&changed_operation)?);

            let mut changed_crate_type = request.clone();
            changed_crate_type.operation.crate_type = RustLibraryCrateType::Lib;
            assert_ne!(action_identity(&request)?, action_identity(&changed_crate_type)?);

            let mut changed_emission = request.clone();
            changed_emission.operation.emission = RustLibraryEmission::Metadata;
            assert_ne!(action_identity(&request)?, action_identity(&changed_emission)?);

            let mut changed_source = request.clone();
            changed_source.inputs[0].content_digest = fixed_identity("sha256:", '6');
            assert_ne!(action_identity(&request)?, action_identity(&changed_source)?);

            let mut changed_resources = request.clone();
            changed_resources.limits.memory_bytes /= 2;
            assert_ne!(action_identity(&request)?, action_identity(&changed_resources)?);
            Ok(())
        })();
        result.unwrap();
    }

    #[test]
    fn canonical_virtual_root_is_not_a_physical_worker_path_leak() {
        let virtual_bytes = format!("diagnostic at {VIRTUAL_WORKSPACE}/src/lib.rs");
        assert!(!physical_worker_root_remains(
            virtual_bytes.as_bytes(),
            Path::new(VIRTUAL_ROOT)
        ));
        assert!(physical_worker_root_remains(
            b"diagnostic at /private/attempt/workspace/src/lib.rs",
            Path::new("/private/attempt")
        ));
        assert!(!physical_worker_root_remains(
            b"diagnostic at /cargo-rail/exec/v3/workspace/src/lib.rs",
            Path::new("/private/attempt")
        ));
    }

    #[test]
    fn compiler_stream_rebinding_handles_json_escaped_windows_paths() {
        let attempt = Path::new(r"C:\Users\runner\attempt");
        let workspace = Path::new(r"C:\Users\runner\attempt\workspace");
        let mut stream = br#"{"artifact":"C:\\Users\\runner\\attempt\\workspace\\target/release/lib.rlib"}"#.to_vec();

        rebind_compiler_stream(&mut stream, workspace, attempt).expect("stream rebinding");

        assert_eq!(
            stream,
            br#"{"artifact":"/cargo-rail/exec/v3/workspace\\target/release/lib.rlib"}"#
        );
        assert!(!physical_worker_root_remains(&stream, attempt));
    }

    #[test]
    fn automatic_remote_execution_requires_qualified_isolation_without_changing_compiler_equivalence() {
        let result: RailResult<()> = (|| {
            let process_only = capability()?;
            assert!(!worker_isolation_allowed(&process_only, false));
            assert!(worker_isolation_allowed(&process_only, true));

            let mut sandboxed = process_only.clone();
            sandboxed.operating_system = "linux".to_string();
            sandboxed.isolation = WorkerIsolation::BubblewrapLinuxV2;
            sandboxed.isolation_identity = fixed_identity("isolation-v2:sha256:", '8');
            sandboxed.filesystem_contract = "bubblewrap-bounded-tmpfs-v2".to_string();
            sandboxed.capability_id = capability_identity(&sandboxed)?;
            validate_capability(&sandboxed)?;
            assert!(worker_isolation_allowed(&sandboxed, false));

            let mut selected = process_only;
            selected.operating_system = "linux".to_string();
            selected.capability_id = capability_identity(&selected)?;
            assert!(worker_execution_environment_matches(&sandboxed, &selected)?);
            sandboxed.rustc_content_digest = fixed_identity("sha256:", '9');
            sandboxed.capability_id = capability_identity(&sandboxed)?;
            assert!(!worker_execution_environment_matches(&sandboxed, &selected)?);

            let mut fewer_processes = selected.clone();
            fewer_processes.resource_limits.max_processes -= 1;
            fewer_processes.capability_id = capability_identity(&fewer_processes)?;
            assert!(!worker_execution_environment_matches(&fewer_processes, &selected)?);
            Ok(())
        })();
        result.unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_resource_evidence_parsers_require_exact_named_values() {
        assert_eq!(
            parse_cgroup_event("usage_usec 8\nnr_periods 5\nnr_throttled 3\n", "nr_throttled"),
            Some(3)
        );
        assert_eq!(
            parse_cgroup_event("low 0\nhigh 2\nmax 7\noom 3\noom_kill 4\n", "oom_kill"),
            Some(4)
        );
        assert_eq!(parse_cgroup_event("max 7\n", "oom_kill"), None);
        assert_eq!(parse_cgroup_event("oom_kill invalid\n", "oom_kill"), None);
        assert_eq!(parse_tmpfs_size_option("size=524288k"), Some(512 * 1024 * 1024));
        assert_eq!(parse_tmpfs_size_option("size=512M"), Some(512 * 1024 * 1024));
        assert_eq!(parse_tmpfs_size_option("nosize=512M"), None);
        assert!(valid_cgroup_attempt_name(&format!("attempt-{}", "a".repeat(64))));
        assert!(!valid_cgroup_attempt_name(&format!(
            "attempt-sha256:{}",
            "a".repeat(64)
        )));
        assert!(!valid_cgroup_attempt_name(&format!("attempt-{}", "A".repeat(64))));
        assert!(!valid_cgroup_attempt_name("attempt-short"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_resource_probes_are_small_and_falsify_each_controller() {
        let cpu = CgroupProbe::Cpu.limits();
        let memory = CgroupProbe::Memory.limits();
        let processes = CgroupProbe::Processes.limits();
        validate_limits(cpu).expect("CPU probe limits must be valid");
        validate_limits(memory).expect("memory probe limits must be valid");
        validate_limits(processes).expect("process probe limits must be valid");
        assert_eq!(cpu.cpu_quota_micros, 1_000);
        assert_eq!(memory.memory_bytes, 64 * 1024 * 1024);
        assert_eq!(processes.max_processes, 4);
        assert!(cpu.cpu_quota_micros < worker_execution_limits().cpu_quota_micros);
        assert!(memory.memory_bytes < worker_execution_limits().memory_bytes);
        assert!(processes.max_processes < worker_execution_limits().max_processes);
    }

    #[test]
    fn placement_class_excludes_action_names_but_binds_cost_shape_and_endpoint() {
        let result: RailResult<()> = (|| {
            let first = placement_candidate(b"pub fn first() -> u8 { 1 }\n")?;
            let mut second = placement_candidate(b"pub fn other() -> u8 { 2 }\n")?;
            second.operation.crate_name = "another_crate".to_string();
            second.operation.metadata = "fedcba9876543210".to_string();
            second.operation.extra_filename = "-fedcba9876543210".to_string();
            let capability = fixed_identity("sha256:", 'a');
            let first_class = first.placement_observation(&capability, "10.0.0.1:39443")?;
            let second_class = second.placement_observation(&capability, "10.0.0.1:39443")?;
            assert_eq!(first_class, second_class);
            assert_ne!(first_class, first.placement_observation(&capability, "10.0.0.2:39443")?);

            second.operation.codegen.opt_level = Some("3".to_string());
            assert_ne!(
                first_class,
                second.placement_observation(&capability, "10.0.0.1:39443")?
            );
            second.operation.codegen.opt_level = None;
            second.operation.emission = RustLibraryEmission::Metadata;
            assert_ne!(
                first_class,
                second.placement_observation(&capability, "10.0.0.1:39443")?
            );
            Ok(())
        })();
        result.unwrap();
    }

    #[test]
    fn automatic_placement_requires_a_conservative_fresh_material_win() {
        let result: RailResult<()> = (|| {
            let observation = placement_candidate(SOURCE)?
                .placement_observation(&fixed_identity("sha256:", 'a'), "10.0.0.1:39443")?;
            let now = 1_000_000;
            let mut history = PlacementHistory {
                entries: BTreeMap::new(),
                version: PLACEMENT_HISTORY_VERSION,
            };
            assert_eq!(
                placement_decision_at(&history, &observation, now),
                PlacementDecision::Local("distributed_cost_history_unavailable")
            );
            let entry = history.entries.entry(observation.key.clone()).or_default();
            entry.local = Some(estimate(2_000_000_000, 100_000_000, 3, now));
            assert_eq!(
                placement_decision_at(&history, &observation, now),
                PlacementDecision::Local("distributed_cost_history_incomplete")
            );
            if let Some(entry) = history.entries.get_mut(&observation.key) {
                entry.remote = Some(estimate(1_000_000_000, 100_000_000, 2, now));
            }
            assert_eq!(
                placement_decision_at(&history, &observation, now),
                PlacementDecision::Local("distributed_cost_history_insufficient")
            );
            if let Some(entry) = history.entries.get_mut(&observation.key) {
                entry.remote = Some(estimate(1_000_000_000, 100_000_000, 3, now));
            }
            assert_eq!(
                placement_decision_at(&history, &observation, now),
                PlacementDecision::Delegate
            );

            if let Some(entry) = history.entries.get_mut(&observation.key) {
                entry.remote = Some(estimate(1_700_000_000, 100_000_000, 3, now));
            }
            assert_eq!(
                placement_decision_at(&history, &observation, now),
                PlacementDecision::Local("distributed_predicted_cost_not_lower")
            );
            if let Some(entry) = history.entries.get_mut(&observation.key) {
                entry.remote = Some(estimate(1_000_000_000, 100_000_000, 3, now));
                entry.retry_after_unix_secs = now + 1;
            }
            assert_eq!(
                placement_decision_at(&history, &observation, now),
                PlacementDecision::Local("distributed_worker_backoff_active")
            );
            if let Some(entry) = history.entries.get_mut(&observation.key) {
                entry.retry_after_unix_secs = 0;
                entry.local = Some(estimate(
                    2_000_000_000,
                    100_000_000,
                    3,
                    now - PLACEMENT_HISTORY_MAX_AGE_SECS - 1,
                ));
            }
            assert_eq!(
                placement_decision_at(&history, &observation, now),
                PlacementDecision::Local("distributed_cost_history_stale")
            );
            Ok(())
        })();
        result.unwrap();
    }

    #[test]
    fn placement_history_is_canonical_bounded_and_uses_integer_ewma() {
        let result: RailResult<()> = (|| {
            let observation = placement_candidate(SOURCE)?
                .placement_observation(&fixed_identity("sha256:", 'a'), "10.0.0.1:39443")?;
            let mut entry = PlacementHistoryEntry::default();
            observe_estimate(&mut entry.local, Duration::from_secs(2), 100);
            observe_estimate(&mut entry.local, Duration::from_secs(1), 101);
            let local = entry
                .local
                .ok_or_else(|| RailError::message("placement test did not retain a local estimate"))?;
            assert_eq!(local.mean_ns, 1_750_000_000);
            assert_eq!(local.deviation_ns, 250_000_000);
            assert_eq!(local.samples, 2);
            let history = PlacementHistory {
                entries: BTreeMap::from([(observation.key, entry)]),
                version: PLACEMENT_HISTORY_VERSION,
            };
            let encoded = encode_placement_history(&history)?;
            assert_eq!(decode_placement_history(&encoded)?, history);
            let mut noncanonical = encoded;
            noncanonical.insert(0, b' ');
            decode_placement_history(&noncanonical).unwrap_err();
            Ok(())
        })();
        result.unwrap();
    }

    #[test]
    fn connection_leases_are_fresh_and_bound_to_the_client_action_and_capability() {
        let result: RailResult<()> = (|| {
            let capability = capability()?;
            let request = request_for(SOURCE)?;
            let lease_request = LeaseRequest {
                action_id: request.action_id.clone(),
                capability_id: capability.capability_id.clone(),
                client_nonce: fixed_identity("sha256:", '9'),
                protocol_version: PROTOCOL_VERSION,
                workload_identity: request.workload_identity,
            };
            validate_lease_request(&lease_request, &capability)?;
            let first = grant_connection_lease(&lease_request)?;
            let second = grant_connection_lease(&lease_request)?;
            validate_lease_grant(&first, &lease_request)?;
            validate_lease_grant(&second, &lease_request)?;
            assert_ne!(first.lease_id, second.lease_id);

            let mut another_client = lease_request.clone();
            another_client.client_nonce = fixed_identity("sha256:", '8');
            validate_lease_grant(&first, &another_client).unwrap();
            let another_grant = grant_connection_lease(&another_client)?;
            assert_ne!(first.lease_id, another_grant.lease_id);

            let mut forged_action = lease_request.clone();
            forged_action.action_id = fixed_identity("execution-action-v3:sha256:", '7');
            assert!(validate_lease_grant(&first, &forged_action).is_err());

            let mut another_workload = lease_request.clone();
            another_workload.workload_identity = fixed_identity("workload-v1:sha256:", '7');
            assert!(validate_lease_grant(&first, &another_workload).is_err());
            assert_ne!(first.lease_id, grant_connection_lease(&another_workload)?.lease_id);

            let mut forged_capability = lease_request;
            forged_capability.capability_id = fixed_identity("worker-capability-v3:sha256:", '6');
            assert!(validate_lease_request(&forged_capability, &capability).is_err());
            assert!(validate_lease_grant(&first, &forged_capability).is_err());
            Ok(())
        })();
        result.unwrap();
    }

    #[test]
    fn control_frames_require_canonical_bounded_json() {
        let result: RailResult<()> = (|| {
            let capability = capability()?;
            let mut encoded = Vec::new();
            write_control_frame(&mut encoded, CAPABILITY_MAGIC, CAPABILITY_TRAILER, &capability)?;
            let decoded: WorkerCapability = read_control_frame(
                &mut Cursor::new(&encoded),
                CAPABILITY_MAGIC,
                CAPABILITY_TRAILER,
                "test capability",
            )?;
            assert_eq!(decoded, capability);

            let header_length = u32::from_le_bytes(
                encoded[8..12]
                    .try_into()
                    .map_err(|_| RailError::message("test control frame did not contain its length"))?,
            ) as usize;
            let header_end = 12 + header_length;
            encoded.insert(header_end, b' ');
            encoded[8..12].copy_from_slice(
                &u32::try_from(header_length + 1)
                    .map_err(|_| RailError::message("test control frame length overflowed"))?
                    .to_le_bytes(),
            );
            read_control_frame::<WorkerCapability>(
                &mut Cursor::new(encoded),
                CAPABILITY_MAGIC,
                CAPABILITY_TRAILER,
                "test capability",
            )
            .unwrap_err();
            Ok(())
        })();
        result.unwrap();
    }

    #[test]
    fn client_disconnect_is_a_cancellation_signal() {
        let result: RailResult<()> = (|| {
            let listener = TcpListener::bind("127.0.0.1:0")?;
            let client = TcpStream::connect(listener.local_addr()?)?;
            let (server, _) = listener.accept()?;
            let stopped = Arc::new(AtomicBool::new(false));
            let monitor_stopped = Arc::clone(&stopped);
            let (sender, receiver) = mpsc::sync_channel(1);
            let monitor = thread::spawn(move || monitor_client_connection(server, &monitor_stopped, sender));
            drop(client);
            assert!(matches!(
                receiver.recv_timeout(Duration::from_secs(2)),
                Ok(Cancellation::ClientLost)
            ));
            stopped.store(true, Ordering::Release);
            monitor
                .join()
                .map_err(|_| RailError::message("test connection monitor panicked"))?;
            Ok(())
        })();
        result.unwrap();
    }

    #[test]
    fn request_validation_rejects_forgery_and_path_shaped_arguments() {
        let result: RailResult<()> = (|| {
            let capability = capability()?;
            let mut forged = request_for(SOURCE)?;
            forged.operation.crate_name = "../escape".to_string();
            forged.action_id = action_identity(&forged)?;
            assert!(validate_request(&forged, &capability).is_err());

            let mut forged = request_for(SOURCE)?;
            forged.operation.extra_filename = "-/escape".to_string();
            forged.action_id = action_identity(&forged)?;
            assert!(validate_request(&forged, &capability).is_err());

            let mut forged = request_for(SOURCE)?;
            forged.action_id = fixed_identity("execution-action-v3:sha256:", '7');
            assert!(validate_request(&forged, &capability).is_err());
            Ok(())
        })();
        result.unwrap();
    }

    #[test]
    fn result_decoder_stages_only_digest_verified_authorized_slots() {
        let result: RailResult<()> = (|| {
            let request = request_for(SOURCE)?;
            let encoded = response_bytes(&request)?;
            let staging_parent = tempfile::tempdir()?;
            let staging = NativeResultStaging::temporary_in(staging_parent.path())?;
            let mut timing = ResponseTiming::default();
            let decoded = read_response_into(&mut Cursor::new(encoded), &request, staging, &mut timing)?;
            let DecodedExecution::Success(staged) = decoded else {
                return Err(RailError::message("test response was unexpectedly rejected"));
            };
            assert_eq!(staged.frames.len(), 5);
            assert_eq!(fs::read(&staged.frames[&ResponseSlot::Metadata])?, b"metadata bytes");
            assert_eq!(fs::read(&staged.frames[&ResponseSlot::Rlib])?, b"rlib bytes");
            assert!(staged.staging.path().starts_with(staging_parent.path()));
            assert_eq!(timing.remote_execution.count, 1);
            assert_eq!(timing.result_transfer.count, 1);
            assert_eq!(
                timing.result_bytes,
                staged
                    .descriptors
                    .values()
                    .fold(0_u64, |total, frame| total.saturating_add(frame.bytes))
            );
            Ok(())
        })();
        result.unwrap();
    }

    #[test]
    fn success_response_slot_set_is_bound_to_the_requested_emission() {
        let result: RailResult<()> = (|| {
            let request = request_for(SOURCE)?;
            let payloads = tempfile::tempdir()?;
            let frames = success_frames(payloads.path())?;
            let timing = worker_timing(&request, &frames);
            let (response, _) = successful_response(&request, frames, timing);

            let mut missing_rlib = response.clone();
            missing_rlib.frames.remove(2);
            assert!(validate_response_header(&missing_rlib, &request.operation).is_err());

            let mut metadata_request = request;
            metadata_request.operation.emission = RustLibraryEmission::Metadata;
            assert!(validate_response_header(&response, &metadata_request.operation).is_err());
            Ok(())
        })();
        result.unwrap();
    }

    #[test]
    fn result_decoder_rejects_corruption_and_mismatched_authority() {
        let result: RailResult<()> = (|| {
            let request = request_for(SOURCE)?;
            let encoded = response_bytes(&request)?;
            let header_length = u32::from_le_bytes(
                encoded[8..12]
                    .try_into()
                    .map_err(|_| RailError::message("test response frame did not contain its length"))?,
            ) as usize;
            let first_payload = 12 + header_length;
            let mut corrupted = encoded.clone();
            corrupted[first_payload] ^= 0xff;
            let staging_parent = tempfile::tempdir()?;
            let staging = NativeResultStaging::temporary_in(staging_parent.path())?;
            assert!(
                read_response_into(
                    &mut Cursor::new(corrupted),
                    &request,
                    staging,
                    &mut ResponseTiming::default()
                )
                .is_err()
            );

            let mut wrong_request = request.clone();
            wrong_request.lease_id = fixed_identity("execution-lease-v3:sha256:", '8');
            let staging = NativeResultStaging::temporary_in(staging_parent.path())?;
            assert!(
                read_response_into(
                    &mut Cursor::new(encoded),
                    &wrong_request,
                    staging,
                    &mut ResponseTiming::default()
                )
                .is_err()
            );

            let mut trailing = response_bytes(&request)?;
            trailing.push(0);
            let staging = NativeResultStaging::temporary_in(staging_parent.path())?;
            assert!(
                read_response_into(
                    &mut Cursor::new(trailing),
                    &request,
                    staging,
                    &mut ResponseTiming::default()
                )
                .is_err()
            );
            Ok(())
        })();
        result.unwrap();
    }

    #[test]
    fn response_rejects_worker_timing_that_does_not_match_the_attempt() {
        let result: RailResult<()> = (|| {
            let request = request_for(SOURCE)?;
            let payloads = tempfile::tempdir()?;
            let frames = success_frames(payloads.path())?;
            let timing = worker_timing(&request, &frames);
            let (mut response, _) = successful_response(&request, frames, timing);
            validate_worker_timing(&response, &request)?;

            response.worker_timing.source_bytes = response.worker_timing.source_bytes.saturating_add(1);
            assert!(validate_worker_timing(&response, &request).is_err());
            response.worker_timing = timing;
            response.worker_timing.result_bytes = response.worker_timing.result_bytes.saturating_add(1);
            assert!(validate_worker_timing(&response, &request).is_err());
            response.worker_timing = timing;
            response.worker_timing.elapsed_ns = timing.compiler_ns;
            assert!(validate_worker_timing(&response, &request).is_err());
            Ok(())
        })();
        result.unwrap();
    }

    #[test]
    fn rejection_response_carries_no_artifact_payload() {
        let result: RailResult<()> = (|| {
            let request = request_for(SOURCE)?;
            let timing = worker_timing(&request, &[]);
            let (response, frames) = rejected_response(&request, "execution_cancelled", timing);
            assert_eq!(response.reason.as_deref(), Some("execution_cancelled"));
            let mut encoded = Vec::new();
            write_response(&mut encoded, &request, &response, &frames)?;
            let staging_parent = tempfile::tempdir()?;
            let staging = NativeResultStaging::temporary_in(staging_parent.path())?;
            let mut timing = ResponseTiming::default();
            let decoded = read_response_into(&mut Cursor::new(encoded), &request, staging, &mut timing)?;
            assert_eq!(timing.result_transfer.count, 0);
            assert_eq!(timing.result_bytes, 0);
            let DecodedExecution::Rejected = decoded else {
                return Err(RailError::message("test rejection unexpectedly admitted artifacts"));
            };
            Ok(())
        })();
        result.unwrap();
    }

    #[test]
    fn compiler_failure_preserves_termination_and_only_diagnostic_streams() {
        let result: RailResult<()> = (|| {
            let request = request_for(SOURCE)?;
            let frames = vec![
                prepare_bytes_frame(ResponseSlot::Stderr, b"compiler failed".to_vec(), MAX_STREAM_BYTES)?,
                prepare_bytes_frame(ResponseSlot::Stdout, b"compiler context".to_vec(), MAX_STREAM_BYTES)?,
            ];
            let termination = CompilerTermination::Signal { signal: 9 };
            let timing = worker_timing(&request, &frames);
            let (response, frames) = compiler_failure_response(&request, termination, frames, timing);
            let mut encoded = Vec::new();
            write_response(&mut encoded, &request, &response, &frames)?;
            let staging_parent = tempfile::tempdir()?;
            let staging = NativeResultStaging::temporary_in(staging_parent.path())?;
            let decoded = read_response_into(
                &mut Cursor::new(encoded),
                &request,
                staging,
                &mut ResponseTiming::default(),
            )?;
            let DecodedExecution::CompilerFailed {
                termination: decoded_termination,
                result,
            } = decoded
            else {
                return Err(RailError::message("test compiler failure changed outcome class"));
            };
            assert_eq!(decoded_termination, termination);
            assert_eq!(result.frames.len(), 2);
            assert_eq!(fs::read(&result.frames[&ResponseSlot::Stderr])?, b"compiler failed");
            assert_eq!(fs::read(&result.frames[&ResponseSlot::Stdout])?, b"compiler context");
            Ok(())
        })();
        result.unwrap();
    }

    #[test]
    fn local_attempt_falls_back_only_before_the_visible_effect_boundary() {
        let result: RailResult<()> = (|| {
            use std::cell::Cell;

            let admitted = Cell::new(0_u8);
            let cold = decide_local_attempt(LocalWorkerAttempt::Cold("worker_lost"), |_| {
                admitted.set(admitted.get().saturating_add(1));
                LocalAdmission::Committed(0)
            });
            assert!(matches!(cold, LocalAttemptDecision::Fallback("worker_lost")));
            assert_eq!(admitted.get(), 0);

            let before = decide_local_attempt(LocalWorkerAttempt::Success(empty_staged_result()?), |_| {
                admitted.set(admitted.get().saturating_add(1));
                LocalAdmission::RejectedBeforeEffect("local_admission_rejected")
            });
            assert!(matches!(
                before,
                LocalAttemptDecision::Fallback("local_admission_rejected")
            ));
            assert_eq!(admitted.get(), 1);

            let after = decide_local_attempt(LocalWorkerAttempt::Success(empty_staged_result()?), |_| {
                admitted.set(admitted.get().saturating_add(1));
                LocalAdmission::FailedAfterEffect(RailError::message("restore crossed its effect boundary"))
            });
            assert!(matches!(after, LocalAttemptDecision::OperationalFailure(_)));
            assert_eq!(admitted.get(), 2);

            let committed = decide_local_attempt(LocalWorkerAttempt::Success(empty_staged_result()?), |_| {
                LocalAdmission::Committed(0)
            });
            assert!(matches!(committed, LocalAttemptDecision::Completed(0)));
            Ok(())
        })();
        result.unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn local_client_collapses_a_malformed_worker_to_cold_without_admitting_bytes() {
        let result: RailResult<()> = (|| {
            use std::os::unix::fs::PermissionsExt as _;

            let root = tempfile::tempdir()?;
            let mut rustup = Command::new("rustup");
            rustup
                .args(["which", "rustc"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let selected = run_bounded_command(rustup, Duration::from_secs(30), MAX_HEADER_BYTES as u64)?;
            if !selected.status.success() || !selected.stderr.is_empty() {
                return Err(RailError::message("test rustc selection failed"));
            }
            let rustc =
                String::from_utf8(selected.stdout).map_err(|_| RailError::message("test rustc path was not UTF-8"))?;
            let rustc = PathBuf::from(rustc.trim());
            let capability = capture_worker_capability(rustc.as_os_str(), None)?.capability;
            let capability_path = root.path().join("capability.json");
            fs::write(&capability_path, canonical_json(&capability)?)?;
            let worker = root.path().join("malformed-worker");
            let quoted_capability = capability_path.to_string_lossy().replace('\'', "'\\''");
            let script = format!(
                "#!/bin/sh\ncase \"$1\" in\n  capability) /bin/cat '{quoted_capability}'; printf '\\n' ;;\n  execute) printf 'not-a-response' ;;\n  *) exit 2 ;;\nesac\n"
            );
            fs::write(&worker, script)?;
            fs::set_permissions(&worker, fs::Permissions::from_mode(0o700))?;
            let candidate = RustLibraryCandidate::new(
                RustLibraryCandidateInput {
                    crate_name: "distributed_fixture".to_string(),
                    crate_type: "rlib".to_string(),
                    dep_info_name: "distributed_fixture-0123456789abcdef.d".to_string(),
                    edition: "2024".to_string(),
                    emission: RustLibraryEmission::MetadataAndLink,
                    metadata: "0123456789abcdef".to_string(),
                    metadata_name: "libdistributed_fixture-0123456789abcdef.rmeta".to_string(),
                    extra_filename: "-0123456789abcdef".to_string(),
                    output_relative_directory: "target/debug/deps".to_string(),
                    source_relative_path: "src/lib.rs".to_string(),
                    test_mode: false,
                    toolchain_proc_macro: false,
                    rlib_name: Some("libdistributed_fixture-0123456789abcdef.rlib".to_string()),
                    options: RustLibraryExecutionOptions::default(),
                },
                SOURCE.to_vec(),
            )?;
            let staging = root.path().join("staging");
            fs::create_dir(&staging)?;
            let native_staging = NativeResultStaging::temporary_in(&staging)?;
            let mut timing = DistributedTiming::default();
            let result = execute_local_worker(
                &worker,
                rustc.as_os_str(),
                &candidate,
                native_staging,
                None,
                &mut timing,
            );
            assert!(matches!(
                result,
                LocalWorkerAttempt::Cold("distributed_execution_unavailable")
            ));
            assert_eq!(fs::read_dir(&staging)?.count(), 0);
            Ok(())
        })();
        result.unwrap();
    }
}
