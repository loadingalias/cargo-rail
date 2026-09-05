//! Target-aware compiler diagnostics collection with persistent caching.

use crate::build_script::{
    BuildScriptActionInputs, BuildScriptCargoOutputSummary, BuildScriptResultInputs,
    analyze_action_key as analyze_build_script_action_key, analyze_result as analyze_build_script_result,
};
use crate::cache::cas::{LocalCacheSelection, LocalCas};
use crate::cargo::manifest_analyzer::ManifestAnalyzer;
use crate::cargo::{CargoConfigSnapshot, DepKind, ToolchainIdentity};
use crate::compiler::acquisition::journal::{
    CompilerAcquisitionCargoTarget, CompilerAcquisitionJournal, EvidenceIdentity, FailureClass,
};
pub(crate) use crate::compiler::acquisition::journal::{
    CompilerAcquisitionProduct, CompilerAcquisitionRequest, validate_compiler_acquisition_resume,
};
use crate::compiler::acquisition::output::{read_cargo_stderr_tail, read_cargo_stdout};
use crate::compiler::acquisition::process::{ProcessTermination, ProcessTree};
use crate::compiler::acquisition::runtime::{ExecutionPolicy, RuntimeState, RuntimeViewSpec};
use crate::compiler::acquisition::sandbox::{SandboxCompatibility, SandboxLease, SandboxPool};
use crate::compiler::analysis::AnalysisContract;
use crate::compiler::diagnostics_store::{CompilerDiagnosticsStore, CompilerFactCacheKey};
use crate::compiler::driver::{CompilerFactDoctestSysroot, CompilerFactDriverAuthority, PreparedCompilerFactDriver};
use crate::compiler::fact_store::CompilerFactStore;
use crate::compiler::facts::{
    COMPILER_FACT_ANNOUNCEMENT_CODE, COMPILER_FACT_ANNOUNCEMENT_PREFIX, COMPILER_FACT_PROTOCOL_VERSION,
    CompilerFactAnnouncement, CompilerFactAnnouncementExpectation, CompilerFactExpectation, CompilerFactRunAuthority,
    CompilerFactTargetKind, RUN_IDENTITY_PREFIX, ValidatedCompilerFactAnnouncement, ValidatedCompilerFactFragment,
    ValidatedCompilerFactObject, load_announced_fragment, load_discovered_doctest_fragment,
    required_compiler_fact_coverage,
};
use crate::compiler::invocation::{
    CACHE_WRAPPER_MARKER, INNER_RUSTDOC_ENV, INNER_WRAPPER_ENV, OBSERVATION_DIRECTORY_ENV, OBSERVATION_SOURCE_ROOT_ENV,
    RUSTDOC_WRAPPER_MARKER, WRAPPER_MARKER,
};
use crate::compiler::model::{
    COLLECTOR_VERSION, CargoTargetKind, CompilationUnitEvidence, CompilationUnitId, CompilerDiagEntry, CompilerDiagKey,
    DependencyEvidenceState, DiagnosticsCompleteness, EvidenceCacheSummary, FeatureSelection, MemberEvidence,
    PlatformTarget, TargetEvidence,
};
use crate::compiler::native_cache::{NativeCompilerSession, NativeSessionAuthority};
use crate::compiler::observation::{
    BuildScriptResultBinding, CargoArtifactObservation, CompilationObservationContext, CompilationObservationManifest,
    CompilationProfile, CompilerCacheWrapperMetadata, CompilerCacheWrapperStatus, CompilerMode,
    CompilerWrapperIdentity, CompilerWrapperRole, FileObservation, ObservationPath,
    attach_build_script_result_dependencies, attach_execution_identities, build_manifests, load_raw,
};
use crate::compiler::scheduler::{
    AnalysisSchedule, CompilerAcquisitionPlan, CompilerAcquisitionView, CompilerCandidate, ViewIx,
};
use crate::compiler::session::{CompilerFactSession, CompilerFactTypedSession, FACT_SESSION_ENV};
use crate::error::{RailError, RailResult, ResultExt};
use crate::executable::{ExecutableIdentity, ToolchainExecutableIdentities};
use crate::progress;
use crate::source::{ContentDigest, SourceEntryKind};
use crate::workspace::WorkspaceSnapshot;
use cargo_metadata::{Message, PackageId, TargetKind};
use rscrypto::Sha256;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::{Arc, atomic::AtomicBool, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(test)]
static QUALIFICATION_CARGO_VIEWS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static QUALIFICATION_COMPILER_INVOCATIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static QUALIFICATION_LIVE_CARGO_VIEWS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static QUALIFICATION_MAX_LIVE_CARGO_VIEWS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Compiler diagnostics collector and cache coordinator.
pub(crate) struct CompilerDiagnosticsCollector<'a> {
    workspace_root: &'a Path,
    manifests: &'a ManifestAnalyzer,
    targets: Vec<&'a str>,
    identity: CompilerCacheIdentity,
    artifact_budget: CompilerArtifactBudget,
    acquisition: Option<CompilerAcquisitionRequest>,
    #[cfg(test)]
    execution_policy: Option<ExecutionPolicy>,
}

/// Storage authority for the command-owned Cargo working set shared across evidence views.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CompilerArtifactBudget {
    soft_limit_bytes: u64,
    hard_limit_bytes: u64,
}

impl CompilerArtifactBudget {
    pub(crate) const fn new(soft_limit_bytes: u64, hard_limit_bytes: u64) -> Self {
        Self {
            soft_limit_bytes,
            hard_limit_bytes,
        }
    }
}

impl Default for CompilerArtifactBudget {
    fn default() -> Self {
        Self::new(32 * 1024 * 1024 * 1024, 64 * 1024 * 1024 * 1024)
    }
}

/// Independently validated products from one shared set of Cargo acquisitions.
pub(crate) struct CompilerAnalysisEvidence {
    pub(crate) diagnostics: HashMap<PackageId, MemberEvidence>,
    pub(crate) compiler_facts: Vec<ValidatedCompilerFactObject>,
    pub(crate) metrics: CompilerAnalysisMetrics,
}

/// Work performed to acquire one combined compiler-evidence set.
#[derive(Debug, Clone, Default)]
pub(crate) struct CompilerAnalysisMetrics {
    pub(crate) analysis_views: usize,
    pub(crate) cargo_views_executed: usize,
    pub(crate) compiler_invocations: usize,
    pub(crate) diagnostic_cache_hits: usize,
    pub(crate) diagnostic_cache_misses: usize,
    pub(crate) fact_cache_hits: usize,
    pub(crate) fact_cache_misses: usize,
    pub(crate) fact_cache_store_failures: usize,
    pub(crate) fact_cache_bypass_reasons: BTreeMap<String, usize>,
    pub(crate) fresh_fragment_bytes: u64,
    pub(crate) retained_fact_object_bytes: u64,
    pub(crate) artifact_high_water_bytes: u64,
}

fn compiler_artifact_bytes(root: &Path) -> RailResult<u64> {
    let started = crate::instrumentation::compiler_acquisition_timer();
    let result = (|| {
        let mut bytes = 0_u64;
        let mut pending = vec![root.to_path_buf()];
        while let Some(path) = pending.pop() {
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() && !crate::utils::is_symlink_or_reparse(&metadata) {
                for entry in fs::read_dir(&path)? {
                    pending.push(entry?.path());
                }
            } else if metadata.is_file() && !crate::utils::is_symlink_or_reparse(&metadata) {
                bytes = bytes
                    .checked_add(metadata.len())
                    .ok_or_else(|| RailError::message("compiler artifact byte count overflow"))?;
            } else {
                return Err(RailError::message(format!(
                    "compiler artifact view contains unsupported path '{}'",
                    path.display()
                )));
            }
        }
        Ok(bytes)
    })();
    crate::instrumentation::record_compiler_acquisition_artifact_tree_walk(started);
    result
}

/// Exact snapshot-derived inputs shared by every compiler-evidence key.
#[derive(Debug, Clone)]
pub(crate) struct CompilerCacheIdentity {
    rustc_version: String,
    cargo_version: String,
    host_triple: String,
    toolchain_fingerprint: String,
    target_fingerprints: HashMap<String, String>,
    lock_fingerprint: String,
    compiler_env_fingerprint: String,
    cargo_config_fingerprint: String,
    cargo_program: OsString,
    rustc_program: OsString,
    rustdoc_program: OsString,
    rustc_wrapper: Option<OsString>,
    rustc_workspace_wrapper: Option<OsString>,
    manifest_fingerprints: HashMap<PackageId, String>,
    source_fingerprints: HashMap<PackageId, String>,
    observation_context: CompilationObservationContext,
    package_observation_identities: HashMap<PackageId, String>,
    package_observation_manifests: HashMap<PathBuf, String>,
    package_dependencies: HashMap<String, BTreeSet<String>>,
    build_script_packages: HashMap<String, BuildScriptPackageContext>,
    rustc_executable: ExecutableIdentity,
    wrapper_chain: Vec<CompilerWrapperIdentity>,
    cache_wrapper: CompilerCacheWrapperMetadata,
    analysis_cache: Option<CompilerAnalysisCache>,
    explicit_build_jobs: Option<usize>,
    executable_bypasses: BTreeSet<String>,
    cache_bypass_reason: Option<CompilerCacheBypass>,
}

/// One installed local-cache domain held stable for a complete compiler
/// analysis. Native wrappers independently validate the same profile before
/// using it; retaining this authority prevents publication from drifting to a
/// different profile generation between compiler invocations.
#[derive(Debug, Clone)]
struct CompilerAnalysisCache {
    cas: LocalCas,
    remote: Option<std::sync::Arc<crate::remote_cache::RemoteStore>>,
    _selection: crate::cache::installation::InstalledLocalSelection,
}

#[derive(Debug, Clone)]
struct BuildScriptPackageContext {
    package_id: PackageId,
    working_directory: String,
}

/// Exact, root-independent rustc toolchain identity used by native-cache keys.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct NativeToolchainCapability {
    schema_version: u32,
    cache_class: &'static str,
    execution_contract: &'static str,
    transported_work_boundary: &'static str,
    platform: String,
    host_target: String,
    rustc_verbose_version: String,
    rustc_content_digest: String,
    sysroot_identity: String,
    identity: String,
}

impl NativeToolchainCapability {
    pub(crate) fn platform(&self) -> &str {
        &self.platform
    }

    pub(crate) fn host_target(&self) -> &str {
        &self.host_target
    }

    pub(crate) fn identity(&self) -> &str {
        &self.identity
    }
}

const TRANSPARENT_SESSION_MEMO_VERSION: u32 = 2;

/// Regenerable, receipt-private proof that a direct compiler session remains
/// exact without launching two identity probes for every rustc unit.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TransparentNativeSessionMemo {
    version: u32,
    source_root: String,
    rustc_program: String,
    rustc_program_generation: Vec<u8>,
    rustc_sysroot: String,
    host_target: String,
    sysroot_evidence: ExactSysrootEvidence,
    compiler_environment_identity: String,
    session: NativeCompilerSession,
    digest: String,
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TransparentNativeSessionMemo;

#[derive(Debug, Clone, PartialEq, Eq)]
enum CargoBuildScriptOutput {
    One(BuildScriptCargoOutputSummary),
    Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompilerCacheBypass {
    CargoConfiguration,
    ResponseFileConfiguration,
    BuildScriptObservations,
    ProcMacroObservations,
    ExternalSourceDigest,
}

impl CompilerCacheBypass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CargoConfiguration => "cargo_configuration_unmodeled",
            Self::ResponseFileConfiguration => "response_file_configuration_unmodeled",
            Self::BuildScriptObservations => "build_script_observations_unavailable",
            Self::ProcMacroObservations => "proc_macro_observations_unavailable",
            Self::ExternalSourceDigest => "external_source_digest_unavailable",
        }
    }
}

static COMPILER_OBSERVATION_PROCESS: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

fn compiler_observation_wrapper() -> RailResult<PathBuf> {
    #[cfg(test)]
    if let Some(path) = std::env::var_os("CARGO_RAIL_TEST_OBSERVATION_WRAPPER") {
        return crate::utils::canonicalize_existing(Path::new(&path))
            .with_context(|| "locating the explicitly provisioned compiler-observation test wrapper".to_string());
    }
    if let Some(process) = COMPILER_OBSERVATION_PROCESS.get() {
        return Ok(process.clone());
    }
    let executable = std::env::current_exe()
        .with_context(|| "locating cargo-rail while selecting its compiler-observation process".to_string())?;
    let observation = executable.with_file_name(format!(
        "cargo-rail-compiler-observation{}",
        std::env::consts::EXE_SUFFIX
    ));
    let process = if observation.is_file() && compatible_observation_process(&observation) {
        crate::utils::canonicalize_existing(&observation).with_context(|| {
            format!(
                "locating cargo-rail compiler-observation process '{}'",
                observation.display()
            )
        })?
    } else {
        executable
    };
    if COMPILER_OBSERVATION_PROCESS.set(process.clone()).is_err()
        && let Some(selected) = COMPILER_OBSERVATION_PROCESS.get()
    {
        return Ok(selected.clone());
    }
    Ok(process)
}

fn compatible_observation_process(path: &Path) -> bool {
    Command::new(path)
        .arg(crate::compiler::invocation::OBSERVATION_PROTOCOL_ARGUMENT)
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && output.stderr.is_empty()
                && String::from_utf8(output.stdout).is_ok_and(|version| {
                    version.trim().parse::<u32>() == Ok(crate::compiler::invocation::OBSERVATION_PROTOCOL_VERSION)
                })
        })
}

impl CompilerCacheIdentity {
    /// Capture exact compiler-cache identity from one immutable workspace snapshot.
    pub fn capture(snapshot: &WorkspaceSnapshot) -> RailResult<Self> {
        let rustc_version = snapshot.toolchain().rustc_verbose_version().to_string();
        let cargo_version = snapshot.toolchain().cargo_verbose_version().to_string();
        let host_triple = snapshot.toolchain().host_target().to_string();
        let current_executable = compiler_observation_wrapper()?;
        let cargo_rail_executable = ExecutableIdentity::capture(
            current_executable.as_os_str(),
            snapshot.source_root(),
            snapshot.source_root(),
        )?;
        let executables = snapshot.executable_identities()?;
        let cache_bypass_reason = compiler_cache_bypass_reason(snapshot);
        let toolchain_fingerprint =
            executable_toolchain_fingerprint(snapshot.toolchain(), executables, &cargo_rail_executable)?;
        let target_fingerprints = target_fingerprints(snapshot)?;
        let lock_fingerprint = snapshot.lockfile_fingerprint();
        let compiler_env_fingerprint = compiler_env_fingerprint(snapshot.cargo_config())?;
        let cargo_config_fingerprint = cargo_config_fingerprint(snapshot.cargo_config(), snapshot.source_root())?;
        let cargo_program = snapshot.toolchain().cargo_program().to_owned();
        let rustc_program = snapshot.toolchain().rustc_program().to_owned();
        let rustdoc_program = snapshot.toolchain().rustdoc_program().to_owned();
        let rustc_wrapper = snapshot.toolchain().rustc_wrapper_program().map(OsString::from);
        let rustc_workspace_wrapper = snapshot
            .toolchain()
            .rustc_workspace_wrapper_program()
            .map(OsString::from);
        let local_dependencies = declared_local_dependency_graph(snapshot)?;
        let manifest_fingerprints = manifest_closure_fingerprints(snapshot, &local_dependencies)?;
        let source_fingerprints = source_closure_fingerprints(snapshot, &local_dependencies)?;
        let observation_context = CompilationObservationContext::capture(snapshot)?;
        let package_observation_identities = package_observation_identities(snapshot)?;
        let package_observation_manifests =
            package_observation_manifest_identities(snapshot, &package_observation_identities)?;
        let package_dependencies = package_dependency_graph(snapshot, &package_observation_identities)?;
        let build_script_packages = build_script_package_contexts(snapshot, &package_observation_identities)?;
        let rustc_executable = executables.rustc().clone();
        let configured_wrappers = [executables.rustc_wrapper(), executables.rustc_workspace_wrapper()];
        if configured_wrappers
            .iter()
            .flatten()
            .any(|wrapper| wrapper.same_resolved_file(&cargo_rail_executable))
        {
            return Err(RailError::with_help(
                "recursive cargo-rail rustc wrapper configuration",
                "remove cargo-rail from RUSTC_WRAPPER and RUSTC_WORKSPACE_WRAPPER; diagnostics injection is automatic",
            ));
        }
        let verified_installed_rustc_wrapper = rustc_wrapper
            .as_deref()
            .and_then(|selection| {
                crate::cache::installation::verified_installed_wrapper_digest(selection, snapshot.source_root())
                    .ok()
                    .flatten()
            })
            .is_some_and(|digest| {
                executables
                    .rustc_wrapper()
                    .is_some_and(|wrapper| wrapper.content_digest() == digest)
            });
        let executable_bypasses = compiler_evidence_executable_bypasses(
            executables,
            &cargo_rail_executable,
            verified_installed_rustc_wrapper,
        );
        let mut wrapper_chain = Vec::with_capacity(4);
        wrapper_chain.extend(
            executables
                .rustc_wrapper()
                .cloned()
                .map(|executable| CompilerWrapperIdentity::new(CompilerWrapperRole::Global, executable)),
        );
        wrapper_chain.push(CompilerWrapperIdentity::new(
            CompilerWrapperRole::Diagnostic,
            cargo_rail_executable,
        ));
        wrapper_chain.extend(
            executables
                .rustc_workspace_wrapper()
                .cloned()
                .map(|executable| CompilerWrapperIdentity::new(CompilerWrapperRole::Workspace, executable)),
        );
        let cache_wrapper = CompilerCacheWrapperMetadata::new(
            CompilerCacheWrapperStatus::Disabled,
            "transparent_cache_owned_by_cargo_configuration",
        );
        let analysis_cache = crate::compiler::capability::host_is_qualified()
            .then(|| {
                crate::cache::installation::installed_local(snapshot.source_root())
                    .ok()
                    .flatten()
            })
            .flatten()
            .and_then(|selection| {
                let cas = LocalCas::open_initialized_selected(selection.cache()).ok()?;
                let remote =
                    crate::remote_cache::RemoteCacheSelection::from_environment_or_installed(selection.remote())
                        .ok()
                        .flatten()
                        .filter(crate::remote_cache::RemoteCacheSelection::direct_transport_supported)
                        .and_then(|remote| crate::remote_cache::RemoteStore::connect(&remote, None).ok())
                        .map(std::sync::Arc::new);
                Some(CompilerAnalysisCache {
                    cas,
                    remote,
                    _selection: selection,
                })
            });
        let explicit_build_jobs = snapshot.cargo_config().explicit_build_jobs();

        Ok(Self {
            rustc_version,
            cargo_version,
            host_triple,
            toolchain_fingerprint,
            target_fingerprints,
            lock_fingerprint,
            compiler_env_fingerprint,
            cargo_config_fingerprint,
            cargo_program,
            rustc_program,
            rustdoc_program,
            rustc_wrapper,
            rustc_workspace_wrapper,
            manifest_fingerprints,
            source_fingerprints,
            observation_context,
            package_observation_identities,
            package_observation_manifests,
            package_dependencies,
            build_script_packages,
            rustc_executable,
            wrapper_chain,
            cache_wrapper,
            analysis_cache,
            explicit_build_jobs,
            executable_bypasses,
            cache_bypass_reason,
        })
    }

    fn diagnostic_wrapper_executable(&self) -> RailResult<&ExecutableIdentity> {
        let mut wrappers = self
            .wrapper_chain
            .iter()
            .filter(|wrapper| wrapper.role() == CompilerWrapperRole::Diagnostic);
        let wrapper = wrappers
            .next()
            .ok_or_else(|| RailError::message("compiler cache identity has no diagnostic wrapper authority"))?;
        if wrappers.next().is_some() {
            return Err(RailError::message(
                "compiler cache identity repeats the diagnostic wrapper authority",
            ));
        }
        Ok(wrapper.executable())
    }

    fn acquisition_compiler_set_identity(&self) -> RailResult<String> {
        let local_compiler_set = crate::compiler::capability::local_compiler_set_identity()?;
        let targets = self
            .target_fingerprints
            .iter()
            .map(|(target, identity)| (target.as_str(), identity.as_str()))
            .collect::<BTreeMap<_, _>>();
        let bytes = serde_json::to_vec(&(
            &self.rustc_version,
            &self.cargo_version,
            &self.host_triple,
            &self.toolchain_fingerprint,
            targets,
            &self.compiler_env_fingerprint,
            &self.cargo_config_fingerprint,
            local_compiler_set,
        ))?;
        Ok(format!("compiler-set-v2-sha256-{}", ContentDigest::sha256(&bytes)))
    }
}

/// Capture the retained exact v10 session from the compiler Cargo selected for
/// one transparent wrapper invocation. This runs only after acquisition-free
/// eligibility gates have accepted the rustc shape.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn capture_transparent_native_session(
    source_root: &Path,
    target_root: &Path,
    rustc_program: &OsStr,
    cache: &LocalCacheSelection,
    root_portability: crate::cache::installation::InstalledRootPortability,
) -> RailResult<(NativeCompilerSession, u64, TransparentNativeSessionMemo)> {
    let source_root = crate::utils::canonicalize_existing(source_root)?;
    let resolved_rustc = crate::executable::resolve_executable_path(rustc_program, &source_root)?;
    let rustc_program_generation = crate::utils::stable_file_generation(&resolved_rustc)
        .ok_or_else(|| RailError::message("selected rustc has no stable local file generation"))?;
    let rustc_verbose_version = transparent_rustc_query(rustc_program, "-vV", &source_root)?;
    let host_target = rustc_verbose_version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .filter(|host| !host.is_empty())
        .ok_or_else(|| RailError::message("selected rustc verbose identity has no host target"))?;
    let rustc_sysroot = PathBuf::from(transparent_rustc_query(rustc_program, "--print=sysroot", &source_root)?);
    let rustc_sysroot = crate::utils::canonicalize_existing(&rustc_sysroot)?;
    #[cfg(windows)]
    let rustc_implementation = rustc_sysroot.join("bin/rustc.exe");
    #[cfg(not(windows))]
    let rustc_implementation = rustc_sysroot.join("bin/rustc");
    let rustc_content_digest =
        ExecutableIdentity::capture(rustc_implementation.as_os_str(), &source_root, &source_root)?
            .content_digest()
            .to_string();
    let memo_path = compiler_sysroot_memo_path(&rustc_sysroot, host_target, Some(cache));
    let (sysroot_identity, bytes_hashed) =
        compiler_sysroot_fingerprint(&rustc_sysroot, host_target, memo_path.as_deref())?;
    let platform = format!(
        "{}-{}-{}",
        std::env::consts::FAMILY,
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let mut framed = Vec::from(&b"cargo-rail-native-toolchain-capability-v1\0"[..]);
    append_identity_frame(
        &mut framed,
        b"cache-class",
        crate::compiler::native_cache::native_cache_class().as_bytes(),
    );
    append_identity_frame(
        &mut framed,
        b"execution-contract",
        crate::compiler::native_cache::native_cache_execution_contract().as_bytes(),
    );
    append_identity_frame(&mut framed, b"platform", platform.as_bytes());
    append_identity_frame(&mut framed, b"host-target", host_target.as_bytes());
    append_identity_frame(&mut framed, b"rustc-version", rustc_verbose_version.as_bytes());
    append_identity_frame(&mut framed, b"rustc-content", rustc_content_digest.as_bytes());
    append_identity_frame(&mut framed, b"compiler-sysroot", sysroot_identity.as_bytes());
    let capability_identity = format!("sha256:{}", ContentDigest::sha256(&framed));
    let compiler_environment = transparent_native_compiler_process_env_fingerprint(target_root)?;
    let session = NativeCompilerSession::capture_with_root_portability(
        &source_root,
        &rustc_verbose_version,
        &capability_identity,
        &compiler_environment,
        crate::compiler::native_cache::native_cache_execution_contract(),
        NativeSessionAuthority::Exact,
        root_portability,
    )?;
    let inventory = compiler_sysroot_inventory(&rustc_sysroot, host_target)?;
    let sysroot_evidence = capture_exact_sysroot_evidence(&inventory)
        .ok_or_else(|| RailError::message("selected rustc sysroot has no stable local generation evidence"))?;
    let mut memo = TransparentNativeSessionMemo {
        version: TRANSPARENT_SESSION_MEMO_VERSION,
        source_root: source_root
            .to_str()
            .ok_or_else(|| RailError::message("transparent compiler source root is not valid UTF-8"))?
            .to_string(),
        rustc_program: resolved_rustc
            .to_str()
            .ok_or_else(|| RailError::message("selected rustc path is not valid UTF-8"))?
            .to_string(),
        rustc_program_generation,
        rustc_sysroot: rustc_sysroot
            .to_str()
            .ok_or_else(|| RailError::message("selected rustc sysroot is not valid UTF-8"))?
            .to_string(),
        host_target: host_target.to_string(),
        sysroot_evidence,
        compiler_environment_identity: compiler_environment,
        session: session.clone(),
        digest: String::new(),
    };
    memo.digest = transparent_session_memo_digest(&memo)?;
    Ok((session, bytes_hashed, memo))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub(crate) fn capture_transparent_native_session(
    _source_root: &Path,
    _target_root: &Path,
    _rustc_program: &OsStr,
    _cache: &LocalCacheSelection,
    _root_portability: crate::cache::installation::InstalledRootPortability,
) -> RailResult<(NativeCompilerSession, u64, TransparentNativeSessionMemo)> {
    Err(RailError::message(
        "transparent compiler session memoization is unsupported on this platform",
    ))
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn reuse_transparent_native_session(
    memo: &TransparentNativeSessionMemo,
    source_root: &Path,
    target_root: &Path,
    rustc_program: &OsStr,
) -> RailResult<Option<NativeCompilerSession>> {
    let source_root = crate::utils::canonicalize_existing(source_root)?;
    if memo.version != TRANSPARENT_SESSION_MEMO_VERSION
        || memo.digest != transparent_session_memo_digest(memo)?
        || memo.source_root != source_root.to_string_lossy()
        || memo.compiler_environment_identity != transparent_native_compiler_process_env_fingerprint(target_root)?
    {
        return Ok(None);
    }
    let resolved_rustc = crate::executable::resolve_executable_path(rustc_program, &source_root)?;
    if memo.rustc_program != resolved_rustc.to_string_lossy()
        || crate::utils::stable_file_generation(&resolved_rustc).as_ref() != Some(&memo.rustc_program_generation)
    {
        return Ok(None);
    }
    let inventory = compiler_sysroot_inventory(Path::new(&memo.rustc_sysroot), &memo.host_target)?;
    let Some(before) = capture_exact_sysroot_evidence(&inventory) else {
        return Ok(None);
    };
    if before != memo.sysroot_evidence || capture_exact_sysroot_evidence(&inventory).as_ref() != Some(&before) {
        return Ok(None);
    }
    memo.session.validate_for_source_root(&source_root)?;
    Ok(Some(memo.session.clone()))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub(crate) fn reuse_transparent_native_session(
    _memo: &TransparentNativeSessionMemo,
    _source_root: &Path,
    _target_root: &Path,
    _rustc_program: &OsStr,
) -> RailResult<Option<NativeCompilerSession>> {
    Ok(None)
}

impl TransparentNativeSessionMemo {
    pub(crate) fn decode(bytes: &[u8]) -> RailResult<Self> {
        let memo: Self = serde_json::from_slice(bytes)?;
        if serde_json::to_vec(&memo)? != bytes {
            return Err(RailError::message("transparent compiler session memo is not canonical"));
        }
        Ok(memo)
    }

    pub(crate) fn encode(&self) -> RailResult<Vec<u8>> {
        serde_json::to_vec(self).map_err(Into::into)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn transparent_session_memo_digest(memo: &TransparentNativeSessionMemo) -> RailResult<String> {
    let mut unsigned = memo.clone();
    unsigned.digest.clear();
    Ok(format!(
        "sha256:{}",
        ContentDigest::sha256(&serde_json::to_vec(&unsigned)?)
    ))
}

fn transparent_rustc_query(program: &OsStr, argument: &str, current_dir: &Path) -> RailResult<String> {
    let mut command = Command::new(program);
    command
        .arg(argument)
        .current_dir(current_dir)
        .env("RUSTUP_AUTO_INSTALL", "0")
        .env("RUSTUP_NO_UPDATE_CHECK", "1");
    crate::remote_cache::scrub_child_environment(&mut command);
    let output = command.output().map_err(|error| {
        RailError::message(format!(
            "failed to query selected rustc '{}': {error}",
            program.to_string_lossy()
        ))
    })?;
    if !output.status.success() {
        return Err(RailError::message(format!(
            "selected rustc '{}' query failed with status {}",
            program.to_string_lossy(),
            output.status
        )));
    }
    let value = String::from_utf8(output.stdout)
        .map_err(|_| RailError::message("selected rustc query returned non-UTF-8 output"))?
        .replace("\r\n", "\n")
        .trim_end()
        .to_string();
    if value.is_empty() {
        return Err(RailError::message("selected rustc query returned empty output"));
    }
    Ok(value)
}

impl<'a> CompilerDiagnosticsCollector<'a> {
    pub(crate) fn with_identity(
        workspace_root: &'a Path,
        manifests: &'a ManifestAnalyzer,
        targets: Vec<&'a str>,
        identity: &CompilerCacheIdentity,
    ) -> Self {
        Self {
            workspace_root,
            manifests,
            targets,
            identity: identity.clone(),
            artifact_budget: CompilerArtifactBudget::default(),
            acquisition: None,
            #[cfg(test)]
            execution_policy: None,
        }
    }

    pub(crate) fn with_artifact_budget(mut self, budget: CompilerArtifactBudget) -> Self {
        self.artifact_budget = budget;
        self
    }

    pub(crate) fn with_acquisition_manifest(mut self, request: CompilerAcquisitionRequest) -> Self {
        self.acquisition = Some(request);
        self
    }

    #[cfg(test)]
    fn with_execution_policy(mut self, policy: ExecutionPolicy) -> Self {
        self.execution_policy = Some(policy);
        self
    }

    /// Collect diagnostics for selected workspace members.
    pub(crate) fn collect_for_candidates(
        &self,
        candidates: &[CompilerCandidate],
    ) -> RailResult<HashMap<PackageId, MemberEvidence>> {
        Ok(self
            .collect_requirements(candidates, &BTreeSet::new(), &BTreeSet::new(), None, None)?
            .diagnostics)
    }

    /// Collect diagnostics and typed items through the same normalized Cargo views.
    pub(crate) fn collect_with_typed_items(
        &self,
        snapshot: &WorkspaceSnapshot,
        candidates: &[CompilerCandidate],
        typed_packages: &BTreeSet<String>,
        doctest_packages: &BTreeSet<String>,
    ) -> RailResult<CompilerAnalysisEvidence> {
        self.collect_requirements(candidates, typed_packages, doctest_packages, None, Some(snapshot))
    }

    /// Collect typed items through an exact, explicitly configured feature matrix.
    pub(crate) fn collect_with_typed_items_and_features(
        &self,
        snapshot: &WorkspaceSnapshot,
        candidates: &[CompilerCandidate],
        typed_packages: &BTreeSet<String>,
        doctest_packages: &BTreeSet<String>,
        features: &[FeatureSelection],
    ) -> RailResult<CompilerAnalysisEvidence> {
        self.collect_requirements(
            candidates,
            typed_packages,
            doctest_packages,
            Some(features),
            Some(snapshot),
        )
    }

    fn collect_requirements(
        &self,
        candidates: &[CompilerCandidate],
        typed_packages: &BTreeSet<String>,
        doctest_packages: &BTreeSet<String>,
        features: Option<&[FeatureSelection]>,
        snapshot: Option<&WorkspaceSnapshot>,
    ) -> RailResult<CompilerAnalysisEvidence> {
        let schedule = AnalysisSchedule::for_combined_with_features(
            &self.manifests.members,
            &self.targets,
            candidates,
            typed_packages,
            doctest_packages,
            features,
        )?;
        let plan_started = crate::instrumentation::compiler_acquisition_timer();
        let plan = CompilerAcquisitionPlan::from_schedule(&schedule, candidates, &self.targets)?;
        crate::instrumentation::record_compiler_acquisition_plan(
            plan_started,
            plan.identity().as_str(),
            plan.package_count(),
            plan.target_count(),
            plan.feature_count(),
            plan.candidate_count(),
            plan.view_count(),
        );
        let mut metrics = CompilerAnalysisMetrics {
            analysis_views: plan.view_count(),
            ..CompilerAnalysisMetrics::default()
        };
        let members = plan
            .views()
            .map(CompilerAcquisitionView::package)
            .collect::<HashSet<_>>();
        if members.is_empty() {
            return Ok(CompilerAnalysisEvidence {
                diagnostics: HashMap::new(),
                compiler_facts: Vec::new(),
                metrics,
            });
        }
        let derived_policy = ExecutionPolicy::derive(
            plan.view_count(),
            self.identity.analysis_cache.is_some(),
            self.identity.explicit_build_jobs,
        );
        #[cfg(test)]
        let execution_policy = self.execution_policy.unwrap_or(derived_policy);
        #[cfg(not(test))]
        let execution_policy = derived_policy;
        crate::instrumentation::record_compiler_acquisition_execution_policy(
            execution_policy.process_slots(),
            execution_policy.work_permits(),
        );
        let acquisition_request = self.acquisition.as_ref();
        let typed_snapshot = if typed_packages.is_empty() {
            None
        } else {
            Some(snapshot.ok_or_else(|| {
                RailError::message("typed compiler fact collection requires its captured workspace snapshot")
            })?)
        };
        let producer_authority = typed_snapshot
            .map(|snapshot| {
                CompilerFactDriverAuthority::producer_authority(snapshot, &self.identity.toolchain_fingerprint)
            })
            .transpose()?;
        let mut sandbox_pool = SandboxPool::prepare(self.workspace_root, execution_policy.sandbox_count())?;
        progress!(
            "  Compiler sandbox pool: {} (up to {} Cargo processes; {} work permits; {} planned views; {} bytes soft; {} bytes hard)",
            sandbox_pool.root().display(),
            execution_policy.process_slots(),
            execution_policy.work_permits(),
            plan.view_count(),
            self.artifact_budget.soft_limit_bytes,
            self.artifact_budget.hard_limit_bytes,
        );
        let mut prepared_driver = None;
        let mut prepared_doctest_sysroot = None;

        let remote = self
            .identity
            .analysis_cache
            .as_ref()
            .and_then(|cache| cache.remote.clone());
        let mut store = CompilerDiagnosticsStore::load_with_remote(remote.clone());
        let fact_store = CompilerFactStore::load_with_remote(remote);
        let package_to_member = build_package_member_index(&self.manifests.members);
        let member_ids: HashMap<&str, &PackageId> = self
            .manifests
            .members
            .iter()
            .map(|member| (member.package_name.as_str(), &member.package_id))
            .collect();
        let candidate_targets = build_candidate_target_index(candidates);

        let mut result: HashMap<PackageId, MemberEvidence> = HashMap::with_capacity(members.len());
        let mut compiler_facts = Vec::new();
        let mut cache_by_member: HashMap<String, EvidenceCacheSummary> = HashMap::with_capacity(members.len());
        let mut stale_by_view = vec![Vec::<&str>::new(); plan.view_count()];
        let mut typed_view = vec![false; plan.view_count()];
        let mut retained_observations = HashMap::<String, CompilationObservationManifest>::new();
        let mut surviving_unused: HashMap<String, BTreeSet<CandidateId>> = candidate_targets
            .iter()
            .map(|(member, candidates)| (member.clone(), candidates.keys().cloned().collect()))
            .collect();

        for view in plan.views() {
            let target = view.platform();
            let features = view.features();
            let collects_diagnostics = view.requires(crate::compiler::scheduler::CompilerFactFamily::StableDiagnostics);
            let collects_typed = view.requires(crate::compiler::scheduler::CompilerFactFamily::TypedRustItems);
            if collects_typed {
                typed_view[view.index().offset()] = true;
            }
            let member = view.package();
            if !collects_diagnostics {
                continue;
            }
            let package_id = member_ids
                .get(member)
                .ok_or_else(|| RailError::message(format!("missing package identity for member '{member}'")))?;
            let manifest = self
                .manifests
                .members
                .iter()
                .find(|manifest| manifest.package_name == member)
                .ok_or_else(|| RailError::message(format!("missing manifest entry for member '{member}'")))?;
            let key = self.key_for(manifest, target, features)?;
            let mut cache_hit = false;
            let cache_started = crate::instrumentation::compiler_acquisition_timer();
            let observation_miss = if self.identity.cache_bypass_reason.is_none() {
                store.get(&key).and_then(|entry| {
                    let miss =
                        compiler_observation_miss_reason(&entry.observations, self.workspace_root).map(str::to_string);
                    if miss.is_none() {
                        cache_hit = true;
                        metrics.diagnostic_cache_hits += 1;
                        cache_by_member.entry(member.to_string()).or_default().hits += 1;
                        update_candidate_survivors(
                            &mut surviving_unused,
                            &candidate_targets,
                            member,
                            target,
                            &entry.evidence,
                        );
                        record_target_evidence(&mut result, package_id, &entry.evidence);
                    }
                    miss
                })
            } else {
                None
            };
            crate::instrumentation::record_compiler_acquisition_cache_lookup(cache_started, cache_hit);
            if cache_hit {
                continue;
            }

            let reason = self
                .identity
                .cache_bypass_reason
                .map(|reason| reason.as_str().to_string())
                .or(observation_miss)
                .unwrap_or_else(|| store.miss_reason(&key).to_string());
            let summary = cache_by_member.entry(member.to_string()).or_default();
            metrics.diagnostic_cache_misses += 1;
            summary.misses += 1;
            *summary.miss_reasons.entry(reason).or_default() += 1;

            stale_by_view[view.index().offset()].push(member);
        }

        let stale_configurations = plan
            .execution_order()
            .filter(|view| typed_view[view.index().offset()] || !stale_by_view[view.index().offset()].is_empty())
            .collect::<Vec<_>>();

        progress!(
            "  Compiler evidence plan: {} views; up to {} Cargo acquisitions; {} diagnostic cache hits; {} diagnostic cache misses",
            metrics.analysis_views,
            stale_configurations.len(),
            metrics.diagnostic_cache_hits,
            metrics.diagnostic_cache_misses
        );

        let mut skipped_member_targets = 0usize;
        let acquisition_views = stale_configurations.len();
        let acquisition_progress_interval = acquisition_views.div_ceil(100).max(1);
        let mut prepared_by_view = (0..plan.view_count())
            .map(|_| None)
            .collect::<Vec<Option<PreparedAcquisitionView<'_>>>>();
        let mut journal_fact_keys = (0..plan.view_count())
            .map(|_| None)
            .collect::<Vec<Option<CompilerFactCacheKey>>>();
        let mut runtime_specs = Vec::with_capacity(acquisition_views);
        for (ordinal, view) in stale_configurations.into_iter().enumerate() {
            let typed_members = if view.requires(crate::compiler::scheduler::CompilerFactFamily::TypedRustItems)
                && typed_packages.contains(view.package())
            {
                BTreeSet::from([view.package().to_string()])
            } else {
                BTreeSet::new()
            };
            let fact_cache_key = if typed_members.is_empty() || self.identity.cache_bypass_reason.is_some() {
                None
            } else {
                Some(
                    self.fact_cache_key(
                        view,
                        typed_members
                            .first()
                            .ok_or_else(|| RailError::message("typed compiler package disappeared"))?,
                        producer_authority
                            .as_ref()
                            .ok_or_else(|| RailError::message("typed compiler fact producer authority disappeared"))?,
                    )?,
                )
            };
            let cached_facts = if let Some(key) = fact_cache_key.as_ref() {
                let cache_started = crate::instrumentation::compiler_acquisition_timer();
                let cached = fact_store.get(key).ok().flatten();
                crate::instrumentation::record_compiler_acquisition_cache_lookup(cache_started, cached.is_some());
                cached
            } else {
                None
            };
            journal_fact_keys[view.index().offset()] = fact_cache_key.clone();
            if !typed_members.is_empty() {
                if cached_facts.is_some() {
                    metrics.fact_cache_hits += 1;
                } else {
                    metrics.fact_cache_misses += 1;
                    if let Some(reason) = self.identity.cache_bypass_reason {
                        *metrics
                            .fact_cache_bypass_reasons
                            .entry(reason.as_str().to_string())
                            .or_default() += 1;
                    }
                }
            }
            let collect_typed = !typed_members.is_empty() && cached_facts.is_none();
            if let Some(cached) = cached_facts {
                compiler_facts.extend(cached);
            }
            let has_diagnostics = !stale_by_view[view.index().offset()].is_empty();
            if !collect_typed && !has_diagnostics {
                continue;
            }
            let runtime_candidates = has_diagnostics.then(|| view.candidates()).into_iter().flatten();
            runtime_specs.push(RuntimeViewSpec::new(
                view.index(),
                ordinal,
                collect_typed,
                runtime_candidates,
            ));
            prepared_by_view[view.index().offset()] = Some(PreparedAcquisitionView {
                ordinal,
                view,
                typed_members,
                fact_cache_key,
                evidence_identity: None,
                collect_typed,
            });
        }

        let requires_typed = prepared_by_view.iter().flatten().any(|prepared| prepared.collect_typed);
        if requires_typed {
            prepared_driver = Some(
                PreparedCompilerFactDriver::prepare(
                    typed_snapshot.ok_or_else(|| RailError::message("typed compiler fact snapshot disappeared"))?,
                    producer_authority
                        .as_ref()
                        .ok_or_else(|| RailError::message("typed compiler fact producer authority disappeared"))?,
                )
                .with_context(|| "preparing authenticated compiler fact driver".to_string())?,
            );
        }
        let requires_doctest = prepared_by_view
            .iter()
            .flatten()
            .any(|prepared| prepared.collect_typed && prepared.view.compiles_doctests());
        if requires_doctest {
            let snapshot =
                typed_snapshot.ok_or_else(|| RailError::message("typed compiler fact snapshot disappeared"))?;
            let wrapper = compiler_observation_wrapper().map_err(|error| {
                RailError::message(format!("failed to locate the typed-doctest compiler wrapper: {error}"))
            })?;
            let rustdoc = crate::executable::resolve_executable_path(
                snapshot.toolchain().rustdoc_program(),
                snapshot.cargo_current_dir(),
            )?;
            let wrapper_digest = self.identity.diagnostic_wrapper_executable()?.content_digest();
            let rustdoc_digest = snapshot
                .executable_identities()?
                .rustdoc()
                .ok_or_else(|| RailError::message("selected toolchain has no captured rustdoc authority"))?
                .content_digest();
            prepared_doctest_sysroot = Some(
                prepared_driver
                    .as_ref()
                    .ok_or_else(|| RailError::message("typed compiler fact driver disappeared before doctest staging"))?
                    .stage_doctest_sysroot(snapshot, &wrapper, wrapper_digest, &rustdoc, rustdoc_digest)
                    .map_err(|error| {
                        RailError::message(format!(
                            "failed to stage the private typed-doctest compiler sysroot: {error}"
                        ))
                    })?,
            );
        }

        let acquisition_broker = if runtime_specs.is_empty() {
            None
        } else {
            self.identity
                .analysis_cache
                .as_ref()
                .map(|cache| {
                    crate::compiler::acquisition::broker::AcquisitionBroker::start(
                        execution_policy.work_permits(),
                        cache.cas.clone(),
                    )
                })
                .transpose()?
        };
        let mut runtime = RuntimeState::new(execution_policy, plan.view_count(), runtime_specs)?;
        let mut acquisition_journal = acquisition_request
            .map(|request| {
                CompilerAcquisitionJournal::begin(
                    self.workspace_root,
                    request,
                    &plan,
                    self.identity.acquisition_compiler_set_identity()?,
                    self.artifact_budget.soft_limit_bytes,
                    self.artifact_budget.hard_limit_bytes,
                    execution_policy.process_slots(),
                    execution_policy.journal_batch(),
                )
            })
            .transpose()?;
        if let Some(journal) = acquisition_journal.as_mut() {
            let revalidation = (|| -> RailResult<()> {
                for view in plan.views() {
                    let offset = view.index().offset();
                    let evidence =
                        self.acquisition_evidence_identity(journal, view, journal_fact_keys[offset].as_ref())?;
                    if let Some(prepared) = prepared_by_view[offset].as_mut() {
                        prepared.evidence_identity = Some(evidence);
                        journal.revalidate(view.index(), None)?;
                    } else {
                        journal.revalidate(view.index(), Some(evidence))?;
                    }
                }
                journal.seal_revalidation()
            })();
            if let Err(error) = revalidation {
                return match journal.fail(None, FailureClass::Journal) {
                    Ok(()) => Err(RailError::with_help(error.to_string(), journal.resume_help())),
                    Err(journal_error) => Err(RailError::message(format!(
                        "{error}; failed to terminally record journal revalidation failure: {journal_error}"
                    ))),
                };
            }
        }
        let cancellation = AtomicBool::new(false);
        let worker_count = execution_policy
            .process_slots()
            .min(prepared_by_view.iter().flatten().count());
        let mut failures = Vec::<AcquisitionFailure>::new();
        if worker_count > 0 {
            let worker_context = AcquisitionWorkerContext {
                workspace_root: self.workspace_root,
                identity: &self.identity,
                typed_snapshot,
                driver: prepared_driver.as_ref(),
                doctest_sysroot: prepared_doctest_sysroot.as_ref(),
                artifact_budget: self.artifact_budget,
                package_to_member: &package_to_member,
                cancellation: &cancellation,
                broker: acquisition_broker.as_ref(),
            };
            let execution = std::thread::scope(|scope| -> RailResult<()> {
                let (outcome_tx, outcome_rx) = mpsc::sync_channel(worker_count.saturating_mul(2).max(1));
                let mut job_senders = Vec::with_capacity(worker_count);
                let mut workers = Vec::with_capacity(worker_count);
                for worker in 0..worker_count {
                    let (job_tx, job_rx) = mpsc::sync_channel(1);
                    let outcome_tx = outcome_tx.clone();
                    let worker_context = &worker_context;
                    workers.push(
                        std::thread::Builder::new()
                            .name(format!("cargo-rail-acquisition-worker-{worker}"))
                            .spawn_scoped(scope, move || {
                                while let Ok(job) = job_rx.recv() {
                                    let outcome = execute_acquisition_job(worker, worker_context, job);
                                    if outcome_tx.send(outcome).is_err() {
                                        break;
                                    }
                                }
                            })?,
                    );
                    job_senders.push(job_tx);
                }
                drop(outcome_tx);
                let coordinator = (|| -> RailResult<()> {
                    let mut idle_workers = (0..worker_count).collect::<VecDeque<_>>();
                    let mut diagnostic_dispatch_order = VecDeque::new();
                    let mut diagnostic_completions = HashMap::<ViewIx, AcquisitionCompletion<'_>>::new();
                    loop {
                        let mut made_progress = false;
                        if let Some(journal) = acquisition_journal.as_mut()
                            && let Err(error) = journal.flush_if_due()
                        {
                            cancellation.store(true, std::sync::atomic::Ordering::Release);
                            failures.push(AcquisitionFailure::global(FailureClass::Journal, error));
                            break;
                        }
                        if !runtime.cancelled() {
                            let skipped = runtime.refresh(|index| {
                                let view = plan.view(index).expect("runtime view belongs to plan");
                                let features = view.features();
                                stale_by_view[index.offset()].iter().copied().any(|member| {
                                    has_applicable_survivor(
                                        &surviving_unused,
                                        &candidate_targets,
                                        member,
                                        view.platform(),
                                        &features,
                                    )
                                })
                            })?;
                            made_progress |= !skipped.is_empty();
                            for index in skipped {
                                skipped_member_targets += stale_by_view[index.offset()].len();
                                prepared_by_view[index.offset()].take();
                            }
                        }

                        while !runtime.cancelled()
                            && diagnostic_dispatch_order
                                .front()
                                .is_some_and(|index| diagnostic_completions.contains_key(index))
                        {
                            let index = diagnostic_dispatch_order.pop_front().expect("checked diagnostic order");
                            let completion = diagnostic_completions
                                .remove(&index)
                                .expect("checked diagnostic completion");
                            match integrate_completed_acquisition(
                                self,
                                completion,
                                acquisition_journal.is_some(),
                                candidates,
                                &package_to_member,
                                &candidate_targets,
                                &fact_store,
                                &mut metrics,
                                &mut compiler_facts,
                                &mut retained_observations,
                                &mut surviving_unused,
                                &mut result,
                                &mut store,
                            ) {
                                Ok((_, evidence_identity, durable)) => {
                                    if let (Some(journal), Some(evidence)) =
                                        (acquisition_journal.as_mut(), evidence_identity)
                                        && let Err(error) = journal.complete(index, evidence, durable)
                                    {
                                        cancellation.store(true, std::sync::atomic::Ordering::Release);
                                        runtime.fail_integration(index)?;
                                        failures.push(AcquisitionFailure::view(
                                            index.offset(),
                                            index,
                                            Vec::new(),
                                            FailureClass::Journal,
                                            error,
                                        ));
                                        diagnostic_completions.clear();
                                        diagnostic_dispatch_order.clear();
                                        break;
                                    }
                                    runtime.complete(index)?;
                                    made_progress = true;
                                }
                                Err(error) => {
                                    cancellation.store(true, std::sync::atomic::Ordering::Release);
                                    runtime.fail_integration(index)?;
                                    failures.push(AcquisitionFailure::view(
                                        index.offset(),
                                        index,
                                        Vec::new(),
                                        FailureClass::Integration,
                                        error,
                                    ));
                                    diagnostic_completions.clear();
                                    diagnostic_dispatch_order.clear();
                                    break;
                                }
                            }
                        }

                        let mut staged_jobs = VecDeque::new();
                        while !runtime.cancelled() && !idle_workers.is_empty() {
                            let Some(index) = runtime.start_next()? else {
                                break;
                            };
                            let worker = idle_workers.pop_front().expect("checked idle worker");
                            let prepared = prepared_by_view[index.offset()]
                                .take()
                                .ok_or_else(|| RailError::message("compiler acquisition ready view lost its work"))?;
                            let view = prepared.view;
                            let features = view.features();
                            let diagnostic_members = stale_by_view[index.offset()]
                                .iter()
                                .copied()
                                .filter(|member| {
                                    has_applicable_survivor(
                                        &surviving_unused,
                                        &candidate_targets,
                                        member,
                                        view.platform(),
                                        &features,
                                    )
                                })
                                .map(str::to_string)
                                .collect::<Vec<_>>();
                            skipped_member_targets += stale_by_view[index.offset()]
                                .len()
                                .saturating_sub(diagnostic_members.len());
                            let active_members = diagnostic_members.len() + usize::from(prepared.collect_typed);
                            if active_members == 0 {
                                cancellation.store(true, std::sync::atomic::Ordering::Release);
                                runtime.fail(index)?;
                                failures.push(AcquisitionFailure::view(
                                    prepared.ordinal,
                                    index,
                                    Vec::new(),
                                    FailureClass::Coordinator,
                                    RailError::message(
                                        "compiler acquisition admitted a view with no unresolved requirement",
                                    ),
                                ));
                                idle_workers.push_front(worker);
                                break;
                            }
                            let report_progress = prepared.ordinal == 0
                                || prepared.ordinal + 1 == acquisition_views
                                || (prepared.ordinal + 1).is_multiple_of(acquisition_progress_interval);
                            if report_progress {
                                progress!(
                                    "  Collecting compiler evidence view {}/{} for target {} ({} package{})...",
                                    prepared.ordinal + 1,
                                    metrics.analysis_views,
                                    format_args!("{} / {}", view.platform(), features.label()),
                                    active_members,
                                    if active_members == 1 { "" } else { "s" }
                                );
                            }
                            if let Err(error) = surface_acquisition_test_fault(prepared.ordinal) {
                                cancellation.store(true, std::sync::atomic::Ordering::Release);
                                runtime.fail(index)?;
                                failures.push(AcquisitionFailure::view(
                                    prepared.ordinal,
                                    index,
                                    Vec::new(),
                                    FailureClass::Coordinator,
                                    error,
                                ));
                                idle_workers.push_front(worker);
                                break;
                            }
                            let sandbox = match sandbox_pool.lease(SandboxCompatibility::new(
                                self.identity.toolchain_fingerprint.clone(),
                                view.platform(),
                                view.command_class(),
                                format!(
                                    "{}:{}",
                                    self.identity.compiler_env_fingerprint, self.identity.cargo_config_fingerprint
                                ),
                                if prepared.collect_typed {
                                    if view.compiles_doctests() {
                                        "typed-doctest"
                                    } else {
                                        "typed"
                                    }
                                } else {
                                    "diagnostic"
                                },
                            )) {
                                Ok(sandbox) => sandbox,
                                Err(error) => {
                                    cancellation.store(true, std::sync::atomic::Ordering::Release);
                                    runtime.fail(index)?;
                                    failures.push(AcquisitionFailure::view(
                                        prepared.ordinal,
                                        index,
                                        Vec::new(),
                                        FailureClass::Sandbox,
                                        error,
                                    ));
                                    idle_workers.push_front(worker);
                                    break;
                                }
                            };
                            let job = AcquisitionJob {
                                prepared,
                                diagnostic_members,
                                sandbox,
                                started: report_progress.then(Instant::now),
                            };
                            staged_jobs.push_back((worker, index, job));
                        }

                        if runtime.cancelled() {
                            while let Some((_, index, job)) = staged_jobs.pop_front() {
                                sandbox_pool.reclaim(job.sandbox.poison())?;
                                runtime.discard_running(index)?;
                            }
                        } else if !staged_jobs.is_empty() {
                            let running = staged_jobs.iter().map(|(_, index, _)| *index).collect::<Vec<_>>();
                            if let Some(journal) = acquisition_journal.as_mut()
                                && let Err(error) = journal.running_batch(&running)
                            {
                                cancellation.store(true, std::sync::atomic::Ordering::Release);
                                let (_, primary_index, primary_job) =
                                    staged_jobs.pop_front().expect("non-empty running batch");
                                let primary_ordinal = primary_job.prepared.ordinal;
                                sandbox_pool.reclaim(primary_job.sandbox.poison())?;
                                runtime.fail(primary_index)?;
                                while let Some((_, index, job)) = staged_jobs.pop_front() {
                                    sandbox_pool.reclaim(job.sandbox.poison())?;
                                    runtime.discard_running(index)?;
                                }
                                failures.push(AcquisitionFailure::view(
                                    primary_ordinal,
                                    primary_index,
                                    Vec::new(),
                                    FailureClass::Journal,
                                    error,
                                ));
                            }
                            while !runtime.cancelled()
                                && let Some((worker, index, job)) = staged_jobs.pop_front()
                            {
                                let has_diagnostics = !job.diagnostic_members.is_empty();
                                if let Err(send_error) = job_senders[worker].send(job) {
                                    let job = send_error.0;
                                    let ordinal = job.prepared.ordinal;
                                    cancellation.store(true, std::sync::atomic::Ordering::Release);
                                    sandbox_pool.reclaim(job.sandbox.poison())?;
                                    runtime.fail(index)?;
                                    while let Some((_, staged_index, staged_job)) = staged_jobs.pop_front() {
                                        sandbox_pool.reclaim(staged_job.sandbox.poison())?;
                                        runtime.discard_running(staged_index)?;
                                    }
                                    failures.push(AcquisitionFailure::view(
                                        ordinal,
                                        index,
                                        Vec::new(),
                                        FailureClass::Coordinator,
                                        RailError::message(
                                            "compiler acquisition worker stopped before accepting its view",
                                        ),
                                    ));
                                    break;
                                }
                                if has_diagnostics {
                                    diagnostic_dispatch_order.push_back(index);
                                }
                                made_progress = true;
                            }
                        }

                        if runtime.all_terminal() {
                            break;
                        }
                        if runtime.running() == 0 {
                            if made_progress {
                                continue;
                            }
                            return Err(RailError::message(
                                "compiler acquisition runtime stalled with nonterminal views",
                            ));
                        }
                        let outcome = if let Some(timeout) = acquisition_journal
                            .as_ref()
                            .and_then(CompilerAcquisitionJournal::completion_flush_timeout)
                        {
                            match outcome_rx.recv_timeout(timeout) {
                                Ok(outcome) => outcome,
                                Err(mpsc::RecvTimeoutError::Timeout) => {
                                    if let Some(journal) = acquisition_journal.as_mut()
                                        && let Err(error) = journal.flush_if_due()
                                    {
                                        cancellation.store(true, std::sync::atomic::Ordering::Release);
                                        failures.push(AcquisitionFailure::global(FailureClass::Journal, error));
                                        break;
                                    }
                                    continue;
                                }
                                Err(mpsc::RecvTimeoutError::Disconnected) => {
                                    return Err(RailError::message(
                                        "compiler acquisition workers stopped with live views",
                                    ));
                                }
                            }
                        } else {
                            outcome_rx.recv().map_err(|_| {
                                RailError::message("compiler acquisition workers stopped with live views")
                            })?
                        };
                        idle_workers.push_back(outcome.worker);
                        sandbox_pool.reclaim(outcome.sandbox)?;
                        let completion = outcome.completion;
                        let index = completion.prepared.view.index();
                        if runtime.cancelled() {
                            runtime.discard_running(index)?;
                            if let Err(error) = completion.result {
                                failures.push(AcquisitionFailure::view(
                                    completion.prepared.ordinal,
                                    index,
                                    completion.failed_cargo_targets,
                                    FailureClass::Cancelled,
                                    error,
                                ));
                            }
                            continue;
                        }
                        if completion.result.is_err() {
                            cancellation.store(true, std::sync::atomic::Ordering::Release);
                            runtime.fail(index)?;
                            let AcquisitionCompletion {
                                prepared,
                                failed_cargo_targets,
                                result,
                                ..
                            } = completion;
                            let Err(error) = result else {
                                return Err(RailError::message(
                                    "compiler acquisition failure lost its failed outcome",
                                ));
                            };
                            let error = error.context(format!(
                                "acquiring compiler evidence for target '{} / {}'",
                                prepared.view.platform(),
                                prepared.view.features().label()
                            ));
                            failures.push(AcquisitionFailure::view(
                                prepared.ordinal,
                                index,
                                failed_cargo_targets,
                                FailureClass::Worker,
                                error,
                            ));
                            diagnostic_completions.clear();
                            diagnostic_dispatch_order.clear();
                        } else {
                            runtime.executed(index)?;
                            if !completion.diagnostic_members.is_empty() {
                                diagnostic_completions.insert(index, completion);
                                continue;
                            }
                            match integrate_completed_acquisition(
                                self,
                                completion,
                                acquisition_journal.is_some(),
                                candidates,
                                &package_to_member,
                                &candidate_targets,
                                &fact_store,
                                &mut metrics,
                                &mut compiler_facts,
                                &mut retained_observations,
                                &mut surviving_unused,
                                &mut result,
                                &mut store,
                            ) {
                                Ok((_, evidence_identity, durable)) => {
                                    if let (Some(journal), Some(evidence)) =
                                        (acquisition_journal.as_mut(), evidence_identity)
                                        && let Err(error) = journal.complete(index, evidence, durable)
                                    {
                                        cancellation.store(true, std::sync::atomic::Ordering::Release);
                                        runtime.fail_integration(index)?;
                                        failures.push(AcquisitionFailure::view(
                                            index.offset(),
                                            index,
                                            Vec::new(),
                                            FailureClass::Journal,
                                            error,
                                        ));
                                        diagnostic_completions.clear();
                                        diagnostic_dispatch_order.clear();
                                        continue;
                                    }
                                    runtime.complete(index)?;
                                }
                                Err(error) => {
                                    cancellation.store(true, std::sync::atomic::Ordering::Release);
                                    runtime.fail_integration(index)?;
                                    failures.push(AcquisitionFailure::view(
                                        index.offset(),
                                        index,
                                        Vec::new(),
                                        FailureClass::Integration,
                                        error,
                                    ));
                                    diagnostic_completions.clear();
                                    diagnostic_dispatch_order.clear();
                                }
                            }
                        }
                    }
                    Ok(())
                })();
                if coordinator.is_err() {
                    cancellation.store(true, std::sync::atomic::Ordering::Release);
                }

                drop(job_senders);
                let mut join_failure = None;
                for worker in workers {
                    if worker.join().is_err() && join_failure.is_none() {
                        join_failure = Some(RailError::message("compiler acquisition worker panicked"));
                    }
                }
                coordinator.and_then(|()| join_failure.map_or(Ok(()), Err))
            });
            if let Err(error) = execution {
                cancellation.store(true, std::sync::atomic::Ordering::Release);
                failures.push(AcquisitionFailure::coordinator(error));
            }
        }
        if let Some(broker) = acquisition_broker
            && let Err(error) = broker.close()
        {
            failures.push(AcquisitionFailure::global(FailureClass::Broker, error));
        }
        if let Err(error) = sandbox_pool.close() {
            failures.push(AcquisitionFailure::global(FailureClass::Sandbox, error));
        }
        if !failures.is_empty() {
            failures.sort_by_key(|failure| (failure.class == FailureClass::Cancelled, failure.ordinal, failure.view));
            let primary = failures.remove(0);
            let mut error = primary.error.to_string();
            for secondary in failures {
                error.push_str("; cleanup or concurrent failure: ");
                error.push_str(&secondary.error.to_string());
            }
            if let Some(journal) = acquisition_journal.as_mut() {
                let durable_primary = primary.view.map(|view| (view, primary.cargo_targets, primary.class));
                if let Err(journal_error) = journal.fail(durable_primary, primary.class) {
                    return Err(RailError::message(format!(
                        "{error}; failed to finalize Surface acquisition manifest '{}': {journal_error}",
                        journal.path().display()
                    )));
                }
                return Err(RailError::with_help(error, journal.resume_help()));
            }
            return Err(RailError::message(error));
        }

        let cache_started = crate::instrumentation::compiler_acquisition_timer();
        let flushed = store.flush();
        crate::instrumentation::record_compiler_acquisition_cache_write(cache_started);
        if let Err(error) = flushed {
            if let Some(journal) = acquisition_journal.as_mut() {
                return match journal.fail(None, FailureClass::Integration) {
                    Ok(()) => Err(RailError::with_help(error.to_string(), journal.resume_help())),
                    Err(journal_error) => Err(RailError::message(format!(
                        "{error}; failed to terminally record compiler evidence publication failure: {journal_error}"
                    ))),
                };
            }
            return Err(error);
        }
        if let Some(journal) = acquisition_journal.as_mut()
            && let Err(error) = journal.finish()
        {
            return Err(RailError::with_help(
                format!(
                    "failed to finalize Surface acquisition manifest '{}': {error}",
                    journal.path().display()
                ),
                journal.resume_help(),
            ));
        }
        if skipped_member_targets > 0 {
            progress!(
                "  Skipped {} target-package check{} after dependencies were proven used",
                skipped_member_targets,
                if skipped_member_targets == 1 { "" } else { "s" }
            );
        }
        for member in &self.manifests.members {
            if let Some(evidence) = result.get_mut(&member.package_id) {
                evidence.cache = cache_by_member.remove(&member.package_name).unwrap_or_default();
            }
        }

        compiler_facts.sort_by(|left, right| left.identity().cmp(right.identity()));
        compiler_facts.dedup_by(|left, right| left.identity() == right.identity());
        metrics.retained_fact_object_bytes = compiler_facts.iter().try_fold(0_u64, |total, fact| {
            total
                .checked_add(fact.bytes())
                .ok_or_else(|| RailError::message("compiler fact object byte count overflow"))
        })?;
        Ok(CompilerAnalysisEvidence {
            diagnostics: result,
            compiler_facts,
            metrics,
        })
    }

    fn key_for(
        &self,
        member: &crate::cargo::manifest_analyzer::ParsedManifest,
        target: &str,
        features: FeatureSelection,
    ) -> RailResult<CompilerDiagKey> {
        let identity = &self.identity;
        Ok(CompilerDiagKey {
            package_id: member.package_id.clone(),
            package_name: member.package_name.clone(),
            target: PlatformTarget::from(target),
            features,
            rustc_version: identity.rustc_version.clone(),
            cargo_version: identity.cargo_version.clone(),
            host_triple: identity.host_triple.clone(),
            toolchain_fingerprint: identity.toolchain_fingerprint.clone(),
            target_fingerprint: identity
                .target_fingerprints
                .get(target)
                .cloned()
                .ok_or_else(|| RailError::message(format!("missing compiler target identity for '{target}'")))?,
            lock_fingerprint: identity.lock_fingerprint.clone(),
            manifest_fingerprint: identity
                .manifest_fingerprints
                .get(&member.package_id)
                .cloned()
                .ok_or_else(|| {
                    RailError::message(format!("missing manifest identity for member '{}'", member.package_id))
                })?,
            source_fingerprint: identity
                .source_fingerprints
                .get(&member.package_id)
                .cloned()
                .ok_or_else(|| {
                    RailError::message(format!("missing source identity for member '{}'", member.package_id))
                })?,
            compiler_env_fingerprint: identity.compiler_env_fingerprint.clone(),
            cargo_config_fingerprint: identity.cargo_config_fingerprint.clone(),
        })
    }

    fn fact_cache_key(
        &self,
        view: CompilerAcquisitionView<'_>,
        typed_package: &str,
        producer_authority: &crate::compiler::facts::CompilerFactProducerAuthority,
    ) -> RailResult<CompilerFactCacheKey> {
        let manifest = self
            .manifests
            .members
            .iter()
            .find(|manifest| manifest.package_name == typed_package)
            .ok_or_else(|| RailError::message(format!("missing manifest entry for member '{typed_package}'")))?;
        let packages = vec![self.key_for(manifest, view.platform(), view.features())?];
        CompilerFactCacheKey::new(
            view.fact_cache_identity(typed_package)?,
            packages,
            BTreeSet::from([typed_package.to_string()]),
            producer_authority.clone(),
            required_compiler_fact_coverage(),
        )
    }

    fn acquisition_evidence_identity(
        &self,
        journal: &CompilerAcquisitionJournal,
        view: CompilerAcquisitionView<'_>,
        fact_cache_key: Option<&CompilerFactCacheKey>,
    ) -> RailResult<EvidenceIdentity> {
        let diagnostic_keys = if view.requires(crate::compiler::scheduler::CompilerFactFamily::StableDiagnostics) {
            let manifest = self
                .manifests
                .members
                .iter()
                .find(|manifest| manifest.package_name == view.package())
                .ok_or_else(|| {
                    RailError::message(format!(
                        "missing manifest entry for compiler acquisition member '{}'",
                        view.package()
                    ))
                })?;
            vec![self.key_for(manifest, view.platform(), view.features())?]
        } else {
            Vec::new()
        };
        journal.evidence_identity(view.index(), &diagnostic_keys, fact_cache_key)
    }
}

/// Confirm which borrowed dependency features are named by a member's
/// standalone compiler failure.
pub fn standalone_missing_features(
    workspace_root: &Path,
    member: &str,
    candidates: &[(String, Vec<String>)],
) -> RailResult<BTreeMap<(String, String), BTreeSet<String>>> {
    let output = Command::new("cargo")
        .current_dir(workspace_root)
        .args([
            "check",
            "--locked",
            "--package",
            member,
            "--all-targets",
            "--message-format=json",
        ])
        .output()
        .with_context(|| format!("checking standalone feature requirements for member '{member}'"))?;
    if output.status.success() {
        return Ok(BTreeMap::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut missing: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for line in stdout.lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if event["reason"] != "compiler-message" || event["message"]["level"] != "error" {
            continue;
        }
        let diagnostic = event["message"].to_string();
        let source_paths: BTreeSet<String> = event["message"]["spans"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|span| span["is_primary"] == true)
            .filter_map(|span| span["file_name"].as_str())
            .map(|path| {
                let path = Path::new(path);
                let relative = path.strip_prefix(workspace_root).unwrap_or(path);
                crate::utils::path_to_git_format(relative)
            })
            .collect();
        for (dependency, features) in candidates {
            let crate_name = dependency.replace('-', "_");
            if !diagnostic.contains(dependency) && !diagnostic.contains(&crate_name) {
                continue;
            }
            for feature in features {
                if diagnostic.contains(feature) {
                    missing
                        .entry((dependency.clone(), feature.clone()))
                        .or_default()
                        .extend(source_paths.iter().cloned());
                }
            }
        }
    }
    Ok(missing)
}

/// Verify that one member compiles without relying on other workspace members
/// after a causal feature repair is applied.
pub fn verify_standalone_member(workspace_root: &Path, member: &str) -> RailResult<()> {
    let output = Command::new("cargo")
        .current_dir(workspace_root)
        .args(["check", "--locked", "--package", member, "--all-targets"])
        .output()
        .with_context(|| format!("verifying standalone member '{member}'"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(RailError::message(format!(
        "standalone check failed for member '{member}': {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

fn record_target_evidence(
    result: &mut HashMap<PackageId, MemberEvidence>,
    package_id: &PackageId,
    evidence: &TargetEvidence,
) {
    let member = result
        .entry(package_id.clone())
        .or_insert_with(|| MemberEvidence::new(package_id.clone()));
    member
        .configurations
        .entry(evidence.platform.clone())
        .or_default()
        .insert(evidence.features.clone(), evidence.clone());
}

type CandidateId = (
    crate::cargo::manifest_analyzer::DepKind,
    String,
    Option<FeatureSelection>,
);

fn surface_acquisition_test_fault(ordinal: usize) -> RailResult<()> {
    #[cfg(debug_assertions)]
    {
        let requested = std::env::var("CARGO_RAIL_SURFACE_FAIL_ACQUISITION_VIEW")
            .ok()
            .and_then(|value| value.parse::<usize>().ok());
        if requested == Some(ordinal + 1) {
            return Err(RailError::message(format!(
                "injected Surface compiler acquisition failure at view {}",
                ordinal + 1
            )));
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = ordinal;
    Ok(())
}

fn build_candidate_target_index(
    candidates: &[CompilerCandidate],
) -> HashMap<String, BTreeMap<CandidateId, BTreeSet<String>>> {
    let mut index: HashMap<String, BTreeMap<CandidateId, BTreeSet<String>>> = HashMap::new();
    for candidate in candidates {
        index
            .entry(candidate.member.clone())
            .or_default()
            .entry((
                candidate.kind,
                candidate.crate_name.clone(),
                candidate.required_features.clone(),
            ))
            .or_default()
            .extend(candidate.applicable_targets.iter().cloned());
    }
    index
}

fn has_applicable_survivor(
    surviving_unused: &HashMap<String, BTreeSet<CandidateId>>,
    candidate_targets: &HashMap<String, BTreeMap<CandidateId, BTreeSet<String>>>,
    member: &str,
    target: &str,
    features: &FeatureSelection,
) -> bool {
    surviving_unused.get(member).is_some_and(|survivors| {
        survivors.iter().any(|candidate| {
            candidate_targets
                .get(member)
                .and_then(|targets| targets.get(candidate))
                .is_some_and(|targets| targets.contains(target))
                && candidate.2.as_ref().is_none_or(|required| required == features)
        })
    })
}

fn update_candidate_survivors(
    surviving_unused: &mut HashMap<String, BTreeSet<CandidateId>>,
    candidate_targets: &HashMap<String, BTreeMap<CandidateId, BTreeSet<String>>>,
    member: &str,
    target: &str,
    evidence: &TargetEvidence,
) {
    let Some(targets_by_candidate) = candidate_targets.get(member) else {
        return;
    };
    let Some(survivors) = surviving_unused.get_mut(member) else {
        return;
    };
    survivors.retain(|candidate| {
        let applicable = targets_by_candidate
            .get(candidate)
            .is_some_and(|targets| targets.contains(target))
            && candidate
                .2
                .as_ref()
                .is_none_or(|required| required == &evidence.features);
        if !applicable {
            return true;
        }
        // One missing required configuration permanently prevents an unused
        // proof for this declaration. Continuing to compile its remaining
        // target/feature matrix cannot change the removal decision.
        evidence.completeness == DiagnosticsCompleteness::Complete
            && !evidence.compiled_units.is_empty()
            && evidence.dependency_state_for_kind(&candidate.1, candidate.0) != DependencyEvidenceState::Used
    });
}

struct WorkspaceCheckOutput {
    stdout: String,
    invocations: Vec<crate::compiler::observation::RawCompilerInvocation>,
    compiler_facts: Vec<ValidatedCompilerFactObject>,
    analysis_contract: AnalysisContract,
    artifact_preflight: ArtifactPreflight,
    artifact_high_water_bytes: u64,
}

fn publish_native_analysis_bindings(
    cache: Option<&CompilerAnalysisCache>,
    contract: &AnalysisContract,
    invocations: &[crate::compiler::observation::RawCompilerInvocation],
    facts: &[ValidatedCompilerFactObject],
) -> RailResult<()> {
    let Some(cache) = cache else {
        return Ok(());
    };
    let facts_by_unit = facts
        .iter()
        .map(|fact| (fact.object().unit.identity.as_str(), fact))
        .collect::<BTreeMap<_, _>>();
    let store = crate::compiler::analysis::AnalysisEvidenceStore::from_cas(cache.cas.clone());
    for invocation in invocations {
        let Some(wrapper) = invocation.cache_wrapper.as_ref() else {
            continue;
        };
        let (Some(action), Some(result)) = (wrapper.action_key(), wrapper.result_key()) else {
            continue;
        };
        let bound_facts = match invocation.compiler_fact_unit.as_ref() {
            Some(unit) if contract.requires_typed_facts() => {
                let Some(fact) = facts_by_unit.get(unit.identity.as_str()) else {
                    continue;
                };
                vec![(**fact).clone()]
            }
            Some(_) => continue,
            None => Vec::new(),
        };
        match cache.remote.as_deref() {
            Some(remote) => store.put_with_remote(
                contract,
                action.to_string(),
                result.to_string(),
                invocation,
                &bound_facts,
                remote,
            )?,
            None => store.put(
                contract,
                action.to_string(),
                result.to_string(),
                invocation,
                &bound_facts,
            )?,
        }
    }
    Ok(())
}

struct TypedAcquisitionContext<'a> {
    snapshot: &'a WorkspaceSnapshot,
    driver: &'a PreparedCompilerFactDriver,
    doctest_sysroot: Option<&'a CompilerFactDoctestSysroot>,
    packages: &'a BTreeSet<String>,
}

struct PreparedAcquisitionView<'plan> {
    ordinal: usize,
    view: CompilerAcquisitionView<'plan>,
    typed_members: BTreeSet<String>,
    fact_cache_key: Option<CompilerFactCacheKey>,
    evidence_identity: Option<EvidenceIdentity>,
    collect_typed: bool,
}

struct AcquisitionJob<'plan> {
    prepared: PreparedAcquisitionView<'plan>,
    diagnostic_members: Vec<String>,
    sandbox: SandboxLease,
    started: Option<Instant>,
}

struct AcquisitionWorkerContext<'a> {
    workspace_root: &'a Path,
    identity: &'a CompilerCacheIdentity,
    typed_snapshot: Option<&'a WorkspaceSnapshot>,
    driver: Option<&'a PreparedCompilerFactDriver>,
    doctest_sysroot: Option<&'a CompilerFactDoctestSysroot>,
    artifact_budget: CompilerArtifactBudget,
    package_to_member: &'a HashMap<String, String>,
    cancellation: &'a AtomicBool,
    broker: Option<&'a crate::compiler::acquisition::broker::AcquisitionBroker>,
}

struct AcquisitionOutcome<'plan> {
    worker: usize,
    sandbox: crate::compiler::acquisition::sandbox::ReturnedSandbox,
    completion: AcquisitionCompletion<'plan>,
}

struct AcquisitionCompletion<'plan> {
    prepared: PreparedAcquisitionView<'plan>,
    diagnostic_members: Vec<String>,
    broker_view: Option<crate::compiler::acquisition::broker::BrokerView>,
    started: Option<Instant>,
    failed_cargo_targets: Vec<CompilerAcquisitionCargoTarget>,
    result: RailResult<WorkspaceCheckOutput>,
}

struct AcquisitionFailure {
    ordinal: usize,
    view: Option<ViewIx>,
    cargo_targets: Vec<CompilerAcquisitionCargoTarget>,
    class: FailureClass,
    error: RailError,
}

impl AcquisitionFailure {
    fn view(
        ordinal: usize,
        view: ViewIx,
        cargo_targets: Vec<CompilerAcquisitionCargoTarget>,
        class: FailureClass,
        error: RailError,
    ) -> Self {
        Self {
            ordinal,
            view: Some(view),
            cargo_targets,
            class,
            error,
        }
    }

    fn coordinator(error: RailError) -> Self {
        Self::global(FailureClass::Coordinator, error)
    }

    fn global(class: FailureClass, error: RailError) -> Self {
        Self {
            ordinal: usize::MAX,
            view: None,
            cargo_targets: Vec::new(),
            class,
            error,
        }
    }
}

fn execute_acquisition_job<'plan>(
    worker: usize,
    context: &AcquisitionWorkerContext<'_>,
    job: AcquisitionJob<'plan>,
) -> AcquisitionOutcome<'plan> {
    let AcquisitionJob {
        prepared,
        diagnostic_members,
        sandbox,
        started,
    } = job;
    let mut active_members = diagnostic_members.iter().map(String::as_str).collect::<BTreeSet<_>>();
    active_members.extend(prepared.typed_members.iter().map(String::as_str));
    let active_members = active_members.into_iter().collect::<Vec<_>>();
    let typed = prepared.collect_typed.then(|| TypedAcquisitionContext {
        snapshot: context.typed_snapshot.expect("typed acquisition snapshot was prepared"),
        driver: context.driver.expect("typed acquisition driver was prepared"),
        doctest_sysroot: context.doctest_sysroot,
        packages: &prepared.typed_members,
    });
    let mut failed_cargo_targets = Vec::new();
    let broker_view = context
        .broker
        .map(|broker| broker.begin_view(prepared.view.index()))
        .transpose();
    let (broker_view, result) = match broker_view {
        Ok(broker_view) if !context.cancellation.load(std::sync::atomic::Ordering::Acquire) => {
            let result = run_workspace_check(
                context.workspace_root,
                context.identity,
                prepared.view,
                &active_members,
                &sandbox,
                typed.as_ref(),
                context.artifact_budget,
                context.package_to_member,
                &mut failed_cargo_targets,
                broker_view
                    .as_ref()
                    .map(crate::compiler::acquisition::broker::BrokerView::environment),
                context.cancellation,
            );
            (broker_view, result)
        }
        Ok(broker_view) => (
            broker_view,
            Err(RailError::message(
                "compiler acquisition was cancelled before Cargo started",
            )),
        ),
        Err(error) => (None, Err(error)),
    };
    let sandbox = if result.is_ok() {
        sandbox.finish()
    } else {
        sandbox.poison()
    };
    AcquisitionOutcome {
        worker,
        sandbox,
        completion: AcquisitionCompletion {
            prepared,
            diagnostic_members,
            broker_view,
            started,
            failed_cargo_targets,
            result,
        },
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one coordinator transition durably integrates a complete worker outcome"
)]
fn integrate_completed_acquisition(
    collector: &CompilerDiagnosticsCollector<'_>,
    completion: AcquisitionCompletion<'_>,
    require_durability: bool,
    candidates: &[CompilerCandidate],
    package_to_member: &HashMap<String, String>,
    candidate_targets: &HashMap<String, BTreeMap<CandidateId, BTreeSet<String>>>,
    fact_store: &CompilerFactStore,
    metrics: &mut CompilerAnalysisMetrics,
    compiler_facts: &mut Vec<ValidatedCompilerFactObject>,
    retained_observations: &mut HashMap<String, CompilationObservationManifest>,
    surviving_unused: &mut HashMap<String, BTreeSet<CandidateId>>,
    result: &mut HashMap<PackageId, MemberEvidence>,
    store: &mut CompilerDiagnosticsStore,
) -> RailResult<(ViewIx, Option<EvidenceIdentity>, bool)> {
    let AcquisitionCompletion {
        prepared,
        diagnostic_members,
        broker_view,
        started,
        failed_cargo_targets: _,
        result: run,
    } = completion;
    let index = prepared.view.index();
    let evidence_identity = prepared.evidence_identity.clone();
    let durable = integrate_acquisition_outcome(
        collector,
        prepared,
        diagnostic_members,
        broker_view,
        started,
        run?,
        candidates,
        package_to_member,
        candidate_targets,
        fact_store,
        metrics,
        compiler_facts,
        retained_observations,
        surviving_unused,
        result,
        store,
    )?;
    if require_durability {
        let cache_started = crate::instrumentation::compiler_acquisition_timer();
        let flushed = store.flush();
        crate::instrumentation::record_compiler_acquisition_cache_write(cache_started);
        flushed?;
    }
    Ok((index, evidence_identity, durable))
}

#[expect(
    clippy::too_many_arguments,
    reason = "one coordinator transition integrates a complete view into each owning evidence domain"
)]
fn integrate_acquisition_outcome(
    collector: &CompilerDiagnosticsCollector<'_>,
    prepared: PreparedAcquisitionView<'_>,
    diagnostic_members: Vec<String>,
    broker_view: Option<crate::compiler::acquisition::broker::BrokerView>,
    started: Option<Instant>,
    mut run: WorkspaceCheckOutput,
    candidates: &[CompilerCandidate],
    package_to_member: &HashMap<String, String>,
    candidate_targets: &HashMap<String, BTreeMap<CandidateId, BTreeSet<String>>>,
    fact_store: &CompilerFactStore,
    metrics: &mut CompilerAnalysisMetrics,
    compiler_facts: &mut Vec<ValidatedCompilerFactObject>,
    retained_observations: &mut HashMap<String, CompilationObservationManifest>,
    surviving_unused: &mut HashMap<String, BTreeSet<CandidateId>>,
    result: &mut HashMap<PackageId, MemberEvidence>,
    store: &mut CompilerDiagnosticsStore,
) -> RailResult<bool> {
    let view = prepared.view;
    let diagnostics_must_publish = !diagnostic_members.is_empty();
    let target = view.platform();
    let features = view.features();
    let preflight = run.artifact_preflight;
    progress!(
        "    Compiler artifact preflight: {} bytes available; {} bytes soft; {} bytes hard; {} bytes reserved",
        preflight.initial_available_bytes,
        preflight.soft_limit_bytes,
        preflight.hard_limit_bytes,
        preflight.free_reserve_bytes
    );
    if let Some(observed) = preflight.soft_limit_observed_bytes {
        progress!(
            "    Compiler artifacts: at least {observed} bytes allocated since sandbox creation; soft limit reached"
        );
    }
    metrics.cargo_views_executed += 1;
    metrics.compiler_invocations += run.invocations.len();
    metrics.artifact_high_water_bytes = metrics.artifact_high_water_bytes.max(run.artifact_high_water_bytes);
    let binding_invocations = run.invocations.clone();
    let binding_facts = run.compiler_facts.clone();
    let mut fact_set_published = !prepared.collect_typed;
    if prepared.collect_typed {
        metrics.fresh_fragment_bytes =
            run.compiler_facts
                .iter()
                .try_fold(metrics.fresh_fragment_bytes, |total, fragment| {
                    total
                        .checked_add(fragment.bytes())
                        .ok_or_else(|| RailError::message("compiler fact fragment byte count overflow"))
                })?;
        let fresh_facts = std::mem::take(&mut run.compiler_facts);
        if let Some(key) = &prepared.fact_cache_key {
            let bypasses = fact_invocation_cache_bypasses(&run.invocations, view.compiles_doctests());
            let complete_empty_view =
                fresh_facts.is_empty() && bypasses == BTreeSet::from(["no_typed_compiler_invocation".to_string()]);
            if bypasses.is_empty() || complete_empty_view {
                let cache_started = crate::instrumentation::compiler_acquisition_timer();
                let stored = fact_store.put(key, &fresh_facts);
                crate::instrumentation::record_compiler_acquisition_cache_write(cache_started);
                match stored {
                    Ok(()) => fact_set_published = true,
                    Err(error) => {
                        metrics.fact_cache_store_failures += 1;
                        progress!("    Compiler fact cache store bypassed: {error}");
                    }
                }
            } else {
                for bypass in &bypasses {
                    *metrics.fact_cache_bypass_reasons.entry(bypass.clone()).or_default() += 1;
                }
                progress!(
                    "    Compiler fact cache bypassed: {}",
                    bypasses.into_iter().collect::<Vec<_>>().join(", ")
                );
            }
        }
        compiler_facts.extend(fresh_facts);
    }
    if let Some(started) = started {
        progress!(
            "    Finished target {} in {:.1}s",
            format_args!("{} / {}", target, features.label()),
            started.elapsed().as_secs_f64()
        );
    }
    if view.requires(crate::compiler::scheduler::CompilerFactFamily::StableDiagnostics)
        && !diagnostic_members.is_empty()
    {
        let stale_set = diagnostic_members.iter().map(String::as_str).collect::<HashSet<_>>();
        let parsed = parse_target_run(
            &run.stdout,
            collector.workspace_root,
            package_to_member,
            &stale_set,
            candidates,
        );
        let invocations = std::mem::take(&mut run.invocations);
        let mut compilation_observations =
            parse_compilation_observations(&run.stdout, invocations, &collector.identity, target)?;
        reconcile_exact_artifact_observations(&mut compilation_observations, retained_observations);
        let completeness = DiagnosticsCompleteness::Complete;

        for member in diagnostic_members {
            let manifests_member = collector
                .manifests
                .members
                .iter()
                .find(|manifest| manifest.package_name == member)
                .ok_or_else(|| RailError::message(format!("missing manifest entry for member '{member}'")))?;
            let key = collector.key_for(manifests_member, target, features.clone())?;
            let mut unused = BTreeSet::new();
            let mut compiled = BTreeSet::new();
            if completeness == DiagnosticsCompleteness::Complete
                && let Some(parsed_member) = parsed.get(&member)
            {
                compiled = parsed_member.compiled_targets.clone();
            }
            let unit_evidence = parsed
                .get(&member)
                .map(ParsedMemberTarget::unit_evidence)
                .unwrap_or_default();
            let normal_units = compiled
                .iter()
                .filter(|unit| !unit.test_mode && unit.kind != CargoTargetKind::CustomBuild)
                .collect::<Vec<_>>();
            if !normal_units.is_empty() {
                for candidate in candidates
                    .iter()
                    .filter(|candidate| candidate.member == member && candidate.kind == DepKind::Normal)
                {
                    if normal_units.iter().all(|unit| {
                        unit_evidence
                            .iter()
                            .find(|evidence| &evidence.unit == *unit)
                            .is_some_and(|evidence| evidence.unused_crates.contains(&candidate.crate_name))
                    }) {
                        unused.insert(candidate.crate_name.clone());
                    }
                }
            }
            let evidence = TargetEvidence {
                platform: PlatformTarget::from(target),
                features: features.clone(),
                compiled_units: compiled,
                unused_crates: unused,
                unit_evidence,
                completeness,
            };
            let observations = collector
                .identity
                .package_observation_identities
                .get(&manifests_member.package_id)
                .map(|package| {
                    compilation_observations
                        .iter()
                        .filter(|manifest| manifest.unit.package == *package)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            let entry = CompilerDiagEntry {
                key,
                evidence: evidence.clone(),
                generated_at_unix_ms: now_unix_ms(),
                collector_version: COLLECTOR_VERSION,
                observations,
            };
            update_candidate_survivors(surviving_unused, candidate_targets, &member, target, &entry.evidence);
            record_target_evidence(result, &manifests_member.package_id, &entry.evidence);
            store.put(entry);
        }
    }
    if fact_set_published
        && let Err(error) = publish_native_analysis_bindings(
            collector.identity.analysis_cache.as_ref(),
            &run.analysis_contract,
            &binding_invocations,
            &binding_facts,
        )
    {
        progress!("    Native analysis binding publication bypassed: {error}");
    }
    if let Some(broker_view) = broker_view {
        broker_view.finish()?;
    }
    Ok(
        (!prepared.collect_typed || fact_set_published && fact_store.durability_available())
            && (!diagnostics_must_publish || store.durability_available()),
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "one compiler acquisition keeps its captured authority, exact view, output, and failure evidence together"
)]
fn run_workspace_check(
    workspace_root: &Path,
    identity: &CompilerCacheIdentity,
    view: CompilerAcquisitionView<'_>,
    members: &[&str],
    sandbox: &SandboxLease,
    typed: Option<&TypedAcquisitionContext<'_>>,
    artifact_budget: CompilerArtifactBudget,
    package_to_member: &HashMap<String, String>,
    failed_cargo_targets: &mut Vec<CompilerAcquisitionCargoTarget>,
    broker_environment: Option<&crate::compiler::acquisition::broker::BrokerEnvironment>,
    cancellation: &AtomicBool,
) -> RailResult<WorkspaceCheckOutput> {
    if members != [view.package()] {
        return Err(RailError::message(
            "compiler acquisition execution requires its exact one-package view",
        ));
    }
    let wrapper = compiler_observation_wrapper()?;
    let cargo_target = sandbox.target_dir().to_path_buf();
    let cargo_build = sandbox.build_dir().to_path_buf();
    let existing_workspace_wrapper = identity.rustc_workspace_wrapper.as_deref();
    let observation_directory = tempfile::Builder::new()
        .prefix("cargo-rail-compiler-observations-")
        .tempdir()
        .with_context(|| "creating compiler observation directory".to_string())?;
    let workspace_wrapper = if typed.is_some() {
        stage_view_workspace_wrapper(&wrapper, observation_directory.path())?
    } else {
        wrapper.clone()
    };
    let doctest_sysroot = if view.compiles_doctests() {
        let typed =
            typed.ok_or_else(|| RailError::message("compile-only doctest view has no typed compiler authority"))?;
        Some(
            typed
                .doctest_sysroot
                .ok_or_else(|| RailError::message("compile-only doctest view has no staged compiler sysroot"))?,
        )
    } else {
        None
    };
    let typed_session = typed
        .map(|typed| {
            typed_session(
                typed,
                view,
                members,
                observation_directory.path(),
                &cargo_target,
                &cargo_build,
                doctest_sysroot,
            )
        })
        .transpose()?;
    let fact_families = if typed.is_some() {
        view.fact_families()
    } else {
        BTreeSet::from([crate::compiler::scheduler::CompilerFactFamily::StableDiagnostics])
    };
    let source_root = typed.map_or(workspace_root, |typed| typed.snapshot.source_root());
    let configuration_identity = format!(
        "sha256:{}",
        ContentDigest::sha256(&serde_json::to_vec(&(
            &identity.compiler_env_fingerprint,
            &identity.cargo_config_fingerprint,
        ))?)
    );
    let analysis_contract = AnalysisContract::new(
        fact_families,
        view.package().to_string(),
        view.platform().to_string(),
        view.features(),
        view.command_class().to_string(),
        configuration_identity,
        typed_session.as_ref().map(|session| session.producer_authority.clone()),
        typed_session
            .as_ref()
            .map_or_else(BTreeSet::new, |session| session.required_coverage.clone()),
    )?;
    let fact_session = CompilerFactSession::write_with_typed(
        observation_directory.path(),
        source_root,
        analysis_contract.clone(),
        typed_session.clone(),
        broker_environment.cloned(),
    )?;
    let args = view.cargo_arguments();

    let mut command = typed.map_or_else(
        || Command::new(&identity.cargo_program),
        |typed| typed.driver.cargo_command(&identity.cargo_program),
    );
    command
        .current_dir(workspace_root)
        .env("RUSTC_WORKSPACE_WRAPPER", &workspace_wrapper)
        .env(WRAPPER_MARKER, "1")
        .env(OBSERVATION_DIRECTORY_ENV, observation_directory.path())
        .env(OBSERVATION_SOURCE_ROOT_ENV, source_root)
        .env(FACT_SESSION_ENV, fact_session)
        .env("CARGO_TARGET_DIR", &cargo_target)
        .env("CARGO_BUILD_BUILD_DIR", &cargo_build)
        .env_remove(CACHE_WRAPPER_MARKER)
        .args(&args)
        .args(["--jobs", "1"]);
    if let Some(wrapper) = identity.rustc_wrapper.as_ref() {
        // Execute the global wrapper captured by the authoritative workspace
        // snapshot instead of asking this later Cargo process to re-resolve it.
        command.env("RUSTC_WRAPPER", wrapper);
    }
    if typed.is_some() {
        command.env("RUSTC", &identity.rustc_program);
    }
    if view.compiles_doctests() {
        command
            .env("RUSTDOC", &workspace_wrapper)
            .env(INNER_RUSTDOC_ENV, &identity.rustdoc_program)
            .env(RUSTDOC_WRAPPER_MARKER, "1");
    }
    if typed.is_none()
        && let Some(inner_wrapper) = existing_workspace_wrapper
        && inner_wrapper != wrapper.as_os_str()
    {
        command.env(INNER_WRAPPER_ENV, inner_wrapper);
    }

    #[cfg(test)]
    QUALIFICATION_CARGO_VIEWS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let bounded = run_artifact_bounded_command(
        &mut command,
        &cargo_target,
        artifact_budget,
        cancellation,
        broker_environment.is_some(),
    )
    .with_context(|| {
        format!(
            "running cargo check for target '{target}' in {}",
            workspace_root.display(),
            target = view.platform()
        )
    })?;
    if !bounded.status.success() {
        *failed_cargo_targets = compiler_error_targets(&bounded.stdout, package_to_member);
        let diagnostics = cargo_failure_diagnostics(&bounded.stdout);
        let stderr = if diagnostics.is_empty() {
            bounded_cargo_failure_stderr(&bounded.stderr)
        } else {
            String::new()
        };
        return Err(RailError::message(format!(
            "compiler-evidence Cargo acquisition failed with status {}{}{}",
            bounded.status,
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            },
            if diagnostics.is_empty() {
                String::new()
            } else {
                format!("\n{diagnostics}")
            }
        )));
    }
    if let Some(doctest_sysroot) = doctest_sysroot {
        doctest_sysroot.revalidate()?;
    }

    let invocations = load_raw(observation_directory.path())?;
    crate::instrumentation::record_compiler_acquisition_actions(invocations.len());
    #[cfg(test)]
    QUALIFICATION_COMPILER_INVOCATIONS.fetch_add(invocations.len(), std::sync::atomic::Ordering::Relaxed);
    let compiler_fact_fragments = typed_session.as_ref().map_or_else(
    || Ok(Vec::new()),
    |typed| {
      let expected_artifacts =
        selected_typed_artifact_count(&String::from_utf8_lossy(&bounded.stdout), source_root, typed)?;
      let fragments = load_compiler_fact_fragments(
        &String::from_utf8_lossy(&bounded.stdout),
        observation_directory.path(),
        &invocations,
        typed,
      )?;
      if !typed.doctest && fragments.len() != expected_artifacts {
        return Err(RailError::message(format!(
          "typed compiler fact acquisition produced {} fragments for {expected_artifacts} selected Cargo artifacts",
          fragments.len()
        )));
      }
      Ok(fragments)
    },
  )?;
    let mut compiler_facts = compiler_fact_fragments
        .into_iter()
        .map(ValidatedCompilerFactFragment::into_object)
        .collect::<Vec<_>>();
    compiler_facts.extend(crate::compiler::analysis::load_fact_imports(
        observation_directory.path(),
        &analysis_contract,
        &invocations,
    )?);
    compiler_facts.sort_by(|left, right| left.identity().cmp(right.identity()));
    if compiler_facts
        .windows(2)
        .any(|pair| pair[0].identity() == pair[1].identity())
    {
        return Err(RailError::message(
            "compiler fact acquisition produced duplicate fresh and imported objects",
        ));
    }
    Ok(WorkspaceCheckOutput {
        stdout: String::from_utf8_lossy(&bounded.stdout).into_owned(),
        invocations,
        compiler_facts,
        analysis_contract,
        artifact_preflight: bounded.preflight,
        artifact_high_water_bytes: bounded.high_water_bytes,
    })
}

fn compiler_error_targets(
    stdout: &[u8],
    package_to_member: &HashMap<String, String>,
) -> Vec<CompilerAcquisitionCargoTarget> {
    let mut targets = Message::parse_stream(BufReader::new(stdout))
        .filter_map(Result::ok)
        .filter_map(|message| match message {
            Message::CompilerMessage(message)
                if matches!(
                    message.message.level,
                    cargo_metadata::diagnostic::DiagnosticLevel::Error
                        | cargo_metadata::diagnostic::DiagnosticLevel::Ice
                ) =>
            {
                let package_id = message.package_id.to_string();
                Some(CompilerAcquisitionCargoTarget {
                    package: package_to_member.get(&package_id).cloned().unwrap_or(package_id),
                    target: message.target.name,
                    kinds: message
                        .target
                        .kind
                        .iter()
                        .filter_map(|kind| serde_json::to_value(kind).ok())
                        .filter_map(|kind| kind.as_str().map(str::to_string))
                        .collect(),
                })
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    targets.sort();
    targets
}

const MAX_COMPILER_ARTIFACT_FREE_RESERVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const COMPILER_ARTIFACT_FREE_RESERVE_DIVISOR: u64 = 10;
const COMPILER_ARTIFACT_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Preserve ten percent on small volumes without reserving more than two GiB on normal workspace storage.
fn compiler_artifact_free_reserve_bytes(total_space: u64) -> u64 {
    (total_space / COMPILER_ARTIFACT_FREE_RESERVE_DIVISOR).min(MAX_COMPILER_ARTIFACT_FREE_RESERVE_BYTES)
}

#[derive(Debug)]
struct ArtifactBoundedCommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    preflight: ArtifactPreflight,
    high_water_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
struct ArtifactPreflight {
    initial_available_bytes: u64,
    soft_limit_bytes: u64,
    hard_limit_bytes: u64,
    free_reserve_bytes: u64,
    soft_limit_observed_bytes: Option<u64>,
}

#[cfg(test)]
struct QualificationLiveCargoProcess;

#[cfg(test)]
impl QualificationLiveCargoProcess {
    fn start() -> Self {
        let live = QUALIFICATION_LIVE_CARGO_VIEWS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        QUALIFICATION_MAX_LIVE_CARGO_VIEWS.fetch_max(live, std::sync::atomic::Ordering::Relaxed);
        Self
    }
}

#[cfg(test)]
impl Drop for QualificationLiveCargoProcess {
    fn drop(&mut self) {
        let previous = QUALIFICATION_LIVE_CARGO_VIEWS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        debug_assert!(previous > 0, "qualification live Cargo process counter underflowed");
    }
}

fn run_artifact_bounded_command(
    command: &mut Command,
    artifact_root: &Path,
    budget: CompilerArtifactBudget,
    cancellation: &AtomicBool,
    broker_enabled: bool,
) -> RailResult<ArtifactBoundedCommandOutput> {
    if budget.soft_limit_bytes == 0 || budget.hard_limit_bytes < budget.soft_limit_bytes {
        return Err(RailError::message("compiler artifact storage budget is invalid"));
    }
    let cargo_started = crate::instrumentation::compiler_acquisition_timer();
    let initial_available = fs2::available_space(artifact_root).with_context(|| {
        format!(
            "measuring filesystem capacity for compiler artifact root '{}'",
            artifact_root.display()
        )
    })?;
    let total_space = fs2::total_space(artifact_root).with_context(|| {
        format!(
            "measuring filesystem size for compiler artifact root '{}'",
            artifact_root.display()
        )
    })?;
    let free_reserve = compiler_artifact_free_reserve_bytes(total_space);
    let capacity_limit = initial_available.saturating_sub(free_reserve);
    let effective_hard_limit = budget.hard_limit_bytes.min(capacity_limit);
    if effective_hard_limit == 0 {
        return Err(RailError::with_help(
            format!(
                "compiler artifact preflight found {initial_available} available bytes, which cannot preserve the {}-byte free-space reserve",
                free_reserve
            ),
            "free workspace storage or configure the operation on a filesystem with more available capacity",
        ));
    }
    let effective_soft_limit = budget.soft_limit_bytes.min(effective_hard_limit);
    if cancellation.load(std::sync::atomic::Ordering::Acquire) {
        return Err(RailError::message(
            "compiler acquisition was cancelled before Cargo started",
        ));
    }
    let mut process = ProcessTree::spawn(command)?;
    let _live_process = crate::instrumentation::compiler_acquisition_process_started(!broker_enabled);
    #[cfg(test)]
    let _qualification_live_process = QualificationLiveCargoProcess::start();
    let stdout = process.take_stdout()?;
    let stderr = process.take_stderr()?;
    let stream_failed = Arc::new(AtomicBool::new(false));
    let stdout_failed = Arc::clone(&stream_failed);
    let stdout_reader = match std::thread::Builder::new()
        .name("cargo-rail-cargo-stdout".to_string())
        .spawn(move || {
            let result = read_cargo_stdout(stdout);
            if result.is_err() {
                stdout_failed.store(true, std::sync::atomic::Ordering::Release);
            }
            result
        }) {
        Ok(reader) => reader,
        Err(spawn_error) => {
            let cleanup = terminate_process_tree(&mut process);
            return Err(RailError::message(format!(
                "failed to spawn Cargo stdout reader: {spawn_error}; process cleanup: {}",
                cleanup
                    .map(|termination| format!("complete after {:.3}s", termination.elapsed.as_secs_f64()))
                    .unwrap_or_else(|error| format!("failed: {error}"))
            )));
        }
    };
    let stderr_failed = Arc::clone(&stream_failed);
    let stderr_reader = match std::thread::Builder::new()
        .name("cargo-rail-cargo-stderr".to_string())
        .spawn(move || {
            let result = read_cargo_stderr_tail(stderr);
            if result.is_err() {
                stderr_failed.store(true, std::sync::atomic::Ordering::Release);
            }
            result
        }) {
        Ok(reader) => reader,
        Err(spawn_error) => {
            let cleanup = terminate_process_tree(&mut process);
            let stdout_join = stdout_reader.join();
            return Err(RailError::message(format!(
                "failed to spawn Cargo stderr reader: {spawn_error}; process cleanup: {}; stdout reader: {}",
                cleanup
                    .map(|termination| format!("complete after {:.3}s", termination.elapsed.as_secs_f64()))
                    .unwrap_or_else(|error| format!("failed: {error}")),
                if stdout_join.is_ok() { "joined" } else { "panicked" }
            )));
        }
    };

    let probe_stride_bytes = (effective_hard_limit / 8).clamp(1024 * 1024, 8 * 1024 * 1024 * 1024);
    let mut next_probe_bytes = effective_soft_limit;
    let mut high_water_bytes = 0_u64;
    let mut soft_reported = false;
    let mut budget_breach = None;
    let mut monitor_error = None;
    let mut cancelled = false;
    let mut status = None;
    loop {
        match process.try_wait() {
            Ok(Some(completed)) => {
                status = Some(completed);
                break;
            }
            Ok(None) => {}
            Err(error) => {
                monitor_error = Some(error);
                break;
            }
        }
        if process.cancellation_requested() || cancellation.load(std::sync::atomic::Ordering::Acquire) {
            cancelled = true;
            break;
        }
        if stream_failed.load(std::sync::atomic::Ordering::Acquire) {
            break;
        }
        match fs2::available_space(artifact_root) {
            Ok(available) => {
                let filesystem_delta = initial_available.saturating_sub(available);
                high_water_bytes = high_water_bytes.max(filesystem_delta);
                if !soft_reported && filesystem_delta >= effective_soft_limit {
                    soft_reported = true;
                }
                if filesystem_delta >= next_probe_bytes {
                    match compiler_artifact_bytes(artifact_root) {
                        Ok(current_bytes) => {
                            high_water_bytes = high_water_bytes.max(current_bytes);
                            if current_bytes > effective_hard_limit {
                                budget_breach = Some(current_bytes);
                                break;
                            }
                        }
                        Err(error) => {
                            monitor_error = Some(std::io::Error::other(error.to_string()));
                            break;
                        }
                    }
                    next_probe_bytes = filesystem_delta.saturating_add(probe_stride_bytes);
                }
            }
            Err(error) => {
                monitor_error = Some(error);
                break;
            }
        }
        std::thread::sleep(COMPILER_ARTIFACT_POLL_INTERVAL);
    }

    if let Some(completed) = status {
        if let Err(error) = process.finish(completed) {
            monitor_error = Some(error);
        }
    } else {
        match terminate_process_tree(&mut process) {
            Ok(_) => {}
            Err(error) => monitor_error = Some(error),
        }
    }
    let stdout_join = stdout_reader.join();
    let stderr_join = stderr_reader.join();
    let stdout = stdout_join.map_err(|_| RailError::message("Cargo compiler acquisition stdout reader panicked"))?;
    let stderr = stderr_join.map_err(|_| RailError::message("Cargo compiler acquisition stderr reader panicked"))?;
    let stdout_bytes_read = stdout.as_ref().map_or(0, |output| output.bytes_read());
    let stderr_bytes_read = stderr.as_ref().map_or(0, |output| output.bytes_read());
    let stdout_bytes_retained = stdout.as_ref().map_or(0, |output| output.retained_bytes());
    let stderr_bytes_retained = stderr.as_ref().map_or(0, |output| output.retained_bytes());
    let cargo_messages = stdout.as_ref().map_or(0, |output| output.messages_read());
    crate::instrumentation::record_compiler_acquisition_cargo_view(
        cargo_started,
        cargo_messages,
        stdout_bytes_read,
        stderr_bytes_read,
        stdout_bytes_retained,
        stderr_bytes_retained,
    );
    if let Some(error) = monitor_error {
        let trigger = if cancelled {
            Some("cancellation was requested".to_string())
        } else if let Err(stream_error) = &stdout {
            Some(format!("Cargo stdout {}: {stream_error}", stream_error.class()))
        } else if let Err(stream_error) = &stderr {
            Some(format!("Cargo stderr reader failure: {stream_error}"))
        } else {
            budget_breach.map(|current_bytes| format!("artifact working set reached {current_bytes} bytes"))
        };
        return Err(RailError::message(format!(
            "compiler acquisition process ownership failed: {error}{}",
            trigger.map_or_else(String::new, |trigger| format!("; triggering failure: {trigger}"))
        )));
    }
    if cancelled {
        return Err(RailError::message(
            "compiler acquisition was cancelled by SIGINT or SIGTERM",
        ));
    }
    let stdout = stdout
        .map_err(|error| RailError::message(format!("Cargo compiler acquisition stdout {}: {error}", error.class())))?;
    let stderr = stderr
        .map_err(|error| RailError::message(format!("Cargo compiler acquisition stderr reader failure: {error}")))?;
    if let Some(current_bytes) = budget_breach {
        return Err(RailError::with_help(
            format!(
                "compiler artifact working set reached {current_bytes} bytes and exceeded its {effective_hard_limit}-byte hard limit"
            ),
            "increase unify.compiler_artifact_hard_limit_bytes only after verifying the workspace's required single-view working set",
        ));
    }

    let final_bytes = compiler_artifact_bytes(artifact_root)?;
    high_water_bytes = high_water_bytes.max(final_bytes);
    if final_bytes > effective_hard_limit {
        return Err(RailError::with_help(
            format!(
                "compiler artifact working set finished at {final_bytes} bytes and exceeded its {effective_hard_limit}-byte hard limit"
            ),
            "increase unify.compiler_artifact_hard_limit_bytes only after verifying the workspace's required single-view working set",
        ));
    }
    let status = status.ok_or_else(|| RailError::message("Cargo compiler acquisition exited without a status"))?;
    Ok(ArtifactBoundedCommandOutput {
        status,
        stdout: stdout.retained().to_vec(),
        stderr: stderr.tail().to_vec(),
        preflight: ArtifactPreflight {
            initial_available_bytes: initial_available,
            soft_limit_bytes: effective_soft_limit,
            hard_limit_bytes: effective_hard_limit,
            free_reserve_bytes: free_reserve,
            soft_limit_observed_bytes: soft_reported.then_some(high_water_bytes),
        },
        high_water_bytes,
    })
}

fn terminate_process_tree(process: &mut ProcessTree) -> std::io::Result<ProcessTermination> {
    process.terminate()
}

fn stage_view_workspace_wrapper(wrapper: &Path, directory: &Path) -> RailResult<PathBuf> {
    let staged = directory.join(format!("cargo-rail-compiler-wrapper{}", std::env::consts::EXE_SUFFIX));
    fs::hard_link(wrapper, &staged)
        .or_else(|_| fs::copy(wrapper, &staged).map(|_| ()))
        .with_context(|| {
            format!(
                "staging compiler-observation wrapper '{}' as '{}'",
                wrapper.display(),
                staged.display()
            )
        })?;
    Ok(staged)
}

fn cargo_failure_diagnostics(stdout: &[u8]) -> String {
    const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024;
    let mut diagnostics = String::new();
    for line in String::from_utf8_lossy(stdout).lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if event["reason"] != "compiler-message" || event["message"]["level"] != "error" {
            continue;
        }
        let message = event["message"]["rendered"]
            .as_str()
            .or_else(|| event["message"]["message"].as_str())
            .unwrap_or("compiler reported an error");
        if diagnostics.len().saturating_add(message.len()) > MAX_DIAGNOSTIC_BYTES {
            diagnostics.push_str("compiler diagnostics truncated");
            break;
        }
        diagnostics.push_str(message.trim_end());
        diagnostics.push('\n');
    }
    diagnostics
}

fn bounded_cargo_failure_stderr(stderr: &[u8]) -> String {
    const MAX_STDERR_BYTES: usize = 16 * 1024;
    let start = stderr.len().saturating_sub(MAX_STDERR_BYTES);
    String::from_utf8_lossy(&stderr[start..]).trim().to_string()
}

fn fact_invocation_cache_bypasses(
    invocations: &[crate::compiler::observation::RawCompilerInvocation],
    completed_doctest_view: bool,
) -> BTreeSet<String> {
    let mut observed = false;
    let mut bypasses = BTreeSet::new();
    for invocation in invocations
        .iter()
        .filter(|invocation| invocation.compiler_fact_unit.is_some())
    {
        observed = true;
        let expected_doctest_failure = is_expected_doctest_compile_failure(invocation, completed_doctest_view);
        if !invocation.success && !expected_doctest_failure {
            bypasses.insert("compiler_invocation_failed".to_string());
        }
        bypasses.extend(
            invocation
                .bypasses
                .iter()
                .filter(|bypass| {
                    !expected_doctest_failure
                        || !matches!(
                            bypass.as_str(),
                            "dep_info_output_bytes_unavailable"
                                | "dep_info_output_symlink_unavailable"
                                | "dep_info_unavailable"
                                | "emitted_output_bytes_unavailable"
                                | "emitted_output_symlink_unavailable"
                        )
                })
                .cloned(),
        );
    }
    if !observed {
        bypasses.insert("no_typed_compiler_invocation".to_string());
    }
    bypasses
}

/// Rustdoc is the authority that classifies a builder failure as `compile_fail`:
/// this helper is called only after the enclosing doctest command succeeded.
/// Requiring an ordinary rustc exit code of one prevents a missing executable,
/// signal, or wrapper setup failure from being accepted as an expected test.
fn is_expected_doctest_compile_failure(
    invocation: &crate::compiler::observation::RawCompilerInvocation,
    completed_doctest_view: bool,
) -> bool {
    completed_doctest_view
        && !invocation.success
        && invocation.compiler_exit_code == Some(1)
        && invocation
            .compiler_fact_unit
            .as_ref()
            .is_some_and(|unit| unit.domain == crate::compiler::facts::CompilerFactDomain::Doctest)
}

fn selected_typed_artifact_count(
    stdout: &str,
    source_root: &Path,
    session: &CompilerFactTypedSession,
) -> RailResult<usize> {
    if session.doctest {
        return Ok(0);
    }
    let selected = session
        .targets
        .iter()
        .map(|target| {
            Ok((
                target.cargo_target.clone(),
                crate::utils::canonicalize_existing(&source_root.join(&target.source))?,
            ))
        })
        .collect::<RailResult<BTreeSet<_>>>()?;
    selected_typed_artifact_count_for(stdout, &selected)
}

fn selected_typed_artifact_count_for(stdout: &str, selected: &BTreeSet<(String, PathBuf)>) -> RailResult<usize> {
    let mut count = 0usize;
    for line in stdout.lines() {
        let Ok(event) = serde_json::from_str::<CargoEvent>(line) else {
            continue;
        };
        if event.reason != "compiler-artifact" {
            continue;
        }
        let Some(target) = event.target else {
            continue;
        };
        let Some(source) = target
            .src_path
            .as_deref()
            .map(Path::new)
            .and_then(|path| crate::utils::canonicalize_existing(path).ok())
        else {
            continue;
        };
        if !selected.contains(&(target.name.clone(), source)) {
            continue;
        }
        if event.fresh != Some(false) {
            return Err(RailError::message(format!(
                "Cargo freshness suppressed required typed facts for selected target '{}'",
                target.name
            )));
        }
        count = count
            .checked_add(1)
            .ok_or_else(|| RailError::message("selected typed Cargo artifact count overflowed"))?;
    }
    Ok(count)
}

fn typed_session(
    context: &TypedAcquisitionContext<'_>,
    view: CompilerAcquisitionView<'_>,
    members: &[&str],
    observation_directory: &Path,
    typed_cargo_target: &Path,
    typed_cargo_build: &Path,
    doctest_sysroot: Option<&CompilerFactDoctestSysroot>,
) -> RailResult<CompilerFactTypedSession> {
    if !view.requires(crate::compiler::scheduler::CompilerFactFamily::TypedRustItems) {
        return Err(RailError::message(
            "typed compiler driver was supplied to a view that does not request typed facts",
        ));
    }
    if members != [view.package()] || context.packages != &BTreeSet::from([view.package().to_string()]) {
        return Err(RailError::message(
            "typed compiler acquisition requires its exact one-package view",
        ));
    }
    let targets = CompilerFactTypedSession::targets_from_snapshot(context.snapshot, context.packages)?;
    let view_identity = view.fact_cache_identity(view.package())?;
    let mut hasher = Sha256::new();
    hasher.update(b"cargo-rail-compiler-fact-run-v1\0");
    hasher.update(&(view_identity.len() as u64).to_le_bytes());
    hasher.update(view_identity.as_bytes());
    hasher.update(&(observation_directory.as_os_str().as_encoded_bytes().len() as u64).to_le_bytes());
    hasher.update(observation_directory.as_os_str().as_encoded_bytes());
    let run_identity = format!(
        "{RUN_IDENTITY_PREFIX}{}",
        ContentDigest::from_sha256_bytes(hasher.finalize())
    );
    let host_platform = context.snapshot.toolchain().host_target().to_string();
    let target_platform = if view.platform() == "default" {
        host_platform.clone()
    } else {
        view.platform().to_string()
    };
    let driver_program = context
        .driver
        .program()
        .to_str()
        .ok_or_else(|| RailError::message("compiler fact driver path is not valid UTF-8"))?
        .to_string();
    let compiler_library_directory = context
        .driver
        .compiler_library_directory()
        .to_str()
        .ok_or_else(|| RailError::message("compiler fact runtime library path is not valid UTF-8"))?
        .to_string();
    let rustc_program = crate::executable::resolve_executable_path(
        context.snapshot.toolchain().rustc_program(),
        context.snapshot.cargo_current_dir(),
    )?
    .to_str()
    .ok_or_else(|| RailError::message("selected rustc path is not valid UTF-8"))?
    .to_string();
    let generated_roots = vec![typed_cargo_target.to_path_buf(), typed_cargo_build.to_path_buf()];
    let mut generated_roots = generated_roots
        .into_iter()
        .map(|root| {
            crate::utils::canonicalize_allow_missing(&root)?
                .to_str()
                .map(str::to_string)
                .ok_or_else(|| RailError::message("Cargo generated-source root is not valid UTF-8"))
        })
        .collect::<RailResult<Vec<_>>>()?;
    generated_roots.sort();
    generated_roots.dedup();
    Ok(CompilerFactTypedSession {
        run_authority: CompilerFactRunAuthority {
            run_identity,
            view_identity,
        },
        producer_authority: context.driver.producer_authority().clone(),
        driver_program,
        rustc_program,
        compiler_library_directory,
        host_platform,
        target_platform,
        doctest: view.compiles_doctests(),
        doctest_sysroot: doctest_sysroot
            .map(CompilerFactDoctestSysroot::path)
            .map(|path| {
                path.to_str()
                    .map(str::to_string)
                    .ok_or_else(|| RailError::message("private doctest sysroot path is not valid UTF-8"))
            })
            .transpose()?,
        generated_roots,
        required_coverage: required_compiler_fact_coverage(),
        targets,
    })
}

fn load_compiler_fact_fragments(
    stdout: &str,
    observation_directory: &Path,
    invocations: &[crate::compiler::observation::RawCompilerInvocation],
    session: &CompilerFactTypedSession,
) -> RailResult<Vec<ValidatedCompilerFactFragment>> {
    let mut expected = BTreeMap::new();
    for invocation in invocations {
        let Some(unit) = &invocation.compiler_fact_unit else {
            continue;
        };
        if !invocation.success && !is_expected_doctest_compile_failure(invocation, session.doctest) {
            return Err(RailError::message(
                "typed compiler fact invocation failed before publishing complete facts",
            ));
        }
        if !invocation.success {
            continue;
        }
        if let Some(previous) = expected.insert(unit.identity.clone(), unit)
            && previous != unit
        {
            return Err(RailError::message(
                "typed compiler fact acquisition observed a compilation-unit identity collision",
            ));
        }
    }
    let mut fragments = Vec::with_capacity(expected.len());
    let mut announced_sidecars = BTreeSet::new();
    let mut announced_units = BTreeMap::<String, (String, String, u64)>::new();
    for line in stdout.lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if event["reason"] != "compiler-message"
            || event["message"]["code"]["code"].as_str() != Some(COMPILER_FACT_ANNOUNCEMENT_CODE)
        {
            continue;
        }
        let message = event["message"]["message"]
            .as_str()
            .ok_or_else(|| RailError::message("compiler fact announcement has no diagnostic message"))?;
        let payload = message
            .strip_prefix(COMPILER_FACT_ANNOUNCEMENT_PREFIX)
            .ok_or_else(|| RailError::message("compiler fact announcement has an incompatible message envelope"))?;
        let untrusted: CompilerFactAnnouncement = serde_json::from_str(payload)?;
        if untrusted.run_authority != session.run_authority {
            if untrusted.version == COMPILER_FACT_PROTOCOL_VERSION
                && untrusted.producer_authority == session.producer_authority
            {
                // Cargo replays cached diagnostics when a later view reuses the shared
                // target directory. The run authority proves this announcement belongs
                // to an earlier acquisition; its authenticated object was already
                // consumed by that view and cannot authorize the current one.
                continue;
            }
            return Err(RailError::message(
                "compiler fact announcement has incompatible replay authority",
            ));
        }
        let unit = expected.get(&untrusted.unit_identity).ok_or_else(|| {
            RailError::message(format!(
                "current compiler fact announcement names unauthorized compilation unit '{}'",
                untrusted.unit_identity
            ))
        })?;
        validate_compiler_fact_cargo_envelope(&event, unit, session)?;
        let announcement_expectation = CompilerFactAnnouncementExpectation::new(
            session.run_authority.clone(),
            session.producer_authority.clone(),
            unit.identity.clone(),
        );
        let announcement = ValidatedCompilerFactAnnouncement::from_compiler_message(
            Some(COMPILER_FACT_ANNOUNCEMENT_CODE),
            message,
            &announcement_expectation,
        )?
        .ok_or_else(|| RailError::message("reserved compiler fact announcement was ignored"))?;
        let digest = announcement
            .content_digest()
            .strip_prefix("sha256:")
            .ok_or_else(|| RailError::message("compiler fact announcement content digest is invalid"))?;
        announced_sidecars.insert(format!("compiler-fact-fragment-sha256-{digest}.json"));
        let announcement_identity = (
            announcement.object_identity().to_string(),
            announcement.content_digest().to_string(),
            announcement.bytes(),
        );
        if let Some(previous) = announced_units.get(&unit.identity) {
            if previous != &announcement_identity {
                return Err(RailError::message(
                    "repeated compiler fact announcement names conflicting content for one compilation unit",
                ));
            }
            continue;
        }
        let fragment_expectation = CompilerFactExpectation::new(
            session.run_authority.clone(),
            session.producer_authority.clone(),
            unit.identity.clone(),
            session.required_coverage.clone(),
        );
        fragments.push(load_announced_fragment(
            observation_directory,
            &announcement,
            &fragment_expectation,
        )?);
        announced_units.insert(unit.identity.clone(), announcement_identity);
    }
    for unit in announced_units.keys() {
        expected.remove(unit);
    }
    if expected
        .values()
        .any(|unit| unit.domain != crate::compiler::facts::CompilerFactDomain::Doctest)
    {
        return Err(RailError::message(
            "typed compiler fact acquisition omitted a Cargo-routed announcement",
        ));
    }
    let doctest_expectations = expected
        .iter()
        .map(|(identity, unit)| {
            (
                identity.clone(),
                CompilerFactExpectation::new(
                    session.run_authority.clone(),
                    session.producer_authority.clone(),
                    unit.identity.clone(),
                    session.required_coverage.clone(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for entry in fs::read_dir(observation_directory)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        if !file_name.starts_with("compiler-fact-fragment-sha256-") {
            continue;
        }
        if announced_sidecars.contains(file_name) {
            continue;
        }
        let (unit_identity, fragment) = load_discovered_doctest_fragment(&entry.path(), &doctest_expectations)?;
        if expected.remove(&unit_identity).is_none() {
            return Err(RailError::message(
                "typed doctest fact acquisition produced a duplicate compilation unit",
            ));
        }
        fragments.push(fragment);
    }
    if !expected.is_empty() {
        return Err(RailError::message(format!(
            "typed compiler fact acquisition is incomplete for {} compilation unit{}",
            expected.len(),
            if expected.len() == 1 { "" } else { "s" }
        )));
    }
    fragments.sort_by(|left, right| left.object_identity().cmp(right.object_identity()));
    if fragments
        .windows(2)
        .any(|pair| pair[0].object_identity() == pair[1].object_identity())
    {
        return Err(RailError::message(
            "typed compiler fact acquisition produced duplicate object identities",
        ));
    }
    Ok(fragments)
}

fn validate_compiler_fact_cargo_envelope(
    event: &serde_json::Value,
    unit: &crate::compiler::facts::CompilerFactUnit,
    session: &CompilerFactTypedSession,
) -> RailResult<()> {
    let target = session
        .targets
        .iter()
        .find(|target| {
            target.package == unit.package
                && target.cargo_target == unit.cargo_target
                && target.target_kind == unit.target_kind
        })
        .ok_or_else(|| RailError::message("compiler fact unit is outside its captured Cargo target authority"))?;
    if event["target"]["name"].as_str() != Some(unit.cargo_target.as_str()) {
        return Err(RailError::message(
            "compiler fact announcement does not match Cargo's target envelope",
        ));
    }
    let kinds = event["target"]["kind"]
        .as_array()
        .ok_or_else(|| RailError::message("compiler fact Cargo envelope has no target kinds"))?;
    let kind_matches = kinds.iter().filter_map(serde_json::Value::as_str).any(|kind| {
        matches!(
            (&unit.target_kind, kind),
            (
                CompilerFactTargetKind::Library,
                "lib" | "rlib" | "dylib" | "cdylib" | "staticlib"
            ) | (CompilerFactTargetKind::Binary, "bin")
                | (CompilerFactTargetKind::Test, "test")
                | (CompilerFactTargetKind::Example, "example")
                | (CompilerFactTargetKind::Benchmark, "bench")
                | (CompilerFactTargetKind::ProcMacro, "proc-macro")
                | (CompilerFactTargetKind::BuildScript, "custom-build")
        ) || matches!(&unit.target_kind, CompilerFactTargetKind::Other(other) if other == kind)
    });
    if !kind_matches {
        return Err(RailError::message(format!(
            "compiler fact announcement for '{}:{}' has target kinds {kinds:?}, but the authorized unit is {:?} and the captured target is {:?}",
            unit.package.name, unit.cargo_target, unit.target_kind, target.target_kind
        )));
    }
    Ok(())
}

#[derive(Debug, Default)]
struct ParsedMemberTarget {
    compiled_targets: BTreeSet<CompilationUnitId>,
    warned_targets_by_dep: HashMap<String, BTreeSet<CompilationUnitId>>,
}

impl ParsedMemberTarget {
    fn unit_evidence(&self) -> Vec<CompilationUnitEvidence> {
        let mut by_unit: BTreeMap<CompilationUnitId, BTreeSet<String>> = self
            .compiled_targets
            .iter()
            .cloned()
            .map(|unit| (unit, BTreeSet::new()))
            .collect();
        for (dependency, units) in &self.warned_targets_by_dep {
            for unit in units {
                by_unit.entry(unit.clone()).or_default().insert(dependency.clone());
            }
        }
        by_unit
            .into_iter()
            .map(|(unit, unused_crates)| CompilationUnitEvidence { unit, unused_crates })
            .collect()
    }
}

fn parse_target_run(
    stdout: &str,
    workspace_root: &Path,
    package_to_member: &HashMap<String, String>,
    stale_members: &HashSet<&str>,
    candidates: &[CompilerCandidate],
) -> HashMap<String, ParsedMemberTarget> {
    let mut parsed: HashMap<String, ParsedMemberTarget> = HashMap::new();
    let mut warnings_by_target: HashMap<(String, CompilationUnitId), BTreeSet<String>> = HashMap::new();

    for line in stdout.lines() {
        let Ok(message) = serde_json::from_str::<CargoEvent>(line) else {
            continue;
        };
        if message.reason != "compiler-message" && message.reason != "compiler-artifact" {
            continue;
        }

        let Some(package_id) = message.package_id.as_deref() else {
            continue;
        };
        let Some(member_name) = package_to_member.get(package_id) else {
            continue;
        };
        if !stale_members.contains(member_name.as_str()) {
            continue;
        }

        let Some(target) = message.target.as_ref() else {
            continue;
        };
        if !is_relevant_target(target) {
            continue;
        }

        let base_target = target.identifier(workspace_root, false);
        if message.reason == "compiler-message" {
            let Some(diagnostic) = message.message.as_ref() else {
                continue;
            };
            if diagnostic.code.as_ref().and_then(|c| c.code.as_deref()) != Some("unused_crate_dependencies") {
                continue;
            }
            let Some(crate_name) = parse_unused_crate_name(&diagnostic.message) else {
                continue;
            };
            warnings_by_target
                .entry((member_name.clone(), base_target))
                .or_default()
                .insert(crate_name.replace('-', "_"));
            continue;
        }

        let target_id = target.identifier(
            workspace_root,
            message.profile.as_ref().is_some_and(|profile| profile.test),
        );
        let parsed_member = parsed.entry(member_name.clone()).or_default();
        parsed_member.compiled_targets.insert(target_id.clone());
    }

    // Cargo does not guarantee whether a diagnostic is emitted before or after
    // its artifact message. Correlate after consuming the stream by stable Cargo
    // target identity, then project it into the dependency's manifest domain.
    for ((member, base_target), warnings) in warnings_by_target {
        let Some(parsed_member) = parsed.get_mut(&member) else {
            continue;
        };
        let matching: Vec<_> = parsed_member
            .compiled_targets
            .iter()
            .filter(|unit| {
                unit.kind == base_target.kind && unit.name == base_target.name && unit.source == base_target.source
            })
            .cloned()
            .collect();
        for crate_name in &warnings {
            let kinds: BTreeSet<_> = candidates
                .iter()
                .filter(|candidate| candidate.member == member && candidate.crate_name == *crate_name)
                .map(|candidate| candidate.kind)
                .collect();
            for target_id in matching.iter().filter(|unit| {
                kinds.iter().any(|kind| match kind {
                    DepKind::Normal => !unit.test_mode && unit.kind != CargoTargetKind::CustomBuild,
                    DepKind::Dev => {
                        unit.test_mode
                            || matches!(
                                unit.kind,
                                CargoTargetKind::Test | CargoTargetKind::Example | CargoTargetKind::Benchmark
                            )
                    }
                    DepKind::Build => unit.kind == CargoTargetKind::CustomBuild,
                })
            }) {
                parsed_member
                    .warned_targets_by_dep
                    .entry(crate_name.clone())
                    .or_default()
                    .insert(target_id.clone());
            }
        }
    }

    parsed
}

fn parse_compilation_observations(
    stdout: &str,
    invocations: Vec<crate::compiler::observation::RawCompilerInvocation>,
    identity: &CompilerCacheIdentity,
    requested_target: &str,
) -> RailResult<Vec<CompilationObservationManifest>> {
    let source_root = &identity.observation_context.source_root;
    let mut artifacts = Vec::new();
    let mut build_script_outputs = HashMap::<String, CargoBuildScriptOutput>::new();
    let mut build_scripts = Vec::new();
    let mut observed_package_identities = identity.package_observation_identities.clone();
    for message in Message::parse_stream(BufReader::new(stdout.as_bytes())) {
        let message = message
            .map_err(|error| RailError::message(format!("failed to parse stable Cargo JSON message: {error}")))?;
        if let Message::BuildScriptExecuted(script) = message {
            build_scripts.push(script);
            continue;
        }
        let Message::CompilerArtifact(artifact) = message else {
            continue;
        };
        let mut bypasses = BTreeSet::new();
        let package = observed_package_identities
            .get(&artifact.package_id)
            .cloned()
            .or_else(|| {
                crate::utils::canonicalize_existing(artifact.manifest_path.as_std_path())
                    .ok()
                    .and_then(|manifest| identity.package_observation_manifests.get(&manifest).cloned())
            })
            .unwrap_or_else(|| {
                bypasses.insert("cargo_package_identity_unavailable".to_string());
                format!("unknown:{}", artifact.package_id)
            });
        if !package.starts_with("local:") {
            continue;
        }
        observed_package_identities.insert(artifact.package_id.clone(), package.clone());
        let is_custom_build = artifact.target.kind.contains(&TargetKind::CustomBuild);
        let explicit_executable_path = artifact
            .executable
            .as_ref()
            .map(|path| ObservationPath::capture(path.as_std_path(), source_root, source_root));
        let mut outputs = Vec::new();
        for filename in &artifact.filenames {
            match FileObservation::capture(filename.as_std_path(), source_root, source_root) {
                Ok(file) => outputs.push(file),
                Err(_) => {
                    bypasses.insert("cargo_artifact_output_bytes_unavailable".to_string());
                }
            }
        }
        if let Some(executable) = &artifact.executable
            && explicit_executable_path
                .as_ref()
                .is_some_and(|path| !outputs.iter().any(|output| &output.path == path))
        {
            match FileObservation::capture(executable.as_std_path(), source_root, source_root) {
                Ok(file) => outputs.push(file),
                Err(_) => {
                    bypasses.insert("cargo_executable_output_bytes_unavailable".to_string());
                }
            }
        }
        outputs.sort();
        outputs.dedup();
        let executable = explicit_executable_path
            .as_ref()
            .and_then(|path| outputs.iter().find(|output| &output.path == path).cloned())
            .or_else(|| {
                if is_custom_build {
                    build_script_executable_output(&outputs, &artifact.target.name, std::env::consts::EXE_SUFFIX)
                } else {
                    None
                }
            });
        if is_custom_build && executable.is_none() {
            bypasses.insert("cargo_build_script_executable_output_unavailable".to_string());
        }
        artifacts.push(CargoArtifactObservation {
            package,
            target_kinds: artifact.target.kind.iter().map(ToString::to_string).collect(),
            target_name: artifact.target.name,
            crate_types: artifact.target.crate_types.iter().map(ToString::to_string).collect(),
            source: ObservationPath::capture(artifact.target.src_path.as_std_path(), source_root, source_root),
            profile: CompilationProfile {
                opt_level: artifact.profile.opt_level,
                debuginfo: artifact.profile.debuginfo.to_string(),
                debug_assertions: artifact.profile.debug_assertions,
                overflow_checks: artifact.profile.overflow_checks,
                test: artifact.profile.test,
            },
            features: artifact.features.into_iter().collect(),
            outputs,
            executable,
            fresh: artifact.fresh,
            bypasses,
        });
    }
    for script in build_scripts {
        if let Some(package) = observed_package_identities.get(&script.package_id)
            && package.starts_with("local:")
        {
            let summary = build_script_output_summary(&script);
            build_script_outputs
                .entry(package.clone())
                .and_modify(|output| *output = CargoBuildScriptOutput::Ambiguous)
                .or_insert_with(|| CargoBuildScriptOutput::One(summary));
        }
    }
    let mut manifests = build_manifests(
        invocations,
        artifacts,
        &identity.observation_context,
        requested_target,
        CompilerMode::Rustc,
    )?;
    attach_execution_identities(
        &mut manifests,
        &identity.rustc_executable,
        &identity.wrapper_chain,
        &identity.cache_wrapper,
        &identity.executable_bypasses,
    );
    attach_build_script_action_keys(&mut manifests, identity, requested_target)?;
    attach_build_script_results(&mut manifests, identity, &build_script_outputs);
    let result_bindings = manifests
        .iter()
        .filter(|manifest| {
            manifest.unit.target_kind == crate::compiler::observation::CompilationTargetKind::BuildScript
        })
        .map(|manifest| BuildScriptResultBinding {
            package: manifest.unit.package.clone(),
            action_key: manifest
                .build_script_action_key
                .as_ref()
                .and_then(crate::build_script::BuildScriptActionKeyAnalysis::key)
                .map(str::to_string),
            result_digest: manifest
                .build_script_result
                .as_ref()
                .and_then(crate::build_script::BuildScriptResultAnalysis::digest)
                .map(str::to_string),
        })
        .collect::<Vec<_>>();
    attach_build_script_result_dependencies(&mut manifests, &identity.package_dependencies, &result_bindings)?;
    Ok(manifests)
}

fn build_script_executable_output(
    outputs: &[FileObservation],
    target_name: &str,
    executable_suffix: &str,
) -> Option<FileObservation> {
    let expected_name = format!("{target_name}{executable_suffix}");
    let mut matches = outputs.iter().filter(|output| {
        let path = match &output.path {
            ObservationPath::Repository(path) | ObservationPath::Host(path) => path,
        };
        path.rsplit('/').next() == Some(expected_name.as_str())
    });
    let executable = matches.next()?.clone();
    matches.next().is_none().then_some(executable)
}

fn build_script_output_summary(script: &cargo_metadata::BuildScript) -> BuildScriptCargoOutputSummary {
    BuildScriptCargoOutputSummary {
        linked_libraries: script.linked_libs.len(),
        linked_paths: script.linked_paths.len(),
        cfgs: script.cfgs.len(),
        rustc_environment: script.env.len(),
        output_directory_reported: !script.out_dir.as_str().is_empty(),
    }
}

fn select_build_script_output(
    output: Option<&CargoBuildScriptOutput>,
) -> (Option<BuildScriptCargoOutputSummary>, &'static str) {
    match output {
        Some(CargoBuildScriptOutput::One(output)) => (
            Some(output.clone()),
            "cargo_build_script_execution_freshness_unavailable",
        ),
        Some(CargoBuildScriptOutput::Ambiguous) => (None, "cargo_build_script_output_ambiguous"),
        None => (None, "cargo_build_script_output_unavailable"),
    }
}

fn attach_build_script_action_keys(
    manifests: &mut [CompilationObservationManifest],
    identity: &CompilerCacheIdentity,
    requested_target: &str,
) -> RailResult<()> {
    for manifest in manifests {
        if manifest.unit.target_kind != crate::compiler::observation::CompilationTargetKind::BuildScript {
            continue;
        }
        let package = identity.build_script_packages.get(&manifest.unit.package);
        let source_inputs = manifest
            .declared_inputs
            .iter()
            .chain(&manifest.observed_reads)
            .cloned()
            .collect();
        let target = if requested_target == "default" {
            identity.host_triple.clone()
        } else {
            requested_target.to_string()
        };
        let inputs = BuildScriptActionInputs {
            compiled_artifact: manifest.executable_output.clone(),
            source_inputs,
            manifest_closure: package
                .and_then(|package| identity.manifest_fingerprints.get(&package.package_id).cloned()),
            lock_closure: Some(identity.lock_fingerprint.clone()),
            toolchain: Some(identity.toolchain_fingerprint.clone()),
            action_id: format!("build-script:{}", manifest.unit_identity),
            package: manifest.unit.package.clone(),
            arguments: Vec::new(),
            working_directory: package.map(|package| package.working_directory.clone()),
            host_target: identity.host_triple.clone(),
            target,
            target_identity: identity.target_fingerprints.get(requested_target).cloned(),
            role: manifest.unit.role,
            profile: manifest.unit.profile.clone(),
            features: manifest.unit.features.clone(),
            cfg: manifest.unit.cfg.clone(),
            configuration: Some(identity.cargo_config_fingerprint.clone()),
            environment: None,
            secret_environment: BTreeSet::new(),
            dependency_actions: BTreeSet::new(),
            dependency_results: None,
            executable_path: None,
            output_root: None,
            platform_identity: None,
        };
        manifest.build_script_action_key = Some(analyze_build_script_action_key(
            &identity.observation_context.source_root,
            inputs,
        )?);
    }
    Ok(())
}

fn attach_build_script_results(
    manifests: &mut [CompilationObservationManifest],
    identity: &CompilerCacheIdentity,
    cargo_outputs: &HashMap<String, CargoBuildScriptOutput>,
) {
    for manifest in manifests {
        if manifest.unit.target_kind != crate::compiler::observation::CompilationTargetKind::BuildScript {
            continue;
        }
        let (cargo_output, limitation) = select_build_script_output(cargo_outputs.get(&manifest.unit.package));
        manifest.build_script_result = Some(analyze_build_script_result(
            &identity.observation_context.source_root,
            BuildScriptResultInputs {
                instruction_stream: None,
                environment_reads: None,
                generated_outputs: None,
                execution: None,
                secret_capabilities: BTreeSet::new(),
                limitations: BTreeSet::from([limitation.to_string()]),
            },
            cargo_output,
        ));
    }
}

fn compiler_observation_miss_reason<'a>(
    observations: &'a [CompilationObservationManifest],
    workspace_root: &Path,
) -> Option<&'a str> {
    if observations.is_empty() {
        return Some("compilation_observations_absent");
    }
    if let Some(reason) = observations
        .iter()
        .flat_map(|manifest| manifest.bypasses.iter().map(String::as_str))
        .next()
    {
        return Some(reason);
    }
    for manifest in observations {
        if let Some(reason) = manifest.diagnostic_revalidation_reason(workspace_root) {
            return Some(reason);
        }
    }
    None
}

fn reconcile_exact_artifact_observations(
    observations: &mut [CompilationObservationManifest],
    retained: &mut HashMap<String, CompilationObservationManifest>,
) {
    for observation in observations {
        let Some(identity) = observation.cargo_artifact_identity.clone() else {
            continue;
        };
        if observation.has_bypass("rustc_invocation_unavailable") {
            if let Some(exact) = retained.get(&identity) {
                observation.clone_from(exact);
            }
        } else if !observation.has_bypass("cargo_artifact_unavailable")
            && !observation.has_bypass("dep_info_unavailable")
            && !observation.has_bypass("dep_info_path_unavailable")
        {
            retained.insert(identity, observation.clone());
        }
    }
}

/// Capture the exact native-cache toolchain identity for operator inspection.
pub(crate) fn native_cache_capability(snapshot: &WorkspaceSnapshot) -> RailResult<NativeToolchainCapability> {
    let executables = snapshot.executable_identities()?;
    capture_native_toolchain_capability(snapshot.toolchain(), executables)
}

fn capture_native_toolchain_capability(
    toolchain: &ToolchainIdentity,
    executables: &ToolchainExecutableIdentities,
) -> RailResult<NativeToolchainCapability> {
    fn implementation_digest<'a>(executable: Option<&'a ExecutableIdentity>, name: &str) -> RailResult<&'a str> {
        executable.map(ExecutableIdentity::content_digest).ok_or_else(|| {
            RailError::message(format!(
                "native-cache capability cannot resolve the sysroot {name} implementation"
            ))
        })
    }

    let platform = format!(
        "{}-{}-{}",
        std::env::consts::FAMILY,
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let rustc_content_digest = implementation_digest(executables.rustc_implementation(), "rustc")?.to_string();
    let memo_path = compiler_sysroot_memo_path(toolchain.rustc_sysroot(), toolchain.host_target(), None);
    let (sysroot_identity, _) =
        compiler_sysroot_fingerprint(toolchain.rustc_sysroot(), toolchain.host_target(), memo_path.as_deref())?;

    let mut framed = Vec::from(&b"cargo-rail-native-toolchain-capability-v1\0"[..]);
    append_identity_frame(
        &mut framed,
        b"cache-class",
        crate::compiler::native_cache::native_cache_class().as_bytes(),
    );
    append_identity_frame(
        &mut framed,
        b"execution-contract",
        crate::compiler::native_cache::native_cache_execution_contract().as_bytes(),
    );
    // A rustc result is owned by the exact compiler capability and invocation.
    // Cargo's decisions are already present in the captured argv and environment;
    // rustdoc is not graduated; Cargo-Rail compatibility belongs to the versioned
    // execution contract. Binding those executable bytes would only partition
    // equivalent compiler work built or installed on different machines.
    append_identity_frame(&mut framed, b"platform", platform.as_bytes());
    append_identity_frame(&mut framed, b"host-target", toolchain.host_target().as_bytes());
    append_identity_frame(
        &mut framed,
        b"rustc-version",
        toolchain.rustc_verbose_version().as_bytes(),
    );
    append_identity_frame(&mut framed, b"rustc-content", rustc_content_digest.as_bytes());
    append_identity_frame(&mut framed, b"compiler-sysroot", sysroot_identity.as_bytes());
    let identity = format!("sha256:{}", ContentDigest::sha256(&framed));
    Ok(NativeToolchainCapability {
        schema_version: crate::compiler::native_cache::native_cache_capability_schema_version(),
        cache_class: crate::compiler::native_cache::native_cache_class(),
        execution_contract: crate::compiler::native_cache::native_cache_execution_contract(),
        transported_work_boundary: crate::compiler::native_cache::native_cache_transported_work_boundary(),
        platform,
        host_target: toolchain.host_target().to_string(),
        rustc_verbose_version: toolchain.rustc_verbose_version().to_string(),
        rustc_content_digest,
        sysroot_identity,
        identity,
    })
}

fn executable_toolchain_fingerprint(
    toolchain: &ToolchainIdentity,
    executables: &ToolchainExecutableIdentities,
    cargo_rail_executable: &ExecutableIdentity,
) -> RailResult<String> {
    let memo_path = compiler_sysroot_memo_path(toolchain.rustc_sysroot(), toolchain.host_target(), None);
    let (sysroot_identity, _) =
        compiler_sysroot_fingerprint(toolchain.rustc_sysroot(), toolchain.host_target(), memo_path.as_deref())?;
    let mut framed = Vec::from(&b"cargo-rail-executable-toolchain-v3\0"[..]);
    append_identity_frame(&mut framed, b"executables", &executables.identity_bytes()?);
    append_identity_frame(
        &mut framed,
        b"cargo-version",
        toolchain.cargo_verbose_version().as_bytes(),
    );
    append_identity_frame(
        &mut framed,
        b"rustc-version",
        toolchain.rustc_verbose_version().as_bytes(),
    );
    append_identity_frame(
        &mut framed,
        b"rustdoc-version",
        toolchain.rustdoc_verbose_version().as_bytes(),
    );
    append_identity_frame(&mut framed, b"host-target", toolchain.host_target().as_bytes());
    append_identity_frame(&mut framed, b"platform-family", std::env::consts::FAMILY.as_bytes());
    append_identity_frame(&mut framed, b"platform-os", std::env::consts::OS.as_bytes());
    append_identity_frame(&mut framed, b"platform-arch", std::env::consts::ARCH.as_bytes());
    append_identity_frame(&mut framed, b"compiler-sysroot", sysroot_identity.as_bytes());
    append_identity_frame(
        &mut framed,
        b"native-runtime-contract",
        b"exact-rust-distribution-and-host-platform-v1",
    );
    append_identity_frame(
        &mut framed,
        b"cargo-rail-diagnostic-wrapper",
        &cargo_rail_executable.identity_bytes()?,
    );
    append_identity_frame(
        &mut framed,
        b"compiler-cache-disposition",
        b"transparent-cache-owned-by-cargo-configuration",
    );
    Ok(format!("sha256:{}", ContentDigest::sha256(&framed)))
}

fn compiler_evidence_executable_bypasses(
    executables: &ToolchainExecutableIdentities,
    cargo_rail_executable: &ExecutableIdentity,
    verified_installed_rustc_wrapper: bool,
) -> BTreeSet<String> {
    executables
        .limitations()
        .filter(|limitation| {
            !rust_distribution_native_runtime_limitation(limitation)
                && !(verified_installed_rustc_wrapper
                    && *limitation == "rustc_wrapper_dynamic_executable_inputs_unavailable")
        })
        .map(str::to_string)
        .chain(
            cargo_rail_executable
                .limitations()
                .filter(|limitation| *limitation != "dynamic_executable_inputs_unavailable")
                .map(|limitation| format!("compiler_wrapper_{limitation}")),
        )
        .collect()
}

fn rust_distribution_native_runtime_limitation(limitation: &str) -> bool {
    limitation
        .strip_suffix("_dynamic_executable_inputs_unavailable")
        .is_some_and(|role| {
            matches!(
                role,
                "cargo"
                    | "rustc"
                    | "rustdoc"
                    | "cargo_implementation"
                    | "rustc_implementation"
                    | "rustdoc_implementation"
            )
        })
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
const SYSROOT_MEMO_VERSION: u32 = 3;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
const MAX_SYSROOT_MEMO_BYTES: u64 = 4 * 1024 * 1024;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
const MAX_GENERATION_IDENTIFIER_BYTES: usize = 256;
#[cfg(any(windows, test))]
const WINDOWS_SYSROOT_CAPTURE_ATTEMPTS: usize = 3;
#[cfg(any(windows, test))]
const WINDOWS_SYSROOT_GENERATION_SETTLE_INTERVAL: Duration = Duration::from_millis(20);
#[cfg(windows)]
const WINDOWS_FILETIME_UNIX_EPOCH_SECONDS: u64 = 11_644_473_600;
#[cfg(windows)]
const WINDOWS_FILETIME_TICKS_PER_SECOND: u64 = 10_000_000;
const MAX_SYSROOT_FILES: usize = 4096;
// rustc-dev is an input to runtime fact-driver manufacturing and expands some
// supported host sysroots beyond 1 GiB. Keep the identity operation bounded,
// but size that bound for the complete supported compiler inventory.
const MAX_SYSROOT_BYTES: u64 = 2 * 1024 * 1024 * 1024;

struct CompilerSysrootInventory {
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    root: PathBuf,
    files: Vec<(String, PathBuf)>,
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    evidence_locations: Vec<SysrootEvidenceLocation>,
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SysrootEvidenceKind {
    Directory,
    File,
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
struct SysrootEvidenceLocation {
    kind: SysrootEvidenceKind,
    relative_path: String,
    physical_path: PathBuf,
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SysrootChangeEvidence {
    kind: SysrootEvidenceKind,
    relative_path: String,
    generation_identifier: Vec<u8>,
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactSysrootEvidence {
    volume_identifier: Vec<u8>,
    entries: Vec<SysrootChangeEvidence>,
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SysrootIdentityMemo {
    version: u32,
    sysroot: String,
    host_target: String,
    fingerprint: String,
    volume_identifier: Vec<u8>,
    entries: Vec<SysrootChangeEvidence>,
    memo_digest: String,
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn compiler_sysroot_memo_lookup(sysroot: &Path, host_target: &str) -> Option<ContentDigest> {
    let sysroot = crate::utils::canonicalize_existing(sysroot).ok()?;
    let mut framed = Vec::from(&b"cargo-rail-compiler-sysroot-memo-location-v1\0"[..]);
    append_identity_frame(&mut framed, b"sysroot", sysroot.as_os_str().as_encoded_bytes());
    append_identity_frame(&mut framed, b"host-target", host_target.as_bytes());
    Some(ContentDigest::sha256(&framed))
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn compiler_sysroot_memo_path(
    sysroot: &Path,
    host_target: &str,
    selection: Option<&LocalCacheSelection>,
) -> Option<PathBuf> {
    let lookup = compiler_sysroot_memo_lookup(sysroot, host_target)?;
    selection
        .map_or_else(LocalCas::open, LocalCas::open_initialized_selected)
        .ok()
        .map(|cas| cas.sysroot_identity_memo_path(&lookup))
}

/// Select the sysroot identity memo owned by one already open local cache.
///
/// The distributed client reaches this after the native session has already
/// established the same fact in the same process, so reusing the caller's cache
/// avoids opening a second one and avoids rehashing the whole sysroot.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn compiler_sysroot_memo_path_in(cas: &LocalCas, sysroot: &Path, host_target: &str) -> Option<PathBuf> {
    compiler_sysroot_memo_lookup(sysroot, host_target).map(|lookup| cas.sysroot_identity_memo_path(&lookup))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn compiler_sysroot_memo_path(
    _sysroot: &Path,
    _host_target: &str,
    _selection: Option<&LocalCacheSelection>,
) -> Option<PathBuf> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub(crate) fn compiler_sysroot_memo_path_in(_cas: &LocalCas, _sysroot: &Path, _host_target: &str) -> Option<PathBuf> {
    None
}

pub(crate) fn compiler_sysroot_fingerprint(
    sysroot: &Path,
    host_target: &str,
    memo_path: Option<&Path>,
) -> RailResult<(String, u64)> {
    let _sysroot_fingerprinting_phase = crate::instrumentation::sysroot_fingerprinting_phase();
    let inventory = compiler_sysroot_inventory(sysroot, host_target)?;

    #[cfg(windows)]
    let windows_before = retry_unstable_windows_sysroot_capture(|| Ok(capture_exact_sysroot_evidence(&inventory)))?;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Some(memo_path) = memo_path
        && let Some(memo) = load_sysroot_identity_memo(memo_path, &inventory, host_target)
        && let Some(before) = capture_exact_sysroot_evidence(&inventory)
        && before.volume_identifier == memo.volume_identifier
        && before.entries == memo.entries
        && capture_exact_sysroot_evidence(&inventory).as_ref() == Some(&before)
    {
        return Ok((memo.fingerprint, 0));
    }

    #[cfg(windows)]
    if let Some(memo_path) = memo_path
        && let Some(memo) = load_sysroot_identity_memo(memo_path, &inventory, host_target)
        && windows_before.volume_identifier == memo.volume_identifier
        && windows_before.entries == memo.entries
        && capture_exact_sysroot_evidence(&inventory).as_ref() == Some(&windows_before)
    {
        return Ok((memo.fingerprint, 0));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let before = memo_path.and_then(|_| capture_exact_sysroot_evidence(&inventory));
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    let _ = memo_path;
    #[cfg(not(windows))]
    let fingerprint = hash_compiler_sysroot(&inventory)?;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let (Some(memo_path), Some(before)) = (memo_path, before)
        && let Ok(after_inventory) = compiler_sysroot_inventory(sysroot, host_target)
        && inventory.files == after_inventory.files
        && let Some(after) = capture_exact_sysroot_evidence(&after_inventory)
        && before == after
    {
        publish_sysroot_identity_memo(memo_path, &after_inventory, host_target, &fingerprint.0, after);
    }

    #[cfg(windows)]
    {
        // NTFS may finish a benign metadata update while the freshly installed sysroot is first inspected. Rehash the
        // whole inventory after drift; accepting only one fully bracketed attempt preserves the exact-byte claim.
        let mut retry_inventory = inventory;
        let mut retry_before = windows_before;
        let (fingerprint, stable_inventory, stable_evidence) = retry_unstable_windows_sysroot_capture(|| {
            let fingerprint = hash_compiler_sysroot(&retry_inventory)?;
            let after_inventory = compiler_sysroot_inventory(sysroot, host_target)?;
            let Some(after) = capture_exact_sysroot_evidence(&after_inventory) else {
                return Ok(None);
            };
            if retry_inventory.files != after_inventory.files || retry_before != after {
                retry_inventory = after_inventory;
                retry_before = after;
                return Ok(None);
            }
            Ok(Some((fingerprint, after_inventory, after)))
        })?;
        if let Some(memo_path) = memo_path {
            publish_sysroot_identity_memo(
                memo_path,
                &stable_inventory,
                host_target,
                &fingerprint.0,
                stable_evidence,
            );
        }
        Ok(fingerprint)
    }

    #[cfg(not(windows))]
    Ok(fingerprint)
}

#[cfg(any(windows, test))]
fn retry_unstable_windows_sysroot_capture<T>(mut capture: impl FnMut() -> RailResult<Option<T>>) -> RailResult<T> {
    for attempt in 0..WINDOWS_SYSROOT_CAPTURE_ATTEMPTS {
        if let Some(captured) = capture()? {
            return Ok(captured);
        }
        if attempt + 1 < WINDOWS_SYSROOT_CAPTURE_ATTEMPTS {
            std::thread::sleep(WINDOWS_SYSROOT_GENERATION_SETTLE_INTERVAL);
        }
    }
    Err(RailError::message("compiler sysroot changed during identity capture"))
}

fn compiler_sysroot_inventory(sysroot: &Path, host_target: &str) -> RailResult<CompilerSysrootInventory> {
    let sysroot = crate::utils::canonicalize_existing(sysroot)?;
    let rustlib = sysroot.join("lib/rustlib").join(host_target);
    let target_lib = rustlib.join("lib");
    #[cfg(windows)]
    let driver_lib = sysroot.join("bin");
    #[cfg(not(windows))]
    let driver_lib = sysroot.join("lib");
    validate_sysroot_directory(&rustlib)?;
    validate_sysroot_directory(&target_lib)?;
    validate_sysroot_directory(&driver_lib)?;

    let mut files = Vec::new();
    let mut self_contained_directory = None;
    for entry in std::fs::read_dir(&target_lib)? {
        let path = entry?.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.is_file() && !crate::utils::is_symlink_or_reparse(&metadata) {
            files.push(path);
            continue;
        }
        if path.file_name() == Some(OsStr::new("self-contained"))
            && metadata.is_dir()
            && !crate::utils::is_symlink_or_reparse(&metadata)
        {
            for entry in std::fs::read_dir(&path)? {
                let runtime = entry?.path();
                let metadata = std::fs::symlink_metadata(&runtime)?;
                if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) {
                    return Err(RailError::message(
                        "compiler self-contained sysroot contains a non-regular entry",
                    ));
                }
                files.push(runtime);
            }
            self_contained_directory = Some(path);
            continue;
        }
        return Err(RailError::message(
            "compiler target sysroot contains an unsupported non-regular entry",
        ));
    }
    let mut driver_files = 0usize;
    for entry in std::fs::read_dir(&driver_lib)? {
        let path = entry?.path();
        let name = path.file_name().and_then(std::ffi::OsStr::to_str).unwrap_or_default();
        let metadata = std::fs::symlink_metadata(&path)?;
        if rustc_driver_library(name) {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(RailError::message(
                    "compiler driver sysroot entry is not a regular file",
                ));
            }
            files.push(path);
            driver_files += 1;
        }
    }
    let codegen_backends = rustlib.join("codegen-backends");
    match std::fs::symlink_metadata(&codegen_backends) {
        Ok(metadata) => {
            if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
                return Err(RailError::message(
                    "compiler codegen backend sysroot entry is not a real directory",
                ));
            }
            for entry in std::fs::read_dir(&codegen_backends)? {
                let path = entry?.path();
                let metadata = std::fs::symlink_metadata(&path)?;
                if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) {
                    return Err(RailError::message("compiler codegen backend is not a regular file"));
                }
                files.push(path);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    files.sort();
    if driver_files == 0 || files.len() > MAX_SYSROOT_FILES {
        return Err(RailError::message(
            "compiler sysroot has no bounded host library inventory",
        ));
    }

    let files = files
        .into_iter()
        .map(|path| {
            let relative = sysroot_relative_path(&sysroot, &path)?;
            Ok((relative, path))
        })
        .collect::<RailResult<Vec<_>>>()?;

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    let evidence_locations = {
        #[cfg(windows)]
        let rustc_implementation = sysroot.join("bin/rustc.exe");
        #[cfg(not(windows))]
        let rustc_implementation = sysroot.join("bin/rustc");
        let mut locations = vec![
            evidence_location(&sysroot, &sysroot, SysrootEvidenceKind::Directory)?,
            evidence_location(&sysroot, &rustlib, SysrootEvidenceKind::Directory)?,
            evidence_location(&sysroot, &target_lib, SysrootEvidenceKind::Directory)?,
            evidence_location(&sysroot, &driver_lib, SysrootEvidenceKind::Directory)?,
            evidence_location(&sysroot, &rustc_implementation, SysrootEvidenceKind::File)?,
        ];
        if codegen_backends.is_dir() {
            locations.push(evidence_location(
                &sysroot,
                &codegen_backends,
                SysrootEvidenceKind::Directory,
            )?);
        }
        if let Some(self_contained) = &self_contained_directory {
            locations.push(evidence_location(
                &sysroot,
                self_contained,
                SysrootEvidenceKind::Directory,
            )?);
        }
        locations.extend(
            files
                .iter()
                .map(|(relative_path, physical_path)| SysrootEvidenceLocation {
                    kind: SysrootEvidenceKind::File,
                    relative_path: relative_path.clone(),
                    physical_path: physical_path.clone(),
                }),
        );
        locations.sort_by(|left, right| (&left.kind, &left.relative_path).cmp(&(&right.kind, &right.relative_path)));
        locations.dedup_by(|left, right| left.kind == right.kind && left.relative_path == right.relative_path);
        locations
    };

    Ok(CompilerSysrootInventory {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        root: sysroot,
        files,
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        evidence_locations,
    })
}

fn validate_sysroot_directory(path: &Path) -> RailResult<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.is_dir() && !crate::utils::is_symlink_or_reparse(&metadata) {
        return Ok(());
    }
    Err(RailError::message(format!(
        "compiler sysroot path '{}' is not a real directory",
        path.display()
    )))
}

fn sysroot_relative_path(sysroot: &Path, path: &Path) -> RailResult<String> {
    let relative = path
        .strip_prefix(sysroot)
        .map_err(|_| RailError::message("compiler sysroot entry escaped its root"))?;
    if relative.as_os_str().is_empty() {
        return Ok(".".to_string());
    }
    relative
        .to_str()
        .map(|path| path.replace('\\', "/"))
        .ok_or_else(|| RailError::message("compiler sysroot entry is not valid UTF-8"))
}

fn hash_compiler_sysroot(inventory: &CompilerSysrootInventory) -> RailResult<(String, u64)> {
    hash_compiler_sysroot_with_limit(inventory, MAX_SYSROOT_BYTES)
}

fn hash_compiler_sysroot_with_limit(
    inventory: &CompilerSysrootInventory,
    maximum_bytes: u64,
) -> RailResult<(String, u64)> {
    let mut total = 0u64;
    let mut framed = Vec::from(&b"cargo-rail-compiler-sysroot-v1\0"[..]);
    for (relative, path) in &inventory.files {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) {
            return Err(RailError::message(
                "compiler sysroot entry changed type during identity capture",
            ));
        }
        total = total.saturating_add(metadata.len());
        if total > maximum_bytes {
            return Err(RailError::message(format!(
                "compiler sysroot identity exceeds its {maximum_bytes}-byte limit after {total} bytes"
            )));
        }
        let mut file = File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut bytes_read = 0u64;
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            bytes_read = bytes_read.saturating_add(read as u64);
            if bytes_read > metadata.len() {
                return Err(RailError::message("compiler sysroot changed during identity capture"));
            }
            hasher.update(&buffer[..read]);
        }
        if bytes_read != metadata.len() {
            return Err(RailError::message("compiler sysroot changed during identity capture"));
        }
        crate::instrumentation::record_hash_operation();
        crate::instrumentation::record_hash_input_bytes(usize::try_from(bytes_read).unwrap_or(usize::MAX));
        crate::instrumentation::record_hashed_file_bytes_read(usize::try_from(bytes_read).unwrap_or(usize::MAX));
        let digest = ContentDigest::from_sha256_bytes(hasher.finalize());
        append_identity_frame(&mut framed, relative.as_bytes(), digest.to_string().as_bytes());
    }
    append_identity_frame(&mut framed, b"bytes", &total.to_le_bytes());
    Ok((format!("sha256:{}", ContentDigest::sha256(&framed)), total))
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn evidence_location(sysroot: &Path, path: &Path, kind: SysrootEvidenceKind) -> RailResult<SysrootEvidenceLocation> {
    Ok(SysrootEvidenceLocation {
        kind,
        relative_path: sysroot_relative_path(sysroot, path)?,
        physical_path: path.to_path_buf(),
    })
}

#[cfg(target_os = "macos")]
fn capture_exact_sysroot_evidence(inventory: &CompilerSysrootInventory) -> Option<ExactSysrootEvidence> {
    use std::os::macos::fs::MetadataExt as _;

    let root = std::fs::symlink_metadata(&inventory.root).ok()?;
    if !root.is_dir() || crate::utils::is_symlink_or_reparse(&root) {
        return None;
    }
    let volume_identifier = root.st_dev().to_le_bytes().to_vec();
    let mut entries = Vec::with_capacity(inventory.evidence_locations.len());
    for location in &inventory.evidence_locations {
        let metadata = std::fs::symlink_metadata(&location.physical_path).ok()?;
        let valid_kind = match location.kind {
            SysrootEvidenceKind::Directory => metadata.is_dir(),
            SysrootEvidenceKind::File => metadata.is_file(),
        };
        if !valid_kind || crate::utils::is_symlink_or_reparse(&metadata) {
            return None;
        }
        let mut generation_identifier = Vec::from(&b"macos-stat-v1\0"[..]);
        for value in [
            metadata.st_dev(),
            metadata.st_ino(),
            u64::from(metadata.st_mode()),
            metadata.st_nlink(),
            metadata.st_size(),
            u64::from_ne_bytes(metadata.st_mtime().to_ne_bytes()),
            u64::from_ne_bytes(metadata.st_mtime_nsec().to_ne_bytes()),
            u64::from_ne_bytes(metadata.st_ctime().to_ne_bytes()),
            u64::from_ne_bytes(metadata.st_ctime_nsec().to_ne_bytes()),
            u64::from_ne_bytes(metadata.st_birthtime().to_ne_bytes()),
            u64::from_ne_bytes(metadata.st_birthtime_nsec().to_ne_bytes()),
            u64::from(metadata.st_gen()),
        ] {
            generation_identifier.extend_from_slice(&value.to_le_bytes());
        }
        entries.push(SysrootChangeEvidence {
            kind: location.kind,
            relative_path: location.relative_path.clone(),
            generation_identifier,
        });
    }
    Some(ExactSysrootEvidence {
        volume_identifier,
        entries,
    })
}

#[cfg(target_os = "linux")]
fn capture_exact_sysroot_evidence(inventory: &CompilerSysrootInventory) -> Option<ExactSysrootEvidence> {
    use std::os::unix::fs::MetadataExt as _;

    let root = std::fs::symlink_metadata(&inventory.root).ok()?;
    if !root.is_dir() || crate::utils::is_symlink_or_reparse(&root) {
        return None;
    }
    let volume_identifier = root.dev().to_le_bytes().to_vec();
    let mut entries = Vec::with_capacity(inventory.evidence_locations.len());
    for location in &inventory.evidence_locations {
        let metadata = std::fs::symlink_metadata(&location.physical_path).ok()?;
        let valid_kind = match location.kind {
            SysrootEvidenceKind::Directory => metadata.is_dir(),
            SysrootEvidenceKind::File => metadata.is_file(),
        };
        if !valid_kind || crate::utils::is_symlink_or_reparse(&metadata) {
            return None;
        }
        let mut generation_identifier = Vec::from(&b"linux-stat-v1\0"[..]);
        for value in [
            metadata.dev(),
            metadata.ino(),
            metadata.mode() as u64,
            metadata.nlink(),
            metadata.size(),
            u64::from_ne_bytes(metadata.mtime().to_ne_bytes()),
            u64::from_ne_bytes(metadata.mtime_nsec().to_ne_bytes()),
            u64::from_ne_bytes(metadata.ctime().to_ne_bytes()),
            u64::from_ne_bytes(metadata.ctime_nsec().to_ne_bytes()),
        ] {
            generation_identifier.extend_from_slice(&value.to_le_bytes());
        }
        entries.push(SysrootChangeEvidence {
            kind: location.kind,
            relative_path: location.relative_path.clone(),
            generation_identifier,
        });
    }
    Some(ExactSysrootEvidence {
        volume_identifier,
        entries,
    })
}

#[cfg(windows)]
fn capture_exact_sysroot_evidence(inventory: &CompilerSysrootInventory) -> Option<ExactSysrootEvidence> {
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0010;

    let observed_at = windows_filetime_now()?;
    let settle_ticks = u64::try_from(WINDOWS_SYSROOT_GENERATION_SETTLE_INTERVAL.as_nanos() / 100).ok()?;
    let root = crate::windows_fs::open_for_observation(&inventory.root).ok()?;
    let root_observation = crate::windows_fs::observe_file(&root).ok()?;
    crate::windows_fs::prove_local_ntfs(&root, root_observation.volume_serial_number).ok()?;
    if root_observation.file_attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
        return None;
    }

    let volume_identifier = root_observation.volume_serial_number.to_le_bytes().to_vec();
    let mut entries = Vec::with_capacity(inventory.evidence_locations.len());
    for location in &inventory.evidence_locations {
        let file = crate::windows_fs::open_for_observation(&location.physical_path).ok()?;
        let observation = crate::windows_fs::observe_file(&file).ok()?;
        crate::windows_fs::prove_local_ntfs(&file, observation.volume_serial_number).ok()?;
        let current = crate::windows_fs::open_for_observation(&location.physical_path).ok()?;
        let current_observation = crate::windows_fs::observe_file(&current).ok()?;
        crate::windows_fs::prove_local_ntfs(&current, current_observation.volume_serial_number).ok()?;

        let is_directory = observation.file_attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
        let valid_kind = match location.kind {
            SysrootEvidenceKind::Directory => is_directory,
            SysrootEvidenceKind::File => !is_directory,
        };
        if !valid_kind
            || observation != current_observation
            || observation.volume_serial_number != root_observation.volume_serial_number
            || observation
                .change_time
                .checked_add(settle_ticks)
                .is_none_or(|settled_at| settled_at > observed_at)
        {
            return None;
        }

        let mut generation_identifier = Vec::from(&b"windows-ntfs-v1\0"[..]);
        for value in [
            observation.volume_serial_number,
            observation.file_id,
            observation.creation_time,
            observation.last_write_time,
            observation.change_time,
            u64::from(observation.file_attributes),
            observation.size,
            observation.number_of_links,
        ] {
            generation_identifier.extend_from_slice(&value.to_le_bytes());
        }
        entries.push(SysrootChangeEvidence {
            kind: location.kind,
            relative_path: location.relative_path.clone(),
            generation_identifier,
        });
    }
    Some(ExactSysrootEvidence {
        volume_identifier,
        entries,
    })
}

#[cfg(windows)]
fn windows_filetime_now() -> Option<u64> {
    let since_unix_epoch = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    since_unix_epoch
        .as_secs()
        .checked_add(WINDOWS_FILETIME_UNIX_EPOCH_SECONDS)?
        .checked_mul(WINDOWS_FILETIME_TICKS_PER_SECOND)?
        .checked_add(u64::from(since_unix_epoch.subsec_nanos()) / 100)
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn load_sysroot_identity_memo(
    path: &Path,
    inventory: &CompilerSysrootInventory,
    host_target: &str,
) -> Option<SysrootIdentityMemo> {
    #[cfg(windows)]
    let mut file = crate::windows_fs::open_for_stable_byte_observation(path).ok()?;
    #[cfg(not(windows))]
    let mut file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_SYSROOT_MEMO_BYTES {
        return None;
    }
    if !crate::utils::private_file_matches_path(&file, path, metadata.len()).ok()? {
        return None;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).ok()?);
    std::io::Read::take(&mut file, MAX_SYSROOT_MEMO_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 != metadata.len()
        || !crate::utils::private_file_matches_path(&file, path, metadata.len()).ok()?
    {
        return None;
    }
    let memo = serde_json::from_slice::<SysrootIdentityMemo>(&bytes).ok()?;
    let sysroot = inventory.root.to_str()?;
    if memo.version != SYSROOT_MEMO_VERSION
        || memo.sysroot != sysroot
        || memo.host_target != host_target
        || !valid_sha256_identity(&memo.fingerprint)
        || !valid_sha256_identity(&memo.memo_digest)
        || memo.memo_digest != sysroot_identity_memo_digest(&memo)
        || memo.volume_identifier.is_empty()
        || memo.volume_identifier.len() > MAX_GENERATION_IDENTIFIER_BYTES
        || memo.entries.len() != inventory.evidence_locations.len()
        || memo.entries.iter().any(|entry| {
            entry.relative_path.len() > 4096
                || entry.generation_identifier.is_empty()
                || entry.generation_identifier.len() > MAX_GENERATION_IDENTIFIER_BYTES
        })
        || !memo.entries.windows(2).all(|entries| entries[0] < entries[1])
    {
        return None;
    }
    Some(memo)
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn publish_sysroot_identity_memo(
    path: &Path,
    inventory: &CompilerSysrootInventory,
    host_target: &str,
    fingerprint: &str,
    evidence: ExactSysrootEvidence,
) {
    let Some(sysroot) = inventory.root.to_str() else {
        return;
    };
    let memo = SysrootIdentityMemo {
        version: SYSROOT_MEMO_VERSION,
        sysroot: sysroot.to_string(),
        host_target: host_target.to_string(),
        fingerprint: fingerprint.to_string(),
        volume_identifier: evidence.volume_identifier,
        entries: evidence.entries,
        memo_digest: String::new(),
    };
    let memo = SysrootIdentityMemo {
        memo_digest: sysroot_identity_memo_digest(&memo),
        ..memo
    };
    let Ok(bytes) = serde_json::to_vec(&memo) else {
        return;
    };
    if bytes.len() as u64 <= MAX_SYSROOT_MEMO_BYTES {
        drop(publish_regenerable_memo(path, &bytes));
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn publish_regenerable_memo(path: &Path, bytes: &[u8]) -> RailResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| RailError::message("compiler sysroot memo has no parent directory"))?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".cargo-rail-sysroot-memo-")
        .suffix(".tmp")
        .tempfile_in(parent)?;
    temporary.write_all(bytes)?;
    crate::utils::persist_regenerable_file_atomic(temporary, path)?;
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn sysroot_identity_memo_digest(memo: &SysrootIdentityMemo) -> String {
    let mut framed = Vec::from(&b"cargo-rail-compiler-sysroot-memo-v2\0"[..]);
    append_identity_frame(&mut framed, b"version", &memo.version.to_le_bytes());
    append_identity_frame(&mut framed, b"sysroot", memo.sysroot.as_bytes());
    append_identity_frame(&mut framed, b"host-target", memo.host_target.as_bytes());
    append_identity_frame(&mut framed, b"fingerprint", memo.fingerprint.as_bytes());
    append_identity_frame(&mut framed, b"volume-identifier", &memo.volume_identifier);
    for entry in &memo.entries {
        append_identity_frame(
            &mut framed,
            b"entry-kind",
            match entry.kind {
                SysrootEvidenceKind::Directory => b"directory",
                SysrootEvidenceKind::File => b"file",
            },
        );
        append_identity_frame(&mut framed, b"entry-path", entry.relative_path.as_bytes());
        append_identity_frame(&mut framed, b"entry-generation", &entry.generation_identifier);
    }
    format!("sha256:{}", ContentDigest::sha256(&framed))
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn valid_sha256_identity(identity: &str) -> bool {
    identity.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

#[cfg(windows)]
fn rustc_driver_library(name: &str) -> bool {
    name.starts_with("rustc_driver-") && name.ends_with(".dll")
}

#[cfg(not(windows))]
fn rustc_driver_library(name: &str) -> bool {
    name.starts_with("librustc_driver-") && (name.ends_with(".so") || name.ends_with(".dylib"))
}

fn package_observation_identities(snapshot: &WorkspaceSnapshot) -> RailResult<HashMap<PackageId, String>> {
    snapshot
        .base_resolution()
        .metadata()
        .packages
        .iter()
        .map(|package| {
            let identity = if let Some(source) = &package.source {
                let checksum = snapshot.lockfile().and_then(|lockfile| {
                    lockfile.packages().iter().find_map(|locked| {
                        (locked.name() == package.name.as_str()
                            && locked.version() == package.version.to_string()
                            && locked.source() == Some(source.repr.as_str()))
                        .then(|| locked.checksum())
                        .flatten()
                    })
                });
                format!(
                    "external:{}#{}@{}#{}",
                    source.repr,
                    package.name,
                    package.version,
                    checksum.unwrap_or("unverified")
                )
            } else {
                let snapshot_package = snapshot
                    .packages()
                    .iter()
                    .find(|candidate| candidate.id() == &package.id)
                    .ok_or_else(|| RailError::message(format!("snapshot is missing local package '{}'", package.id)))?;
                let manifest = snapshot_package.manifest_path().ok_or_else(|| {
                    RailError::message(format!("local package '{}' has no manifest identity", package.id))
                })?;
                format!("local:{}#{}@{}", manifest.as_str(), package.name, package.version)
            };
            Ok((package.id.clone(), identity))
        })
        .collect()
}

fn package_observation_manifest_identities(
    snapshot: &WorkspaceSnapshot,
    identities: &HashMap<PackageId, String>,
) -> RailResult<HashMap<PathBuf, String>> {
    snapshot
        .base_resolution()
        .metadata()
        .packages
        .iter()
        .filter(|package| package.source.is_none())
        .map(|package| {
            let manifest = crate::utils::canonicalize_existing(package.manifest_path.as_std_path())?;
            let identity = identities.get(&package.id).cloned().ok_or_else(|| {
                RailError::message(format!("local package '{}' has no observation identity", package.id))
            })?;
            Ok((manifest, identity))
        })
        .collect()
}

fn package_dependency_graph(
    snapshot: &WorkspaceSnapshot,
    identities: &HashMap<PackageId, String>,
) -> RailResult<HashMap<String, BTreeSet<String>>> {
    let mut graph = identities
        .values()
        .cloned()
        .map(|identity| (identity, BTreeSet::new()))
        .collect::<HashMap<_, _>>();
    let resolve = snapshot
        .base_resolution()
        .metadata()
        .resolve
        .as_ref()
        .ok_or_else(|| RailError::message("Cargo metadata omitted the resolved package graph"))?;
    for node in &resolve.nodes {
        let consumer = identities.get(&node.id).ok_or_else(|| {
            RailError::message(format!(
                "resolved package '{}' has no portable compiler-observation identity",
                node.id
            ))
        })?;
        let dependencies = graph
            .get_mut(consumer)
            .ok_or_else(|| RailError::message("portable package dependency graph lost its consumer"))?;
        for dependency in &node.deps {
            let identity = identities.get(&dependency.pkg).ok_or_else(|| {
                RailError::message(format!(
                    "resolved dependency '{}' has no portable compiler-observation identity",
                    dependency.pkg
                ))
            })?;
            dependencies.insert(identity.clone());
        }
    }
    Ok(graph)
}

fn build_script_package_contexts(
    snapshot: &WorkspaceSnapshot,
    observation_identities: &HashMap<PackageId, String>,
) -> RailResult<HashMap<String, BuildScriptPackageContext>> {
    let package_ids = snapshot
        .base_resolution()
        .metadata()
        .packages
        .iter()
        .filter(|package| package.source.is_none())
        .filter(|package| {
            package
                .targets
                .iter()
                .any(|target| target.kind.contains(&TargetKind::CustomBuild))
        })
        .map(|package| package.id.clone())
        .collect::<HashSet<_>>();
    snapshot
        .packages()
        .iter()
        .filter(|package| package_ids.contains(package.id()))
        .map(|package| {
            let observation_identity = observation_identities.get(package.id()).ok_or_else(|| {
                RailError::message(format!(
                    "local build-script package '{}' has no portable observation identity",
                    package.id()
                ))
            })?;
            let manifest = package.manifest_path().ok_or_else(|| {
                RailError::message(format!("local package '{}' has no manifest identity", package.id()))
            })?;
            let working_directory = manifest.as_path().parent().unwrap_or_else(|| Path::new(""));
            Ok((
                observation_identity.clone(),
                BuildScriptPackageContext {
                    package_id: package.id().clone(),
                    working_directory: format!("repository:{}", crate::utils::path_to_git_format(working_directory)),
                },
            ))
        })
        .collect()
}

fn target_fingerprints(snapshot: &WorkspaceSnapshot) -> RailResult<HashMap<String, String>> {
    let mut fingerprints = HashMap::new();
    for target in snapshot.targets() {
        let identity = format!(
            "sha256:{}",
            ContentDigest::sha256(&target.portable_snapshot_identity(snapshot.source_root())?)
        );
        fingerprints.insert(target_name(target).to_string(), identity.clone());
        if target.is_build_target() || (target.is_host() && !fingerprints.contains_key("default")) {
            fingerprints.insert("default".to_string(), identity);
        }
    }
    if !fingerprints.contains_key("default") {
        return Err(RailError::message(
            "compiler evidence snapshot contains no default build or host target identity",
        ));
    }
    Ok(fingerprints)
}

fn compiler_cache_bypass_reason(snapshot: &WorkspaceSnapshot) -> Option<CompilerCacheBypass> {
    if !snapshot.cargo_config().unmodeled_settings().is_empty() {
        return Some(CompilerCacheBypass::CargoConfiguration);
    }
    if snapshot
        .targets()
        .iter()
        .any(crate::cargo::resolution::TargetIdentity::uses_response_file_argument)
    {
        return Some(CompilerCacheBypass::ResponseFileConfiguration);
    }
    for package in &snapshot.base_resolution().metadata().packages {
        if package
            .targets
            .iter()
            .flat_map(|target| target.kind.iter())
            .any(|kind| *kind == TargetKind::CustomBuild)
        {
            return Some(CompilerCacheBypass::BuildScriptObservations);
        }
        if package
            .targets
            .iter()
            .flat_map(|target| target.kind.iter())
            .any(|kind| *kind == TargetKind::ProcMacro)
        {
            return Some(CompilerCacheBypass::ProcMacroObservations);
        }
    }
    snapshot
        .packages()
        .iter()
        .any(|package| package.source().is_some() && package.checksum().is_none())
        .then_some(CompilerCacheBypass::ExternalSourceDigest)
}

fn target_name(target: &crate::cargo::resolution::TargetIdentity) -> &str {
    match target.specification() {
        crate::cargo::resolution::TargetSpecificationIdentity::BuiltIn(name) => name,
        crate::cargo::resolution::TargetSpecificationIdentity::Custom(specification) => specification.name(),
    }
}

fn package_source_fingerprints(snapshot: &WorkspaceSnapshot) -> RailResult<HashMap<PackageId, String>> {
    let manifest_paths = snapshot
        .manifests()
        .iter()
        .map(|manifest| manifest.path())
        .collect::<BTreeSet<_>>();
    let mut package_roots = HashMap::new();
    let mut roots_by_package = HashMap::new();
    let mut identities = HashMap::new();
    for package in snapshot
        .packages()
        .iter()
        .filter(|package| package.package_root().is_some())
    {
        let root = package.package_root().ok_or_else(|| {
            RailError::message(format!(
                "compiler evidence package '{}' is not backed by local snapshot source",
                package.id()
            ))
        })?;
        package_roots.insert(root, package.id());
        roots_by_package.insert(package.id(), root);
        identities.insert(package.id(), Vec::from(&b"cargo-rail-compiler-source-v1\0"[..]));
    }

    for entry in snapshot.source().tree().entries() {
        if manifest_paths.contains(&entry.path) {
            continue;
        }
        let Some(package_id) = entry
            .path
            .as_path()
            .ancestors()
            .find_map(|ancestor| package_roots.get(ancestor).copied())
        else {
            continue;
        };
        let package_root = roots_by_package[package_id];
        let relative = entry.path.as_path().strip_prefix(package_root).map_err(|_| {
            RailError::message(format!(
                "source entry '{}' is outside package '{}' root",
                entry.path, package_id
            ))
        })?;
        let identity = identities.get_mut(package_id).ok_or_else(|| {
            RailError::message(format!(
                "compiler source identity is missing local package '{package_id}'"
            ))
        })?;
        append_identity_frame(identity, b"path", crate::utils::path_to_git_format(relative).as_bytes());
        match &entry.kind {
            SourceEntryKind::RegularFile { digest, executable } => {
                append_identity_frame(identity, b"kind", b"regular-file");
                append_identity_frame(identity, b"content", digest.as_bytes());
                append_identity_frame(identity, b"executable", &[u8::from(*executable)]);
            }
            SourceEntryKind::Symlink { target } => {
                append_identity_frame(identity, b"kind", b"symlink");
                append_identity_frame(identity, b"target", target.as_bytes());
            }
            SourceEntryKind::Deleted => {
                return Err(RailError::message(format!(
                    "compiler source identity contains deleted entry '{}'",
                    entry.path
                )));
            }
        }
    }
    Ok(identities
        .into_iter()
        .map(|(package_id, identity)| {
            (
                package_id.clone(),
                format!("sha256:{}", ContentDigest::sha256(&identity)),
            )
        })
        .collect())
}

fn declared_local_dependency_graph(snapshot: &WorkspaceSnapshot) -> RailResult<HashMap<PackageId, Vec<PackageId>>> {
    let local_packages = snapshot
        .packages()
        .iter()
        .filter(|package| package.source().is_none())
        .map(|package| package.id())
        .collect::<HashSet<_>>();
    let mut roots = HashMap::new();
    for package in &snapshot.base_resolution().metadata().packages {
        if !local_packages.contains(&package.id) {
            continue;
        }
        let root = package
            .manifest_path
            .as_std_path()
            .parent()
            .ok_or_else(|| RailError::message(format!("local package '{}' manifest has no parent", package.id)))?;
        let root = std::fs::canonicalize(root)
            .with_context(|| format!("resolving local package '{}' root for compiler evidence", package.id))?;
        if let Some(previous) = roots.insert(root.clone(), package.id.clone())
            && previous != package.id
        {
            return Err(RailError::message(format!(
                "local packages '{previous}' and '{}' share compiler input root '{}'",
                package.id,
                root.display()
            )));
        }
    }

    let mut graph = HashMap::new();
    for package in &snapshot.base_resolution().metadata().packages {
        if !local_packages.contains(&package.id) {
            continue;
        }
        let mut dependencies = BTreeSet::new();
        for dependency in &package.dependencies {
            let Some(path) = dependency.path.as_ref() else {
                for candidate in &snapshot.base_resolution().metadata().packages {
                    if local_packages.contains(&candidate.id)
                        && candidate.name == dependency.name
                        && dependency.req.matches(&candidate.version)
                    {
                        dependencies.insert(candidate.id.clone());
                    }
                }
                continue;
            };
            let root = std::fs::canonicalize(path.as_std_path()).with_context(|| {
                format!(
                    "resolving local dependency '{}' declared by '{}' for compiler evidence",
                    dependency.name, package.id
                )
            })?;
            let dependency_id = roots.get(&root).ok_or_else(|| {
                RailError::message(format!(
                    "local dependency '{}' declared by '{}' is absent from the captured package graph",
                    dependency.name, package.id
                ))
            })?;
            dependencies.insert(dependency_id.clone());
        }
        graph.insert(package.id.clone(), dependencies.into_iter().collect());
    }
    Ok(graph)
}

fn manifest_closure_fingerprints(
    snapshot: &WorkspaceSnapshot,
    dependencies: &HashMap<PackageId, Vec<PackageId>>,
) -> RailResult<HashMap<PackageId, String>> {
    let manifests = snapshot
        .manifests()
        .iter()
        .map(|manifest| (manifest.path(), manifest))
        .collect::<HashMap<_, _>>();
    let packages = snapshot
        .packages()
        .iter()
        .map(|package| (package.id(), package))
        .collect::<HashMap<_, _>>();
    let root_manifest = snapshot
        .manifests()
        .iter()
        .find(|manifest| manifest.path().as_path() == Path::new("Cargo.toml"));
    let mut fingerprints = HashMap::new();

    for member in snapshot
        .packages()
        .iter()
        .filter(|package| package.is_workspace_member())
    {
        let mut closure = BTreeMap::new();
        if let Some(manifest) = root_manifest {
            closure.insert(manifest.path(), manifest.digest());
        }
        for package_id in local_dependency_closure(member.id(), dependencies) {
            let package = packages.get(&package_id).ok_or_else(|| {
                RailError::message(format!(
                    "compiler manifest identity is missing local package '{package_id}'"
                ))
            })?;
            let manifest_path = package.manifest_path().ok_or_else(|| {
                RailError::message(format!(
                    "local dependency '{package_id}' has no logical manifest identity"
                ))
            })?;
            let manifest = manifests.get(manifest_path).ok_or_else(|| {
                RailError::message(format!(
                    "local dependency '{package_id}' manifest '{manifest_path}' is absent from the snapshot"
                ))
            })?;
            closure.insert(manifest.path(), manifest.digest());
        }

        let mut identity = Vec::from(&b"cargo-rail-compiler-manifest-closure-v1\0"[..]);
        for (path, digest) in closure {
            append_identity_frame(&mut identity, b"path", path.as_str().as_bytes());
            append_identity_frame(&mut identity, b"content", digest.as_bytes());
        }
        fingerprints.insert(
            member.id().clone(),
            format!("sha256:{}", ContentDigest::sha256(&identity)),
        );
    }
    Ok(fingerprints)
}

fn source_closure_fingerprints(
    snapshot: &WorkspaceSnapshot,
    dependencies: &HashMap<PackageId, Vec<PackageId>>,
) -> RailResult<HashMap<PackageId, String>> {
    let package_sources = package_source_fingerprints(snapshot)?;
    let packages = snapshot
        .packages()
        .iter()
        .map(|package| (package.id(), package))
        .collect::<HashMap<_, _>>();
    let mut fingerprints = HashMap::new();

    for member in snapshot
        .packages()
        .iter()
        .filter(|package| package.is_workspace_member())
    {
        let mut closure = BTreeMap::new();
        for package_id in local_dependency_closure(member.id(), dependencies) {
            let package = packages.get(&package_id).ok_or_else(|| {
                RailError::message(format!(
                    "compiler source identity is missing local package '{package_id}'"
                ))
            })?;
            let source_fingerprint = package_sources.get(&package_id).ok_or_else(|| {
                RailError::message(format!(
                    "local dependency '{package_id}' source is absent from the authoritative snapshot"
                ))
            })?;
            let manifest = package.manifest_path().ok_or_else(|| {
                RailError::message(format!(
                    "local dependency '{package_id}' has no logical manifest identity"
                ))
            })?;
            closure.insert(manifest, source_fingerprint);
        }

        let mut identity = Vec::from(&b"cargo-rail-compiler-source-closure-v1\0"[..]);
        for (manifest, source_fingerprint) in closure {
            append_identity_frame(&mut identity, b"manifest", manifest.as_str().as_bytes());
            append_identity_frame(&mut identity, b"source", source_fingerprint.as_bytes());
        }
        fingerprints.insert(
            member.id().clone(),
            format!("sha256:{}", ContentDigest::sha256(&identity)),
        );
    }
    Ok(fingerprints)
}

fn local_dependency_closure(
    root: &PackageId,
    dependencies: &HashMap<PackageId, Vec<PackageId>>,
) -> BTreeSet<PackageId> {
    let mut pending = vec![root.clone()];
    let mut visited = BTreeSet::new();
    while let Some(package_id) = pending.pop() {
        if !visited.insert(package_id.clone()) {
            continue;
        }
        if let Some(package_dependencies) = dependencies.get(&package_id) {
            pending.extend(package_dependencies.iter().cloned());
        }
    }
    visited
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn compiler_env_fingerprint(cargo_config: &CargoConfigSnapshot) -> RailResult<String> {
    let mut framed = Vec::from(&b"cargo-rail-native-compiler-environment-v2\0"[..]);
    append_identity_frame(&mut framed, b"cargo", &serde_json::to_vec(cargo_config.environment())?);
    let runtime = std::env::vars_os()
        .filter_map(|(name, value)| {
            let name = name.into_string().ok()?;
            compiler_diagnostics_runtime_environment(&name).then(|| {
                (
                    name,
                    format!("sha256:{}", ContentDigest::sha256(value.as_encoded_bytes())),
                )
            })
        })
        .collect::<BTreeMap<_, _>>();
    append_identity_frame(&mut framed, b"runtime", &serde_json::to_vec(&runtime)?);
    Ok(format!("sha256:{}", ContentDigest::sha256(&framed)))
}

fn transparent_native_compiler_process_env_fingerprint(target_root: &Path) -> RailResult<String> {
    let target_root = crate::utils::canonicalize_existing(target_root)?;
    let runtime = std::env::vars_os()
        .filter_map(|(name, value)| {
            let name = name.into_string().ok()?;
            native_compiler_process_environment(&name).then(|| {
                (
                    name.clone(),
                    Some(format!(
                        "sha256:{}",
                        ContentDigest::sha256(&transparent_native_environment_value(&name, &value, &target_root,)),
                    )),
                )
            })
        })
        .collect::<BTreeMap<_, _>>();
    #[cfg(unix)]
    let default_regular_file_mode = transparent_default_regular_file_creation_mode();
    #[cfg(not(unix))]
    let default_regular_file_mode = 0o644_u32;
    let mut framed = Vec::from(&b"cargo-rail-native-compiler-process-environment-v3\0"[..]);
    append_identity_frame(&mut framed, b"runtime", &serde_json::to_vec(&runtime)?);
    append_identity_frame(
        &mut framed,
        b"default-regular-file-mode",
        &default_regular_file_mode.to_le_bytes(),
    );
    Ok(format!("sha256:{}", ContentDigest::sha256(&framed)))
}

fn transparent_native_environment_value(name: &str, value: &OsStr, target_root: &Path) -> Vec<u8> {
    if !matches!(
        name,
        "DYLD_FALLBACK_LIBRARY_PATH"
            | "DYLD_LIBRARY_PATH"
            | "LD_LIBRARY_PATH"
            | "LD_RUN_PATH"
            | "LIBRARY_PATH"
            | "LPATH"
    ) {
        return value.as_encoded_bytes().to_vec();
    }

    let mut framed = Vec::from(&b"cargo-rail-native-search-path-v1\0"[..]);
    for component in std::env::split_paths(value) {
        let relative = component
            .strip_prefix(target_root)
            .ok()
            .map(Path::to_path_buf)
            .or_else(|| {
                crate::utils::canonicalize_existing(&component)
                    .ok()
                    .and_then(|canonical| canonical.strip_prefix(target_root).ok().map(Path::to_path_buf))
            });
        if let Some(relative) = relative {
            append_identity_frame(&mut framed, b"selected-target", relative.as_os_str().as_encoded_bytes());
        } else {
            append_identity_frame(&mut framed, b"external", component.as_os_str().as_encoded_bytes());
        }
    }
    framed
}

#[cfg(unix)]
fn transparent_default_regular_file_creation_mode() -> u32 {
    // The pre-Clap compiler wrapper is single-threaded at this boundary. Reading
    // and restoring umask avoids a filesystem transaction for every rustc unit
    // without exposing process-global mutation to another thread.
    let current = rustix::process::umask(rustix::fs::Mode::empty());
    let _ = rustix::process::umask(current);
    #[cfg(target_os = "macos")]
    let current_bits = u32::from(current.bits());
    #[cfg(not(target_os = "macos"))]
    let current_bits = current.bits();
    0o666 & !current_bits
}

fn compiler_diagnostics_runtime_environment(name: &str) -> bool {
    matches!(
        name,
        "AR" | "BINDGEN_EXTRA_CLANG_ARGS"
            | "CC"
            | "CFLAGS"
            | "CPPFLAGS"
            | "CXX"
            | "CXXFLAGS"
            | "DEVELOPER_DIR"
            | "DYLD_FALLBACK_LIBRARY_PATH"
            | "DYLD_INSERT_LIBRARIES"
            | "DYLD_LIBRARY_PATH"
            | "LANG"
            | "LC_ALL"
            | "LC_CTYPE"
            | "LD"
            | "LDFLAGS"
            | "LD_LIBRARY_PATH"
            | "LD_PRELOAD"
            | "LIBCLANG_PATH"
            | "MACOSX_DEPLOYMENT_TARGET"
            | "PATH"
            | "PKG_CONFIG"
            | "PKG_CONFIG_PATH"
            | "PKG_CONFIG_SYSROOT_DIR"
            | "RANLIB"
            | "RUSTC_BOOTSTRAP"
            | "RUSTC_FORCE_INCREMENTAL"
            | "RUSTC_LOG"
            | "RUST_MIN_STACK"
            | "SDKROOT"
            | "SOURCE_DATE_EPOCH"
    ) || ["AR_", "CC_", "CFLAGS_", "CXX_", "CXXFLAGS_", "PKG_CONFIG_", "RANLIB_"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
        || ["_AR", "_CC", "_CFLAGS", "_CXX", "_CXXFLAGS", "_RANLIB"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

fn native_compiler_process_environment(name: &str) -> bool {
    // Linked native actions bind their resolved driver, linker, and search
    // namespace in the platform linker witness. Raw PATH is intentionally not a
    // session-wide partition: setup-owned wrappers prepend their installation
    // directory, even though the selected toolchain is unchanged. Per-unit
    // environment observation still binds any additional values rustc reads.
    matches!(
        name,
        "AR" | "DEVELOPER_DIR"
            | "DYLD_FALLBACK_LIBRARY_PATH"
            | "DYLD_INSERT_LIBRARIES"
            | "DYLD_LIBRARY_PATH"
            | "GCC_EXEC_PREFIX"
            | "GNUTARGET"
            | "IPHONEOS_DEPLOYMENT_TARGET"
            | "LANG"
            | "LC_ALL"
            | "LC_CTYPE"
            | "LD"
            | "LDFLAGS"
            | "LD_LIBRARY_PATH"
            | "LD_PRELOAD"
            | "LD_RUN_PATH"
            | "LDEMULATION"
            | "LIBRARY_PATH"
            | "LPATH"
            | "MACOSX_DEPLOYMENT_TARGET"
            | "RANLIB"
            | "RUSTC_BOOTSTRAP"
            | "RUSTC_FORCE_INCREMENTAL"
            | "RUSTC_LOG"
            | "RUST_MIN_STACK"
            | "SDKROOT"
            | "SOURCE_DATE_EPOCH"
            | "TVOS_DEPLOYMENT_TARGET"
            | "VISIONOS_DEPLOYMENT_TARGET"
            | "WATCHOS_DEPLOYMENT_TARGET"
            | "ZERO_AR_DATE"
            | "COMPILER_PATH"
            | "COLLECT_NO_DEMANGLE"
    ) || ["AR_", "LC_", "RANLIB_", "RUSTC_"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
        || ["_AR", "_DEPLOYMENT_TARGET", "_RANLIB"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

fn cargo_config_fingerprint(cargo_config: &CargoConfigSnapshot, source_root: &Path) -> RailResult<String> {
    Ok(format!(
        "sha256:{}",
        ContentDigest::sha256(&cargo_config.portable_snapshot_identity(source_root)?)
    ))
}

fn append_identity_frame(output: &mut Vec<u8>, tag: &[u8], value: &[u8]) {
    output.extend_from_slice(&(tag.len() as u64).to_le_bytes());
    output.extend_from_slice(tag);
    output.extend_from_slice(&(value.len() as u64).to_le_bytes());
    output.extend_from_slice(value);
}

fn build_package_member_index(members: &[crate::cargo::manifest_analyzer::ParsedManifest]) -> HashMap<String, String> {
    let mut index = HashMap::with_capacity(members.len());
    for member in members {
        index.insert(member.package_id.to_string(), member.package_name.clone());
    }
    index
}

fn parse_unused_crate_name(message: &str) -> Option<&str> {
    let prefix = "extern crate `";
    let start = message.find(prefix)? + prefix.len();
    let rest = message.get(start..)?;
    let end = rest.find('`')?;
    rest.get(..end)
}

#[derive(Debug, Deserialize)]
struct CargoEvent {
    reason: String,
    package_id: Option<String>,
    target: Option<CargoTarget>,
    message: Option<CargoDiagnostic>,
    profile: Option<CargoProfile>,
    fresh: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CargoProfile {
    test: bool,
}

#[derive(Debug, Deserialize)]
struct CargoTarget {
    kind: Vec<String>,
    name: String,
    src_path: Option<String>,
}

impl CargoTarget {
    fn identifier(&self, workspace_root: &Path, test_mode: bool) -> CompilationUnitId {
        let kind = if self
            .kind
            .iter()
            .any(|kind| matches!(kind.as_str(), "lib" | "rlib" | "dylib" | "cdylib" | "staticlib"))
        {
            CargoTargetKind::Library
        } else if self.kind.iter().any(|kind| kind == "proc-macro") {
            CargoTargetKind::ProcMacro
        } else if self.kind.iter().any(|kind| kind == "bin") {
            CargoTargetKind::Binary
        } else if self.kind.iter().any(|kind| kind == "test") {
            CargoTargetKind::Test
        } else if self.kind.iter().any(|kind| kind == "example") {
            CargoTargetKind::Example
        } else if self.kind.iter().any(|kind| kind == "bench") {
            CargoTargetKind::Benchmark
        } else if self.kind.iter().any(|kind| kind == "custom-build") {
            CargoTargetKind::CustomBuild
        } else {
            CargoTargetKind::Other(self.kind.join(","))
        };
        let source = self.src_path.as_deref().map(|source| {
            Path::new(source)
                .strip_prefix(workspace_root)
                .unwrap_or_else(|_| Path::new(source))
                .to_string_lossy()
                .into_owned()
        });
        CompilationUnitId {
            kind,
            name: self.name.clone(),
            source,
            test_mode,
        }
    }
}

#[derive(Debug, Deserialize)]
struct CargoDiagnostic {
    message: String,
    code: Option<CargoDiagnosticCode>,
}

#[derive(Debug, Deserialize)]
struct CargoDiagnosticCode {
    code: Option<String>,
}

fn is_relevant_target(target: &CargoTarget) -> bool {
    !target.kind.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::facts::{
        CompilerFactCoverage, CompilerFactDomain, CompilerFactPackage, CompilerFactProducerAuthority, CompilerFactRole,
        CompilerFactUnit,
    };

    #[test]
    fn completed_compile_fail_doctest_ignores_only_its_absent_outputs() {
        let package = CompilerFactPackage {
            name: "member".to_string(),
            version: "0.1.0".to_string(),
            source: None,
        };
        let invocation = crate::compiler::observation::RawCompilerInvocation {
            version: 1,
            mode: CompilerMode::Rustc,
            crate_name: Some("doctest".to_string()),
            crate_types: BTreeSet::new(),
            target_argument: None,
            cfg: BTreeSet::new(),
            emit_modes: BTreeSet::new(),
            test_mode: true,
            compiler_arguments: Vec::new(),
            declared_inputs: Vec::new(),
            observed_reads: Vec::new(),
            dependency_artifacts: Vec::new(),
            emitted_outputs: Vec::new(),
            environment_reads: BTreeSet::new(),
            compiler: None,
            wrappers: Vec::new(),
            cache_wrapper: None,
            compiler_exit_code: Some(1),
            success: false,
            bypasses: BTreeSet::from([
                "emitted_output_bytes_unavailable".to_string(),
                "response_file_expansion_unavailable".to_string(),
            ]),
            compiler_fact_unit: Some(CompilerFactUnit {
                identity: "doctest-unit".to_string(),
                invocation_identity: format!("compiler-invocation-v1-sha256-{}", "5".repeat(64)),
                package,
                cargo_target: "member".to_string(),
                crate_name: "doctest".to_string(),
                target_kind: CompilerFactTargetKind::Library,
                domain: CompilerFactDomain::Doctest,
                role: CompilerFactRole::Target,
                platform: "target".to_string(),
                features: Vec::new(),
                cfg: Vec::new(),
            }),
        };

        assert_eq!(
            fact_invocation_cache_bypasses(std::slice::from_ref(&invocation), true),
            BTreeSet::from(["response_file_expansion_unavailable".to_string()])
        );
        assert_eq!(
            fact_invocation_cache_bypasses(&[invocation], false),
            BTreeSet::from([
                "compiler_invocation_failed".to_string(),
                "emitted_output_bytes_unavailable".to_string(),
                "response_file_expansion_unavailable".to_string(),
            ])
        );
    }

    #[test]
    fn compiler_fact_cargo_envelope_distinguishes_duplicate_target_names_by_kind() {
        let package = CompilerFactPackage {
            name: "member".to_string(),
            version: "0.1.0".to_string(),
            source: None,
        };
        let target = |target_kind| crate::compiler::session::CompilerFactTargetAuthority {
            package: package.clone(),
            manifest_directory: "member".to_string(),
            cargo_target: "shared".to_string(),
            crate_name: "shared".to_string(),
            target_kind,
            source: "member/shared.rs".to_string(),
            doctest: false,
        };
        let session = CompilerFactTypedSession {
            run_authority: CompilerFactRunAuthority {
                run_identity: format!("{RUN_IDENTITY_PREFIX}{}", "1".repeat(64)),
                view_identity: format!("compiler-view-v1-sha256-{}", "2".repeat(64)),
            },
            producer_authority: CompilerFactProducerAuthority {
                compiler_identity: format!("compiler-v1-sha256-{}", "3".repeat(64)),
                driver_identity: format!("compiler-fact-driver-v1-sha256-{}", "4".repeat(64)),
            },
            driver_program: "/driver".to_string(),
            rustc_program: "/rustc".to_string(),
            compiler_library_directory: "/lib".to_string(),
            host_platform: "host".to_string(),
            target_platform: "target".to_string(),
            doctest: false,
            doctest_sysroot: None,
            generated_roots: vec!["/generated".to_string()],
            required_coverage: BTreeSet::from([CompilerFactCoverage::Definitions]),
            targets: vec![
                target(CompilerFactTargetKind::Test),
                target(CompilerFactTargetKind::Benchmark),
            ],
        };
        let unit = |target_kind| CompilerFactUnit {
            identity: format!("unit-{target_kind:?}"),
            invocation_identity: format!("compiler-invocation-v1-sha256-{}", "5".repeat(64)),
            package: package.clone(),
            cargo_target: "shared".to_string(),
            crate_name: "shared".to_string(),
            target_kind,
            domain: CompilerFactDomain::NonProduction,
            role: CompilerFactRole::Target,
            platform: "target".to_string(),
            features: Vec::new(),
            cfg: Vec::new(),
        };
        let benchmark_event = serde_json::json!({
            "target": {
                "name": "shared",
                "kind": ["bench"],
            }
        });
        let test_event = serde_json::json!({
            "target": {
                "name": "shared",
                "kind": ["test"],
            }
        });

        validate_compiler_fact_cargo_envelope(&benchmark_event, &unit(CompilerFactTargetKind::Benchmark), &session)
            .expect("benchmark envelope");
        validate_compiler_fact_cargo_envelope(&test_event, &unit(CompilerFactTargetKind::Test), &session)
            .expect("test envelope");

        let mismatch =
            validate_compiler_fact_cargo_envelope(&test_event, &unit(CompilerFactTargetKind::Benchmark), &session)
                .expect_err("same-named test envelope must not satisfy benchmark authority")
                .to_string();
        assert!(mismatch.contains("member:shared"), "{mismatch}");
        assert!(mismatch.contains("Benchmark"), "{mismatch}");
        assert!(mismatch.contains("test"), "{mismatch}");
    }

    #[test]
    fn target_run_attributes_cargo_messages_by_package_identity() {
        let package_id = "path+file:///C:/workspace/member#0.1.0";
        let target = serde_json::json!({
          "kind": ["lib"],
          "name": "member",
          "src_path": "C:\\workspace\\member\\src\\lib.rs",
        });
        let diagnostic = serde_json::json!({
          "reason": "compiler-message",
          "package_id": package_id,
          "manifest_path": "C:\\workspace\\member\\Cargo.toml",
          "target": target,
          "message": {
            "message": "extern crate `log` is unused in crate `member`",
            "code": { "code": "unused_crate_dependencies" },
          },
        });
        let artifact = serde_json::json!({
          "reason": "compiler-artifact",
          "package_id": package_id,
          "manifest_path": "\\\\?\\C:\\workspace\\member\\Cargo.toml",
          "target": target,
          "profile": { "test": false },
        });
        let stdout = format!("{diagnostic}\n{artifact}\n");
        let package_to_member = HashMap::from([(package_id.to_string(), "member".to_string())]);
        let stale_members = HashSet::from(["member"]);
        let candidates = [CompilerCandidate {
            member: "member".to_string(),
            crate_name: "log".to_string(),
            kind: DepKind::Normal,
            applicable_targets: BTreeSet::from(["default".to_string()]),
            required_features: None,
        }];

        let parsed = parse_target_run(
            &stdout,
            Path::new("C:\\workspace"),
            &package_to_member,
            &stale_members,
            &candidates,
        );
        let member = parsed.get("member").expect("member evidence");
        assert_eq!(member.compiled_targets.len(), 1);
        assert_eq!(member.warned_targets_by_dep.get("log").map(BTreeSet::len), Some(1));
    }

    #[test]
    fn typed_artifact_completeness_rejects_cargo_freshness() {
        let root = tempfile::tempdir().expect("typed source root");
        let source = root.path().join("src/lib.rs");
        fs::create_dir_all(source.parent().expect("source parent")).expect("source directory");
        fs::write(&source, "pub fn item() {}\n").expect("source");
        let source = crate::utils::canonicalize_existing(&source).expect("canonical source");
        let selected = BTreeSet::from([("unit".to_string(), source.clone())]);
        let event = |fresh| {
            serde_json::json!({
              "reason": "compiler-artifact",
              "target": {
                "kind": ["lib"],
                "name": "unit",
                "src_path": source,
              },
              "fresh": fresh,
            })
            .to_string()
        };

        selected_typed_artifact_count_for(&event(true), &selected).unwrap_err();
        assert_eq!(
            selected_typed_artifact_count_for(&event(false), &selected).expect("non-fresh selected artifact"),
            1
        );
    }

    #[cfg(any(unix, windows))]
    struct InstalledTestFactDriver {
        path: PathBuf,
        installed: bool,
    }

    #[cfg(any(unix, windows))]
    impl InstalledTestFactDriver {
        fn install() -> RailResult<Self> {
            let source = std::env::var_os("CARGO_RAIL_TEST_FACT_DRIVER")
                .map(PathBuf::from)
                .ok_or_else(|| {
                    RailError::message("CARGO_RAIL_TEST_FACT_DRIVER is required for the exact-reuse workload")
                })?;
            let executable = std::env::current_exe()?;
            let path = executable
                .parent()
                .ok_or_else(|| RailError::message("test executable has no companion directory"))?
                .join(if cfg!(windows) {
                    "cargo-rail-fact-driver.exe"
                } else {
                    "cargo-rail-fact-driver"
                });
            if path.exists() {
                return Err(RailError::message(format!(
                    "refusing to replace pre-existing test driver sibling '{}'",
                    path.display()
                )));
            }
            fs::copy(&source, &path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o500))?;
            }
            Ok(Self { path, installed: true })
        }

        fn remove(&mut self) -> RailResult<()> {
            if self.installed {
                fs::remove_file(&self.path)?;
                self.installed = false;
            }
            Ok(())
        }
    }

    #[cfg(any(unix, windows))]
    impl Drop for InstalledTestFactDriver {
        fn drop(&mut self) {
            if self.installed {
                drop(fs::remove_file(&self.path));
            }
        }
    }

    #[cfg(any(unix, windows))]
    fn exact_reuse_workspace() -> RailResult<tempfile::TempDir> {
        let workspace = tempfile::Builder::new()
            .prefix("cargo-rail-compiler-fact-reuse-")
            .tempdir()?;
        crate::git::init_repo(workspace.path(), "main")?;
        fs::create_dir_all(workspace.path().join(".config"))?;
        fs::create_dir_all(workspace.path().join("app/src"))?;
        fs::create_dir_all(workspace.path().join("dep/src"))?;
        fs::write(
            workspace.path().join("Cargo.toml"),
            r#"[workspace]
members = ["app", "dep"]
resolver = "3"
"#,
        )?;
        fs::write(
            workspace.path().join("Cargo.lock"),
            r#"# This file is automatically @generated by Cargo.
# It is not intended for manual editing.
version = 4

[[package]]
name = "app"
version = "0.1.0"
dependencies = [
 "dep",
]

[[package]]
name = "dep"
version = "0.1.0"
"#,
        )?;
        fs::write(workspace.path().join(".gitignore"), "/target\n/.cargo-rail\n")?;
        fs::write(workspace.path().join(".config/rail.toml"), "")?;
        fs::write(
            workspace.path().join("app/Cargo.toml"),
            r#"[package]
name = "app"
version = "0.1.0"
edition = "2024"

[features]
default = ["default-mode"]
default-mode = []
extra = []

[dependencies]
dep = { path = "../dep" }
"#,
        )?;
        fs::write(
            workspace.path().join("app/src/lib.rs"),
            r#"pub fn answer() -> u32 {
  42
}
"#,
        )?;
        fs::write(
            workspace.path().join("dep/Cargo.toml"),
            r#"[package]
name = "dep"
version = "0.1.0"
edition = "2024"
"#,
        )?;
        fs::write(
            workspace.path().join("dep/src/lib.rs"),
            "pub fn unused() -> u32 { 7 }\n",
        )?;

        let git = crate::git::SystemGit::open(workspace.path())?;
        git.set_config("user.name", "Compiler Fact Test")?;
        git.set_config("user.email", "compiler-fact-test@example.invalid")?;
        git.set_config("commit.gpgSign", "false")?;
        git.stage_all()?;
        git.commit("fixture")?;
        Ok(workspace)
    }

    /// This is an ignored, explicitly provisioned native-driver workload rather
    /// than an ordinary unit test. It proves the cold acquisition and the warm
    /// exact-CAS path through the production collector.
    #[cfg(any(unix, windows))]
    #[test]
    #[ignore = "requires the exact rustc-dev companion authority embedded by the protocol harness"]
    fn exact_compiler_fact_cas_reuse_eliminates_independent_acquisitions() {
        let result: RailResult<()> = (|| {
            let workspace = exact_reuse_workspace()?;
            let context = crate::workspace::WorkspaceContext::build_with_snapshot(workspace.path())?;
            let snapshot = context.snapshot()?;
            let packages = context.cargo().workspace_members();
            let manifests = ManifestAnalyzer::parse_snapshot(snapshot, &packages)?;
            let identity = CompilerCacheIdentity::capture(snapshot)?;
            let targets = vec!["default"];
            let collector =
                CompilerDiagnosticsCollector::with_identity(workspace.path(), &manifests, targets.clone(), &identity);
            let candidates = [CompilerCandidate {
                member: "app".to_string(),
                crate_name: "dep".to_string(),
                kind: DepKind::Normal,
                applicable_targets: BTreeSet::from(["default".to_string()]),
                required_features: None,
            }];
            let typed_packages = BTreeSet::from(["app".to_string()]);
            let doctest_packages = BTreeSet::new();

            let combined = AnalysisSchedule::for_combined(
                &manifests.members,
                &targets,
                &candidates,
                &typed_packages,
                &doctest_packages,
            )?
            .views()
            .len();
            let diagnostics = AnalysisSchedule::for_diagnostics(&manifests.members, &targets, &candidates)?
                .views()
                .len();
            let typed =
                AnalysisSchedule::for_combined(&manifests.members, &targets, &[], &typed_packages, &doctest_packages)?
                    .views()
                    .len();
            assert_eq!((combined, diagnostics + typed), (3, 6));

            let driver_source = std::env::var_os("CARGO_RAIL_TEST_FACT_DRIVER")
                .map(PathBuf::from)
                .ok_or_else(|| {
                    RailError::message("CARGO_RAIL_TEST_FACT_DRIVER is required for the exact-reuse workload")
                })?;
            let driver_bytes = fs::metadata(driver_source)?.len();
            let mut installed_driver = InstalledTestFactDriver::install()?;
            let cold_started = Instant::now();
            let cold = collector.collect_with_typed_items(snapshot, &candidates, &typed_packages, &doctest_packages)?;
            let cold_elapsed = cold_started.elapsed();
            if cold.compiler_facts.is_empty() {
                return Err(RailError::message(
                    "cold compiler fact workload returned no exact objects",
                ));
            }
            let app = context
                .cargo()
                .get_package("app")
                .ok_or_else(|| RailError::message("fixture app package disappeared"))?;
            let cold_cache = &cold
                .diagnostics
                .get(&app.id)
                .ok_or_else(|| RailError::message("cold compiler diagnostics disappeared"))?
                .cache;
            assert_eq!((cold_cache.hits, cold_cache.misses), (0, diagnostics));
            let cold_objects = cold
                .compiler_facts
                .iter()
                .map(|fact| serde_json::to_vec(fact.object()))
                .collect::<Result<Vec<_>, _>>()?;
            let cold_identities = cold
                .compiler_facts
                .iter()
                .map(|fact| fact.identity().to_string())
                .collect::<Vec<_>>();

            installed_driver.remove()?;
            let warm_started = Instant::now();
            let warm = collector.collect_with_typed_items(snapshot, &[], &typed_packages, &doctest_packages)?;
            let warm_elapsed = warm_started.elapsed();
            assert!(warm.diagnostics.is_empty());
            assert_eq!(
                warm.compiler_facts
                    .iter()
                    .map(|fact| fact.identity())
                    .collect::<Vec<_>>(),
                cold_identities.iter().map(String::as_str).collect::<Vec<_>>()
            );
            assert_eq!(
                warm.compiler_facts
                    .iter()
                    .map(|fact| serde_json::to_vec(fact.object()))
                    .collect::<Result<Vec<_>, _>>()?,
                cold_objects
            );
            assert!(
                warm_elapsed < cold_elapsed,
                "warm exact reuse ({warm_elapsed:?}) must be faster than cold acquisition ({cold_elapsed:?})"
            );

            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                  "schema_version": 1,
                  "workload": "compiler-fact-exact-reuse",
                  "host": snapshot.toolchain().host_target(),
                  "combined_cold_cargo_views": combined,
                  "independent_cold_cargo_views": diagnostics + typed,
                  "cold_cargo_views_eliminated": diagnostics + typed - combined,
                  "warm_cargo_views": 0,
                  "cold_wall_ms": u64::try_from(cold_elapsed.as_millis()).unwrap_or(u64::MAX),
                  "warm_wall_ms": u64::try_from(warm_elapsed.as_millis()).unwrap_or(u64::MAX),
                  "exact_fact_objects": cold_objects.len(),
                  "exact_fact_bytes": cold_objects.iter().map(Vec::len).sum::<usize>(),
                  "driver_bytes": driver_bytes,
                }))?
            );
            Ok(())
        })();
        result.unwrap();
    }

    /// Compare equivalent serial and concurrent cold acquisitions in one
    /// physical workspace. Cache reuse is disabled so both policies execute
    /// the same three typed views and produce byte-identical fact objects.
    #[cfg(any(unix, windows))]
    #[test]
    #[ignore = "requires the exact rustc-dev companion authority embedded by the qualification harness"]
    fn compiler_fact_concurrent_acquisition_qualification_sample() {
        let result: RailResult<()> = (|| {
            let workspace = exact_reuse_workspace()?;
            let context = crate::workspace::WorkspaceContext::build_with_snapshot(workspace.path())?;
            let snapshot = context.snapshot()?;
            let packages = context.cargo().workspace_members();
            let manifests = ManifestAnalyzer::parse_snapshot(snapshot, &packages)?;
            let mut identity = CompilerCacheIdentity::capture(snapshot)?;
            identity.analysis_cache = None;
            identity.cache_bypass_reason = Some(CompilerCacheBypass::CargoConfiguration);
            let targets = vec!["default"];
            let typed_packages = BTreeSet::from(["app".to_string()]);
            let doctest_packages = BTreeSet::new();
            let _installed_driver = InstalledTestFactDriver::install()?;

            let run = |policy: ExecutionPolicy| -> RailResult<(Duration, usize, usize, Vec<Vec<u8>>)> {
                assert_eq!(
                    QUALIFICATION_LIVE_CARGO_VIEWS.load(std::sync::atomic::Ordering::Relaxed),
                    0,
                    "a prior acquisition retained a live Cargo process"
                );
                QUALIFICATION_CARGO_VIEWS.store(0, std::sync::atomic::Ordering::Relaxed);
                QUALIFICATION_COMPILER_INVOCATIONS.store(0, std::sync::atomic::Ordering::Relaxed);
                QUALIFICATION_MAX_LIVE_CARGO_VIEWS.store(0, std::sync::atomic::Ordering::Relaxed);
                let collector = CompilerDiagnosticsCollector::with_identity(
                    workspace.path(),
                    &manifests,
                    targets.clone(),
                    &identity,
                )
                .with_execution_policy(policy);
                let started = Instant::now();
                let evidence = collector.collect_with_typed_items(snapshot, &[], &typed_packages, &doctest_packages)?;
                let elapsed = started.elapsed();
                let cargo_views = QUALIFICATION_CARGO_VIEWS.load(std::sync::atomic::Ordering::Relaxed);
                let compiler_invocations =
                    QUALIFICATION_COMPILER_INVOCATIONS.load(std::sync::atomic::Ordering::Relaxed);
                let max_live = QUALIFICATION_MAX_LIVE_CARGO_VIEWS.load(std::sync::atomic::Ordering::Relaxed);
                if cargo_views != 3 || compiler_invocations == 0 {
                    return Err(RailError::message(format!(
                        "compiler concurrency workload executed {cargo_views} Cargo views and {compiler_invocations} compiler invocations"
                    )));
                }
                let objects = evidence
                    .compiler_facts
                    .iter()
                    .map(|fact| serde_json::to_vec(fact.object()))
                    .collect::<Result<Vec<_>, _>>()?;
                if objects.is_empty() {
                    return Err(RailError::message(
                        "compiler concurrency workload returned no exact fact objects",
                    ));
                }
                Ok((elapsed, compiler_invocations, max_live, objects))
            };

            let serial_policy = ExecutionPolicy::fixed(1, 1)?;
            let concurrent_policy = ExecutionPolicy::fixed(3, 3)?;
            let serial_first = run(serial_policy)?;
            let concurrent_first = run(concurrent_policy)?;
            let concurrent_second = run(concurrent_policy)?;
            let serial_second = run(serial_policy)?;

            let reference = &serial_first.3;
            for objects in [&concurrent_first.3, &concurrent_second.3, &serial_second.3] {
                if objects != reference {
                    return Err(RailError::message(
                        "serial and concurrent compiler acquisitions produced different exact fact objects",
                    ));
                }
            }
            if serial_first.1 != concurrent_first.1
                || serial_first.1 != concurrent_second.1
                || serial_first.1 != serial_second.1
            {
                return Err(RailError::message(
                    "serial and concurrent compiler acquisitions executed different compiler work",
                ));
            }
            if serial_first.2 != 1 || serial_second.2 != 1 {
                return Err(RailError::message(
                    "serial compiler acquisition exceeded one live Cargo process",
                ));
            }
            if concurrent_first.2 < 2 || concurrent_second.2 < 2 {
                return Err(RailError::message(
                    "concurrent compiler acquisition did not overlap Cargo processes",
                ));
            }
            let serial_wall = serial_first.0.saturating_add(serial_second.0);
            let concurrent_wall = concurrent_first.0.saturating_add(concurrent_second.0);
            if concurrent_wall >= serial_wall {
                return Err(RailError::message(format!(
                    "concurrent compiler acquisition ({concurrent_wall:?}) did not outperform equivalent serial work ({serial_wall:?})"
                )));
            }

            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "schema_version": 1,
                    "workload": "compiler-fact-concurrent-acquisition",
                    "host": snapshot.toolchain().host_target(),
                    "cargo_views_per_run": 3,
                    "compiler_invocations_per_run": serial_first.1,
                    "serial_wall_ns": u64::try_from(serial_wall.as_nanos()).unwrap_or(u64::MAX),
                    "concurrent_wall_ns": u64::try_from(concurrent_wall.as_nanos()).unwrap_or(u64::MAX),
                    "reduction_percent": 100.0 * (serial_wall.as_secs_f64() - concurrent_wall.as_secs_f64())
                        / serial_wall.as_secs_f64(),
                    "serial_max_live_cargo_processes": serial_first.2.max(serial_second.2),
                    "concurrent_max_live_cargo_processes": concurrent_first.2.max(concurrent_second.2),
                    "exact_fact_objects": reference.len(),
                    "exact_fact_bytes": reference.iter().map(Vec::len).sum::<usize>(),
                }))?
            );
            Ok(())
        })();
        result.unwrap();
    }

    /// Execute one release-optimized acquisition sample for the retained Task 6
    /// qualification harness. The lane is explicit so the harness measures the
    /// real independent collectors instead of inferring their cost from a view
    /// count.
    #[cfg(any(unix, windows))]
    #[test]
    #[ignore = "requires the exact rustc-dev companion authority embedded by the qualification harness"]
    fn compiler_fact_acquisition_qualification_sample() {
        let result: RailResult<()> = (|| {
            let lane = std::env::var("CARGO_RAIL_COMPILER_FACT_QUALIFICATION_LANE")
                .map_err(|_| RailError::message("CARGO_RAIL_COMPILER_FACT_QUALIFICATION_LANE is required"))?;
            if lane != "combined" && lane != "independent" {
                return Err(RailError::message(format!(
                    "unsupported compiler fact qualification lane '{lane}'"
                )));
            }

            let workspace = exact_reuse_workspace()?;
            let context = crate::workspace::WorkspaceContext::build_with_snapshot(workspace.path())?;
            let snapshot = context.snapshot()?;
            let packages = context.cargo().workspace_members();
            let manifests = ManifestAnalyzer::parse_snapshot(snapshot, &packages)?;
            let identity = CompilerCacheIdentity::capture(snapshot)?;
            let targets = vec!["default"];
            let collector =
                CompilerDiagnosticsCollector::with_identity(workspace.path(), &manifests, targets.clone(), &identity);
            let candidates = [CompilerCandidate {
                member: "app".to_string(),
                crate_name: "dep".to_string(),
                kind: DepKind::Normal,
                applicable_targets: BTreeSet::from(["default".to_string()]),
                required_features: None,
            }];
            let typed_packages = BTreeSet::from(["app".to_string()]);
            let doctest_packages = BTreeSet::new();
            let combined_views = AnalysisSchedule::for_combined(
                &manifests.members,
                &targets,
                &candidates,
                &typed_packages,
                &doctest_packages,
            )?
            .views()
            .len();
            let diagnostic_views = AnalysisSchedule::for_diagnostics(&manifests.members, &targets, &candidates)?
                .views()
                .len();
            let typed_views =
                AnalysisSchedule::for_combined(&manifests.members, &targets, &[], &typed_packages, &doctest_packages)?
                    .views()
                    .len();
            assert_eq!((combined_views, diagnostic_views + typed_views), (3, 6));
            // The diagnostics-only lane proves the fixture's sole candidate in
            // its first view and stops. The remaining two scheduled diagnostic
            // views are intentionally eliminated; the independent baseline is
            // executed work, not the schedule's theoretical upper bound.
            let independent_diagnostic_views = 1;

            let driver_source = std::env::var_os("CARGO_RAIL_TEST_FACT_DRIVER")
                .map(PathBuf::from)
                .ok_or_else(|| {
                    RailError::message("CARGO_RAIL_TEST_FACT_DRIVER is required for the qualification workload")
                })?;
            let driver_bytes = fs::metadata(driver_source)?.len();
            let mut installed_driver = InstalledTestFactDriver::install()?;
            QUALIFICATION_CARGO_VIEWS.store(0, std::sync::atomic::Ordering::Relaxed);
            QUALIFICATION_COMPILER_INVOCATIONS.store(0, std::sync::atomic::Ordering::Relaxed);

            let cold_started = Instant::now();
            let cold = if lane == "combined" {
                collector.collect_with_typed_items(snapshot, &candidates, &typed_packages, &doctest_packages)?
            } else {
                let diagnostics = collector.collect_for_candidates(&candidates)?;
                let mut typed =
                    collector.collect_with_typed_items(snapshot, &[], &typed_packages, &doctest_packages)?;
                assert!(typed.diagnostics.is_empty());
                typed.diagnostics = diagnostics;
                typed
            };
            let cold_elapsed = cold_started.elapsed();
            let cold_cargo_views = QUALIFICATION_CARGO_VIEWS.load(std::sync::atomic::Ordering::Relaxed);
            let cold_compiler_invocations =
                QUALIFICATION_COMPILER_INVOCATIONS.load(std::sync::atomic::Ordering::Relaxed);
            let expected_cold_views = if lane == "combined" {
                combined_views
            } else {
                independent_diagnostic_views + typed_views
            };
            assert_eq!(cold_cargo_views, expected_cold_views);
            if cold.compiler_facts.is_empty() {
                return Err(RailError::message(
                    "compiler fact qualification workload returned no exact objects",
                ));
            }
            let app = context
                .cargo()
                .get_package("app")
                .ok_or_else(|| RailError::message("qualification fixture app package disappeared"))?;
            let cold_cache = &cold
                .diagnostics
                .get(&app.id)
                .ok_or_else(|| RailError::message("qualification compiler diagnostics disappeared"))?
                .cache;
            assert_eq!((cold_cache.hits, cold_cache.misses), (0, diagnostic_views));
            let cold_objects = cold
                .compiler_facts
                .iter()
                .map(|fact| serde_json::to_vec(fact.object()))
                .collect::<Result<Vec<_>, _>>()?;
            let cold_identities = cold
                .compiler_facts
                .iter()
                .map(|fact| fact.identity().to_string())
                .collect::<Vec<_>>();
            let mut framed_objects = Vec::new();
            for object in &cold_objects {
                framed_objects.extend_from_slice(&(object.len() as u64).to_le_bytes());
                framed_objects.extend_from_slice(object);
            }
            let object_set_digest = format!("sha256:{}", ContentDigest::sha256(&framed_objects));

            let mut warm_wall_ns = None;
            let mut warm_cargo_views = None;
            let mut warm_compiler_invocations = None;
            if lane == "combined" {
                installed_driver.remove()?;
                QUALIFICATION_CARGO_VIEWS.store(0, std::sync::atomic::Ordering::Relaxed);
                QUALIFICATION_COMPILER_INVOCATIONS.store(0, std::sync::atomic::Ordering::Relaxed);
                let warm_started = Instant::now();
                let warm = collector.collect_with_typed_items(snapshot, &[], &typed_packages, &doctest_packages)?;
                let warm_elapsed = warm_started.elapsed();
                assert!(warm.diagnostics.is_empty());
                assert_eq!(
                    warm.compiler_facts
                        .iter()
                        .map(|fact| fact.identity())
                        .collect::<Vec<_>>(),
                    cold_identities.iter().map(String::as_str).collect::<Vec<_>>()
                );
                assert_eq!(
                    warm.compiler_facts
                        .iter()
                        .map(|fact| serde_json::to_vec(fact.object()))
                        .collect::<Result<Vec<_>, _>>()?,
                    cold_objects
                );
                warm_wall_ns = Some(u64::try_from(warm_elapsed.as_nanos()).unwrap_or(u64::MAX));
                warm_cargo_views = Some(QUALIFICATION_CARGO_VIEWS.load(std::sync::atomic::Ordering::Relaxed));
                warm_compiler_invocations =
                    Some(QUALIFICATION_COMPILER_INVOCATIONS.load(std::sync::atomic::Ordering::Relaxed));
                assert_eq!((warm_cargo_views, warm_compiler_invocations), (Some(0), Some(0)));
                assert!(warm_elapsed < cold_elapsed);
            }

            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                  "schema_version": 2,
                  "workload": "compiler-fact-acquisition",
                  "lane": lane,
                  "host": snapshot.toolchain().host_target(),
                  "combined_scheduled_cargo_views": combined_views,
                  "diagnostic_scheduled_cargo_views": diagnostic_views,
                  "typed_scheduled_cargo_views": typed_views,
                  "cold_cargo_views": cold_cargo_views,
                  "cold_compiler_invocations": cold_compiler_invocations,
                  "cold_wall_ns": u64::try_from(cold_elapsed.as_nanos()).unwrap_or(u64::MAX),
                  "warm_cargo_views": warm_cargo_views,
                  "warm_compiler_invocations": warm_compiler_invocations,
                  "warm_wall_ns": warm_wall_ns,
                  "exact_fact_objects": cold_objects.len(),
                  "exact_fact_bytes": cold_objects.iter().map(Vec::len).sum::<usize>(),
                  "exact_fact_identities": cold_identities,
                  "exact_fact_set_digest": object_set_digest,
                  "driver_bytes": driver_bytes,
                }))?
            );
            Ok(())
        })();
        result.unwrap();
    }

    #[test]
    fn native_session_environment_excludes_launcher_and_build_script_only_state() {
        for name in [
            "LD",
            "LD_LIBRARY_PATH",
            "RUSTC_BOOTSTRAP",
            "SDKROOT",
            "X86_64_UNKNOWN_LINUX_GNU_AR",
        ] {
            assert!(
                native_compiler_process_environment(name),
                "missing compiler state: {name}"
            );
        }
        for name in ["BINDGEN_EXTRA_CLANG_ARGS", "CC", "CFLAGS", "CXX", "PKG_CONFIG_PATH"] {
            assert!(
                !native_compiler_process_environment(name),
                "build-script-only state partitioned every native compiler unit: {name}"
            );
            assert!(
                compiler_diagnostics_runtime_environment(name),
                "diagnostic evidence still needs the broader environment: {name}"
            );
        }
        assert!(
            !native_compiler_process_environment("PATH"),
            "raw PATH must not partition every native compiler unit"
        );
        for name in ["GCC_EXEC_PREFIX", "COMPILER_PATH", "LIBRARY_PATH", "LDEMULATION"] {
            assert!(
                native_compiler_process_environment(name),
                "linked compiler state is not partitioned: {name}"
            );
        }
    }

    #[test]
    fn native_session_search_paths_abstract_only_the_selected_target_root() {
        let first = tempfile::tempdir().expect("first target root");
        let second = tempfile::tempdir().expect("second target root");
        let external = tempfile::tempdir().expect("external search root");
        let first_deps = first.path().join("debug/deps");
        let second_deps = second.path().join("debug/deps");
        fs::create_dir_all(&first_deps).expect("first deps");
        fs::create_dir_all(&second_deps).expect("second deps");
        let first_value = std::env::join_paths([first_deps.as_path(), external.path()]).expect("first path list");
        let second_value = std::env::join_paths([second_deps.as_path(), external.path()]).expect("second path list");

        assert_eq!(
            transparent_native_environment_value(
                "DYLD_FALLBACK_LIBRARY_PATH",
                &first_value,
                &crate::utils::canonicalize_existing(first.path()).expect("first canonical root"),
            ),
            transparent_native_environment_value(
                "DYLD_FALLBACK_LIBRARY_PATH",
                &second_value,
                &crate::utils::canonicalize_existing(second.path()).expect("second canonical root"),
            ),
        );
        assert_ne!(
            transparent_native_environment_value("LD_LIBRARY_PATH", &first_value, first.path()),
            transparent_native_environment_value(
                "LD_LIBRARY_PATH",
                &std::env::join_paths([first_deps.as_path(), second.path()]).expect("changed external path list"),
                first.path(),
            ),
            "external search namespaces must remain exact",
        );
        assert_ne!(
            transparent_native_environment_value("LD_PRELOAD", first.path().as_os_str(), first.path()),
            transparent_native_environment_value("LD_PRELOAD", second.path().as_os_str(), second.path()),
            "injected libraries are exact files, not selected-root search namespaces",
        );
    }

    #[cfg(unix)]
    #[test]
    fn compiler_artifact_hard_limit_terminates_an_active_view() {
        let root = tempfile::tempdir().expect("artifact root");
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("dd if=/dev/urandom of=\"$1/artifact\" bs=1048576 count=8 2>/dev/null; sync; exec sleep 10")
            .arg("cargo-rail-artifact-budget-test")
            .arg(root.path());
        let started = Instant::now();
        let error = run_artifact_bounded_command(
            &mut command,
            root.path(),
            CompilerArtifactBudget::new(512 * 1024, 1024 * 1024),
            &AtomicBool::new(false),
            false,
        )
        .expect_err("artifact growth must exceed the hard limit");

        assert!(error.to_string().contains("hard limit"), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "budget monitor did not terminate the active view"
        );
    }

    #[cfg(unix)]
    #[test]
    fn malformed_cargo_stream_terminates_the_owned_process_tree() {
        let root = tempfile::tempdir().expect("artifact root");
        let mut command = Command::new("sh");
        command.args(["-c", "printf '{\"reason\":17}\\n'; exec sleep 10"]);
        let started = Instant::now();
        let error = run_artifact_bounded_command(
            &mut command,
            root.path(),
            CompilerArtifactBudget::default(),
            &AtomicBool::new(false),
            false,
        )
        .expect_err("malformed Cargo output must fail");

        assert!(error.to_string().contains("unexpected message shape"), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "stream failure did not terminate the active process tree"
        );
    }

    #[cfg(unix)]
    #[test]
    fn noisy_cargo_stderr_is_drained_without_unbounded_retention() {
        let root = tempfile::tempdir().expect("artifact root");
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "i=0; while [ \"$i\" -lt 20000 ]; do printf '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef'; i=$((i + 1)); done >&2; printf '{\"reason\":\"build-finished\",\"success\":true}\\n'",
        ]);
        let output = run_artifact_bounded_command(
            &mut command,
            root.path(),
            CompilerArtifactBudget::default(),
            &AtomicBool::new(false),
            false,
        )
        .expect("bounded noisy stderr");

        assert!(output.status.success());
        assert!(
            output.stdout.is_empty(),
            "build-finished message should not be retained"
        );
        assert_eq!(
            output.stderr.len(),
            crate::compiler::acquisition::output::MAX_RETAINED_STDERR_BYTES
        );
    }

    #[test]
    fn compiler_artifact_free_reserve_scales_to_the_physical_volume() {
        assert_eq!(
            compiler_artifact_free_reserve_bytes(512 * 1024 * 1024),
            512 * 1024 * 1024 / COMPILER_ARTIFACT_FREE_RESERVE_DIVISOR
        );
        assert_eq!(
            compiler_artifact_free_reserve_bytes(64 * 1024 * 1024 * 1024),
            MAX_COMPILER_ARTIFACT_FREE_RESERVE_BYTES
        );
    }

    #[test]
    fn compiler_artifact_hard_limit_checks_exact_final_bytes() {
        let root = tempfile::tempdir().expect("artifact root");
        fs::write(root.path().join("artifact"), vec![7_u8; 2048]).expect("artifact");
        let mut command = if cfg!(windows) {
            let mut command = Command::new("cmd");
            command.args(["/c", "exit", "0"]);
            command
        } else {
            let mut command = Command::new("sh");
            command.args(["-c", "exit 0"]);
            command
        };
        let error = run_artifact_bounded_command(
            &mut command,
            root.path(),
            CompilerArtifactBudget::new(512, 1024),
            &AtomicBool::new(false),
            false,
        )
        .expect_err("final logical bytes must remain bounded");

        assert!(error.to_string().contains("2048 bytes"), "{error}");
        assert!(error.to_string().contains("hard limit"), "{error}");
    }

    #[test]
    fn build_script_executable_comes_from_cargo_filenames() {
        let unix = FileObservation {
            path: ObservationPath::Repository("target/debug/build/unit/build-script-build".to_string()),
            content_digest: "sha256:unix".to_string(),
            executable: true,
            symlink_target: None,
        };
        assert_eq!(
            build_script_executable_output(std::slice::from_ref(&unix), "build-script-build", ""),
            Some(unix)
        );

        let windows = FileObservation {
            path: ObservationPath::Repository("target/debug/build/unit/build-script-build.exe".to_string()),
            content_digest: "sha256:windows".to_string(),
            executable: false,
            symlink_target: None,
        };
        let debug_symbols = FileObservation {
            path: ObservationPath::Repository("target/debug/build/unit/build-script-build.pdb".to_string()),
            content_digest: "sha256:pdb".to_string(),
            executable: false,
            symlink_target: None,
        };
        assert_eq!(
            build_script_executable_output(&[debug_symbols, windows.clone()], "build-script-build", ".exe"),
            Some(windows.clone())
        );
        let ambiguous = FileObservation {
            path: ObservationPath::Repository("other/build-script-build.exe".to_string()),
            ..windows.clone()
        };
        assert_eq!(
            build_script_executable_output(&[windows, ambiguous], "build-script-build", ".exe"),
            None
        );
    }

    #[test]
    fn cargo_build_script_summary_discards_values_and_physical_paths() {
        let script: cargo_metadata::BuildScript = serde_json::from_value(serde_json::json!({
          "package_id": "path+file:///workspace#unit@0.1.0",
          "linked_libs": ["static=never-persist-this-library"],
          "linked_paths": ["native=/physical/never-persist-this-path"],
          "cfgs": ["never_persist_this_cfg"],
          "env": [["REGISTRY_TOKEN", "never-persist-this-value"]],
          "out_dir": "/physical/never-persist-this-output",
        }))
        .expect("Cargo build-script message");
        let summary = build_script_output_summary(&script);
        assert_eq!(
            summary,
            BuildScriptCargoOutputSummary {
                linked_libraries: 1,
                linked_paths: 1,
                cfgs: 1,
                rustc_environment: 1,
                output_directory_reported: true,
            }
        );
        let encoded = serde_json::to_string(&summary).expect("serialize redacted summary");
        for raw in [
            "never-persist-this-library",
            "never-persist-this-path",
            "never_persist_this_cfg",
            "REGISTRY_TOKEN",
            "never-persist-this-value",
            "never-persist-this-output",
        ] {
            assert!(!encoded.contains(raw), "persisted raw Cargo output {raw:?}");
        }
    }

    #[test]
    fn cargo_build_script_output_selection_fails_closed() {
        let output = BuildScriptCargoOutputSummary {
            linked_libraries: 1,
            linked_paths: 2,
            cfgs: 3,
            rustc_environment: 4,
            output_directory_reported: true,
        };
        assert_eq!(
            select_build_script_output(Some(&CargoBuildScriptOutput::One(output.clone()))),
            (Some(output), "cargo_build_script_execution_freshness_unavailable")
        );
        assert_eq!(
            select_build_script_output(None),
            (None, "cargo_build_script_output_unavailable")
        );
        assert_eq!(
            select_build_script_output(Some(&CargoBuildScriptOutput::Ambiguous)),
            (None, "cargo_build_script_output_ambiguous")
        );
    }

    #[test]
    fn test_candidate_scheduler_keeps_inapplicable_declaration_for_later_target() {
        let candidates = vec![
            CompilerCandidate {
                member: "member".to_string(),
                crate_name: "alpha".to_string(),
                kind: crate::cargo::manifest_analyzer::DepKind::Normal,
                applicable_targets: BTreeSet::from(["linux".to_string()]),
                required_features: None,
            },
            CompilerCandidate {
                member: "member".to_string(),
                crate_name: "beta".to_string(),
                kind: crate::cargo::manifest_analyzer::DepKind::Normal,
                applicable_targets: BTreeSet::from(["linux".to_string(), "macos".to_string()]),
                required_features: None,
            },
        ];
        let targets = build_candidate_target_index(&candidates);
        let mut survivors = HashMap::from([(
            "member".to_string(),
            BTreeSet::from([
                (
                    crate::cargo::manifest_analyzer::DepKind::Normal,
                    "alpha".to_string(),
                    None,
                ),
                (
                    crate::cargo::manifest_analyzer::DepKind::Normal,
                    "beta".to_string(),
                    None,
                ),
            ]),
        )]);
        let evidence = test_evidence(&["alpha"]);

        update_candidate_survivors(&mut survivors, &targets, "member", "macos", &evidence);

        assert_eq!(
            survivors["member"],
            BTreeSet::from([(
                crate::cargo::manifest_analyzer::DepKind::Normal,
                "alpha".to_string(),
                None
            )])
        );
        assert!(!has_applicable_survivor(
            &survivors,
            &targets,
            "member",
            "macos",
            &FeatureSelection::Default
        ));
        assert!(has_applicable_survivor(
            &survivors,
            &targets,
            "member",
            "linux",
            &FeatureSelection::Default
        ));
    }

    #[test]
    fn test_candidate_scheduler_stops_after_positive_usage() {
        let candidates = vec![CompilerCandidate {
            member: "member".to_string(),
            crate_name: "alpha".to_string(),
            kind: crate::cargo::manifest_analyzer::DepKind::Normal,
            applicable_targets: BTreeSet::from(["linux".to_string()]),
            required_features: None,
        }];
        let targets = build_candidate_target_index(&candidates);
        let mut survivors = HashMap::from([(
            "member".to_string(),
            BTreeSet::from([(
                crate::cargo::manifest_analyzer::DepKind::Normal,
                "alpha".to_string(),
                None,
            )]),
        )]);

        update_candidate_survivors(&mut survivors, &targets, "member", "linux", &test_evidence(&[]));

        assert!(survivors["member"].is_empty());
    }

    #[test]
    fn test_candidate_scheduler_stops_after_incomplete_required_view() {
        let candidates = vec![CompilerCandidate {
            member: "member".to_string(),
            crate_name: "alpha".to_string(),
            kind: crate::cargo::manifest_analyzer::DepKind::Normal,
            applicable_targets: BTreeSet::from(["linux".to_string()]),
            required_features: None,
        }];
        let targets = build_candidate_target_index(&candidates);
        let mut survivors = HashMap::from([(
            "member".to_string(),
            BTreeSet::from([(
                crate::cargo::manifest_analyzer::DepKind::Normal,
                "alpha".to_string(),
                None,
            )]),
        )]);
        let mut evidence = test_evidence(&["alpha"]);
        evidence.completeness = DiagnosticsCompleteness::Incomplete;

        update_candidate_survivors(&mut survivors, &targets, "member", "linux", &evidence);

        assert!(
            survivors["member"].is_empty(),
            "one incomplete required view already makes an unused proof impossible"
        );
    }

    #[test]
    fn test_candidate_scheduler_defers_optional_candidate_until_required_feature_mode() {
        let candidates = vec![CompilerCandidate {
            member: "member".to_string(),
            crate_name: "optional_dep".to_string(),
            kind: crate::cargo::manifest_analyzer::DepKind::Normal,
            applicable_targets: BTreeSet::from(["linux".to_string()]),
            required_features: Some(FeatureSelection::AllFeatures),
        }];
        let targets = build_candidate_target_index(&candidates);
        let candidate = (
            crate::cargo::manifest_analyzer::DepKind::Normal,
            "optional_dep".to_string(),
            Some(FeatureSelection::AllFeatures),
        );
        let mut survivors = HashMap::from([("member".to_string(), BTreeSet::from([candidate]))]);

        update_candidate_survivors(&mut survivors, &targets, "member", "linux", &test_evidence(&[]));
        assert_eq!(survivors["member"].len(), 1, "default-mode evidence is inapplicable");
        assert!(!has_applicable_survivor(
            &survivors,
            &targets,
            "member",
            "linux",
            &FeatureSelection::Default
        ));
        assert!(has_applicable_survivor(
            &survivors,
            &targets,
            "member",
            "linux",
            &FeatureSelection::AllFeatures
        ));

        let mut all_features = test_evidence(&[]);
        all_features.features = FeatureSelection::AllFeatures;
        update_candidate_survivors(&mut survivors, &targets, "member", "linux", &all_features);
        assert!(survivors["member"].is_empty());
    }

    #[test]
    fn compiler_sysroot_identity_rehashes_target_driver_and_self_contained_bytes() {
        let sysroot = tempfile::tempdir().expect("sysroot");
        let target_lib = sysroot.path().join("lib/rustlib/test-host/lib");
        std::fs::create_dir_all(&target_lib).expect("target lib");
        std::fs::write(target_lib.join("libcore-test.rlib"), b"target-one").expect("target library");
        let self_contained = target_lib.join("self-contained");
        std::fs::create_dir(&self_contained).expect("self-contained directory");
        let runtime = self_contained.join("crt1.o");
        std::fs::write(&runtime, b"runtime-one").expect("self-contained runtime");
        #[cfg(windows)]
        let driver = sysroot.path().join("bin/rustc_driver-test.dll");
        #[cfg(not(windows))]
        let driver = sysroot.path().join("lib/librustc_driver-test.so");
        std::fs::create_dir_all(driver.parent().expect("driver parent")).expect("driver directory");
        std::fs::write(&driver, b"driver-one").expect("driver library");
        #[cfg(windows)]
        let rustc_implementation = sysroot.path().join("bin/rustc.exe");
        #[cfg(not(windows))]
        let rustc_implementation = sysroot.path().join("bin/rustc");
        std::fs::create_dir_all(rustc_implementation.parent().expect("rustc parent")).expect("rustc directory");
        std::fs::write(rustc_implementation, b"rustc").expect("rustc implementation");

        let baseline = compiler_sysroot_fingerprint(sysroot.path(), "test-host", None).expect("baseline fingerprint");
        assert_eq!(baseline.1, 31);
        let inventory = compiler_sysroot_inventory(sysroot.path(), "test-host").expect("sysroot inventory");
        hash_compiler_sysroot_with_limit(&inventory, baseline.1).expect("exact byte limit");
        let error = hash_compiler_sysroot_with_limit(&inventory, baseline.1 - 1).expect_err("byte limit +1 must fail");
        assert!(
            error.to_string().contains("30-byte limit after 31 bytes"),
            "unexpected byte-limit diagnostic: {error}"
        );
        std::fs::write(target_lib.join("libcore-test.rlib"), b"target-two").expect("target mutation");
        let target_changed =
            compiler_sysroot_fingerprint(sysroot.path(), "test-host", None).expect("target fingerprint");
        assert_ne!(baseline.0, target_changed.0);
        std::fs::write(&driver, b"driver-two").expect("driver mutation");
        let driver_changed =
            compiler_sysroot_fingerprint(sysroot.path(), "test-host", None).expect("driver fingerprint");
        assert_ne!(target_changed.0, driver_changed.0);
        std::fs::write(&runtime, b"runtime-two").expect("self-contained runtime mutation");
        let runtime_changed =
            compiler_sysroot_fingerprint(sysroot.path(), "test-host", None).expect("runtime fingerprint");
        assert_ne!(driver_changed.0, runtime_changed.0);

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&runtime, self_contained.join("linked-crt1.o")).expect("self-contained symlink");
            let error = compiler_sysroot_fingerprint(sysroot.path(), "test-host", None)
                .expect_err("self-contained symlinks must fail closed");
            assert!(
                error
                    .to_string()
                    .contains("compiler self-contained sysroot contains a non-regular entry")
            );
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[test]
    fn compiler_sysroot_memo_requires_exact_content_generation_evidence() {
        let sysroot = tempfile::tempdir().expect("sysroot");
        let memo_directory = tempfile::tempdir().expect("memo directory");
        let memo = memo_directory.path().join("sysroot.json");
        let target_lib = sysroot.path().join("lib/rustlib/test-host/lib");
        std::fs::create_dir_all(&target_lib).expect("target lib");
        let target = target_lib.join("libcore-test.rlib");
        std::fs::write(&target, b"target-one").expect("target library");
        #[cfg(target_os = "macos")]
        let driver = sysroot.path().join("lib/librustc_driver-test.dylib");
        #[cfg(target_os = "linux")]
        let driver = sysroot.path().join("lib/librustc_driver-test.so");
        #[cfg(windows)]
        let driver = sysroot.path().join("bin/rustc_driver-test.dll");
        std::fs::create_dir_all(driver.parent().expect("driver parent")).expect("driver directory");
        std::fs::write(&driver, b"driver-one").expect("driver library");
        #[cfg(windows)]
        let rustc_implementation = sysroot.path().join("bin/rustc.exe");
        #[cfg(not(windows))]
        let rustc_implementation = sysroot.path().join("bin/rustc");
        std::fs::create_dir_all(rustc_implementation.parent().expect("rustc parent")).expect("rustc directory");
        std::fs::write(rustc_implementation, b"rustc").expect("rustc implementation");

        let baseline =
            compiler_sysroot_fingerprint(sysroot.path(), "test-host", Some(&memo)).expect("baseline fingerprint");
        assert_eq!(baseline.1, 20);
        let memo_hit = compiler_sysroot_fingerprint(sysroot.path(), "test-host", Some(&memo)).expect("memo hit");
        assert_eq!(memo_hit, (baseline.0.clone(), 0));

        let mut corrupted =
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(&memo).expect("memo bytes")).expect("memo JSON");
        corrupted["fingerprint"] = serde_json::Value::String(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".to_string(),
        );
        std::fs::write(&memo, serde_json::to_vec(&corrupted).expect("corrupted memo JSON")).expect("corrupted memo");
        let recovered =
            compiler_sysroot_fingerprint(sysroot.path(), "test-host", Some(&memo)).expect("corrupted memo recovery");
        assert_eq!(recovered, baseline, "a corrupted memo must force a full hash");
        let recovered_hit =
            compiler_sysroot_fingerprint(sysroot.path(), "test-host", Some(&memo)).expect("recovered memo hit");
        assert_eq!(recovered_hit, (baseline.0.clone(), 0));

        let modified = std::fs::metadata(&target)
            .and_then(|metadata| metadata.modified())
            .expect("target modification time");
        std::fs::write(&target, b"target-two").expect("same-size target mutation");
        std::fs::OpenOptions::new()
            .write(true)
            .open(&target)
            .and_then(|file| file.set_times(std::fs::FileTimes::new().set_modified(modified)))
            .expect("restore target modification time");

        let changed =
            compiler_sysroot_fingerprint(sysroot.path(), "test-host", Some(&memo)).expect("changed fingerprint");
        assert_eq!(changed.1, 20, "same-size content changes must force a full hash");
        assert_ne!(changed.0, baseline.0);
        let changed_hit =
            compiler_sysroot_fingerprint(sysroot.path(), "test-host", Some(&memo)).expect("changed memo hit");
        assert_eq!(changed_hit, (changed.0, 0));
    }

    #[test]
    fn windows_sysroot_capture_retries_transient_generation_drift() {
        let mut attempts = 0;
        let captured = retry_unstable_windows_sysroot_capture(|| {
            attempts += 1;
            Ok((attempts == 2).then_some("stable"))
        })
        .expect("second bracketed capture");

        assert_eq!(captured, "stable");
        assert_eq!(attempts, 2);
    }

    #[test]
    fn windows_sysroot_capture_rejects_persistent_generation_drift() {
        let mut attempts = 0;
        let error = retry_unstable_windows_sysroot_capture::<()>(|| {
            attempts += 1;
            Ok(None)
        })
        .expect_err("persistent drift");

        assert_eq!(attempts, WINDOWS_SYSROOT_CAPTURE_ATTEMPTS);
        assert_eq!(error.to_string(), "compiler sysroot changed during identity capture");
    }

    fn test_evidence(unused: &[&str]) -> TargetEvidence {
        TargetEvidence {
            platform: PlatformTarget::from("test"),
            features: FeatureSelection::Default,
            compiled_units: BTreeSet::from([CompilationUnitId {
                kind: CargoTargetKind::Library,
                name: "member".to_string(),
                source: Some("src/lib.rs".to_string()),
                test_mode: false,
            }]),
            unused_crates: unused.iter().map(|value| (*value).to_string()).collect(),
            unit_evidence: Vec::new(),
            completeness: DiagnosticsCompleteness::Complete,
        }
    }
}
