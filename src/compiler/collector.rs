//! Target-aware compiler diagnostics collection with persistent caching.

use crate::build_script::{
    BuildScriptActionInputs, BuildScriptCargoOutputSummary, BuildScriptResultInputs,
    analyze_action_key as analyze_build_script_action_key, analyze_result as analyze_build_script_result,
};
use crate::cache::cas::{LocalCacheSelection, LocalCas};
use crate::cargo::manifest_analyzer::ManifestAnalyzer;
use crate::cargo::{CargoConfigSnapshot, DepKind, ToolchainIdentity};
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
use crate::compiler::scheduler::{AnalysisSchedule, AnalysisView, CompilerCandidate};
use crate::compiler::session::{CompilerFactSession, CompilerFactTypedSession, FACT_SESSION_ENV};
use crate::error::{RailError, RailResult, ResultExt};
use crate::executable::{ExecutableIdentity, ToolchainExecutableIdentities};
use crate::progress;
use crate::source::{ContentDigest, SourceEntryKind};
use crate::workspace::WorkspaceSnapshot;
use cargo_metadata::{Message, PackageId, TargetKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[cfg(test)]
static QUALIFICATION_CARGO_VIEWS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static QUALIFICATION_COMPILER_INVOCATIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Compiler diagnostics collector and cache coordinator.
pub(crate) struct CompilerDiagnosticsCollector<'a> {
    workspace_root: &'a Path,
    manifests: &'a ManifestAnalyzer,
    targets: Vec<&'a str>,
    identity: CompilerCacheIdentity,
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
}

/// Exact snapshot-derived inputs shared by every compiler-evidence key.
#[derive(Clone)]
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
    rustdoc_program: OsString,
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
    executable_bypasses: BTreeSet<String>,
    cache_bypass_reason: Option<CompilerCacheBypass>,
}

#[derive(Clone)]
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

const TRANSPARENT_SESSION_MEMO_VERSION: u32 = 1;

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
        let rustdoc_program = snapshot.toolchain().rustdoc_program().to_owned();
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
        let mut executable_bypasses = executables.limitations().map(str::to_string).collect::<BTreeSet<_>>();
        executable_bypasses.extend(
            cargo_rail_executable
                .limitations()
                .map(|limitation| format!("compiler_wrapper_{limitation}")),
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
            rustdoc_program,
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
}

/// Capture the retained exact v10 session from the compiler Cargo selected for
/// one transparent wrapper invocation. This runs only after acquisition-free
/// eligibility gates have accepted the rustc shape.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn capture_transparent_native_session(
    source_root: &Path,
    rustc_program: &OsStr,
    cache: &LocalCacheSelection,
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
    let compiler_environment = transparent_native_compiler_process_env_fingerprint()?;
    let session = NativeCompilerSession::capture(
        &source_root,
        &rustc_verbose_version,
        &capability_identity,
        &compiler_environment,
        crate::compiler::native_cache::native_cache_execution_contract(),
        NativeSessionAuthority::Exact,
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
    _rustc_program: &OsStr,
    _cache: &LocalCacheSelection,
) -> RailResult<(NativeCompilerSession, u64, TransparentNativeSessionMemo)> {
    Err(RailError::message(
        "transparent compiler session memoization is unsupported on this platform",
    ))
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn reuse_transparent_native_session(
    memo: &TransparentNativeSessionMemo,
    source_root: &Path,
    rustc_program: &OsStr,
) -> RailResult<Option<NativeCompilerSession>> {
    let source_root = crate::utils::canonicalize_existing(source_root)?;
    if memo.version != TRANSPARENT_SESSION_MEMO_VERSION
        || memo.digest != transparent_session_memo_digest(memo)?
        || memo.source_root != source_root.to_string_lossy()
        || memo.compiler_environment_identity != transparent_native_compiler_process_env_fingerprint()?
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
        }
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
        let mut metrics = CompilerAnalysisMetrics {
            analysis_views: schedule.views().len(),
            ..CompilerAnalysisMetrics::default()
        };
        let members = schedule.packages().iter().map(String::as_str).collect::<HashSet<_>>();
        if members.is_empty() {
            return Ok(CompilerAnalysisEvidence {
                diagnostics: HashMap::new(),
                compiler_facts: Vec::new(),
                metrics,
            });
        }
        let typed_snapshot = if typed_packages.is_empty() {
            None
        } else {
            Some(snapshot.ok_or_else(|| {
                RailError::message("typed compiler fact collection requires its captured workspace snapshot")
            })?)
        };
        let producer_authority = typed_snapshot
            .map(|snapshot| {
                CompilerFactDriverAuthority::producer_authority(
                    snapshot.toolchain(),
                    &self.identity.toolchain_fingerprint,
                )
            })
            .transpose()?;
        let typed_cargo_target = typed_snapshot
            .map(|_| {
                tempfile::Builder::new()
                    .prefix("cargo-rail-compiler-target-")
                    .tempdir()
                    .with_context(|| "creating shared compiler-analysis target directory".to_string())
            })
            .transpose()?;
        let mut prepared_driver = None;
        let mut prepared_doctest_sysroot = None;

        let mut store = CompilerDiagnosticsStore::load(self.workspace_root);
        let fact_store = CompilerFactStore::load();
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
        let mut stale_by_configuration: BTreeMap<AnalysisView, Vec<&str>> = BTreeMap::new();
        let mut retained_observations = HashMap::<String, CompilationObservationManifest>::new();
        let mut surviving_unused: HashMap<String, BTreeSet<CandidateId>> = candidate_targets
            .iter()
            .map(|(member, candidates)| (member.clone(), candidates.keys().cloned().collect()))
            .collect();

        for view in schedule.views() {
            let target = view.platform().as_str();
            let collects_diagnostics = view
                .fact_families()
                .contains(&crate::compiler::scheduler::CompilerFactFamily::StableDiagnostics);
            let collects_typed = view
                .fact_families()
                .contains(&crate::compiler::scheduler::CompilerFactFamily::TypedRustItems);
            if collects_typed {
                stale_by_configuration.entry(view.clone()).or_default();
            }
            for member in view.packages() {
                let member = member.as_str();
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
                let key = self.key_for(manifest, target, view.features().clone())?;
                let mut cache_hit = false;
                let observation_miss = if self.identity.cache_bypass_reason.is_none() {
                    store.get(&key).and_then(|entry| {
                        let miss = compiler_observation_miss_reason(&entry.observations, self.workspace_root)
                            .map(str::to_string);
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

                stale_by_configuration.entry(view.clone()).or_default().push(member);
            }
        }

        let mut skipped_member_targets = 0usize;
        for (view, stale_members) in stale_by_configuration {
            let target = view.platform().as_str();
            let features = view.features();
            let diagnostic_members: Vec<&str> = stale_members
                .iter()
                .copied()
                .filter(|member| {
                    has_applicable_survivor(&surviving_unused, &candidate_targets, member, target, features)
                })
                .collect();
            skipped_member_targets += stale_members.len() - diagnostic_members.len();
            let typed_members = if view
                .fact_families()
                .contains(&crate::compiler::scheduler::CompilerFactFamily::TypedRustItems)
            {
                view.packages()
                    .intersection(typed_packages)
                    .cloned()
                    .collect::<BTreeSet<_>>()
            } else {
                BTreeSet::new()
            };
            let mut acquisition_members = diagnostic_members.iter().copied().collect::<BTreeSet<_>>();
            acquisition_members.extend(typed_members.iter().map(String::as_str));
            let fact_members = acquisition_members.iter().copied().collect::<Vec<_>>();
            let fact_cache_key = if typed_members.is_empty() || self.identity.cache_bypass_reason.is_some() {
                None
            } else {
                Some(
                    self.fact_cache_key(
                        &view,
                        &fact_members,
                        &typed_members,
                        producer_authority
                            .as_ref()
                            .ok_or_else(|| RailError::message("typed compiler fact producer authority disappeared"))?,
                    )?,
                )
            };
            let cached_facts = fact_cache_key
                .as_ref()
                .and_then(|key| fact_store.get(key).ok().flatten());
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
            let active_members = if collect_typed {
                fact_members
            } else {
                diagnostic_members.clone()
            };
            if active_members.is_empty() {
                continue;
            }

            let mut stale_set = HashSet::with_capacity(diagnostic_members.len());
            for member in &diagnostic_members {
                stale_set.insert(*member);
            }

            progress!(
                "  Collecting compiler evidence for target {} ({} package{})...",
                format_args!("{} / {}", target, features.label()),
                active_members.len(),
                if active_members.len() == 1 { "" } else { "s" }
            );
            let started = Instant::now();
            if collect_typed && prepared_driver.is_none() {
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
            if collect_typed && view.compiles_doctests() && prepared_doctest_sysroot.is_none() {
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
                        .ok_or_else(|| {
                            RailError::message("typed compiler fact driver disappeared before doctest staging")
                        })?
                        .stage_doctest_sysroot(snapshot, &wrapper, wrapper_digest, &rustdoc, rustdoc_digest)
                        .map_err(|error| {
                            RailError::message(format!(
                                "failed to stage the private typed-doctest compiler sysroot: {error}"
                            ))
                        })?,
                );
            }
            let typed_context = if collect_typed {
                Some(TypedAcquisitionContext {
                    snapshot: typed_snapshot
                        .ok_or_else(|| RailError::message("typed compiler fact snapshot disappeared"))?,
                    driver: prepared_driver
                        .as_ref()
                        .ok_or_else(|| RailError::message("typed compiler fact driver disappeared"))?,
                    doctest_sysroot: prepared_doctest_sysroot.as_ref(),
                    cargo_target: typed_cargo_target
                        .as_ref()
                        .ok_or_else(|| RailError::message("typed compiler target directory disappeared"))?
                        .path(),
                    packages: &typed_members,
                })
            } else {
                None
            };
            let mut run = run_workspace_check(
                self.workspace_root,
                &self.identity,
                &view,
                &active_members,
                typed_context.as_ref(),
            )
            .with_context(|| {
                format!(
                    "acquiring compiler evidence for target '{} / {}'",
                    view.platform().as_str(),
                    view.features().label()
                )
            })?;
            metrics.cargo_views_executed += 1;
            metrics.compiler_invocations += run.invocations.len();
            if collect_typed {
                metrics.fresh_fragment_bytes =
                    run.compiler_facts
                        .iter()
                        .try_fold(metrics.fresh_fragment_bytes, |total, fragment| {
                            total
                                .checked_add(fragment.bytes())
                                .ok_or_else(|| RailError::message("compiler fact fragment byte count overflow"))
                        })?;
                let fresh_facts = std::mem::take(&mut run.compiler_facts)
                    .into_iter()
                    .map(ValidatedCompilerFactFragment::into_object)
                    .collect::<Vec<_>>();
                if run.success
                    && let Some(key) = &fact_cache_key
                {
                    let bypasses = fact_invocation_cache_bypasses(&run.invocations);
                    let complete_empty_view = fresh_facts.is_empty()
                        && bypasses == BTreeSet::from(["no_typed_compiler_invocation".to_string()]);
                    if bypasses.is_empty() || complete_empty_view {
                        if let Err(error) = fact_store.put(key, &fresh_facts) {
                            metrics.fact_cache_store_failures += 1;
                            progress!("    Compiler fact cache store bypassed: {error}");
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
            progress!(
                "    Finished target {} in {:.1}s",
                format_args!("{} / {}", target, features.label()),
                started.elapsed().as_secs_f64()
            );
            if !run.success && !run.stderr.trim().is_empty() {
                progress!("    Cargo analysis failed:\n{}", run.stderr.trim_end());
            }
            if !view
                .fact_families()
                .contains(&crate::compiler::scheduler::CompilerFactFamily::StableDiagnostics)
            {
                continue;
            }
            let parsed = parse_target_run(
                &run.stdout,
                self.workspace_root,
                &package_to_member,
                &stale_set,
                candidates,
            );
            let invocations = std::mem::take(&mut run.invocations);
            let mut compilation_observations =
                parse_compilation_observations(&run.stdout, invocations, &self.identity, target)?;
            reconcile_exact_artifact_observations(&mut compilation_observations, &mut retained_observations);
            let completeness = if run.success {
                DiagnosticsCompleteness::Complete
            } else {
                DiagnosticsCompleteness::Incomplete
            };

            for member in diagnostic_members {
                let manifests_member = self
                    .manifests
                    .members
                    .iter()
                    .find(|m| m.package_name == member)
                    .ok_or_else(|| RailError::message(format!("missing manifest entry for member '{member}'")))?;

                let key = self.key_for(manifests_member, target, features.clone())?;

                let mut unused = BTreeSet::new();
                let mut compiled = BTreeSet::new();

                if completeness == DiagnosticsCompleteness::Complete
                    && let Some(parsed_member) = parsed.get(member)
                {
                    compiled = parsed_member.compiled_targets.clone();
                }

                let unit_evidence = parsed
                    .get(member)
                    .map(ParsedMemberTarget::unit_evidence)
                    .unwrap_or_default();
                let normal_units: Vec<_> = compiled
                    .iter()
                    .filter(|unit| !unit.test_mode && unit.kind != CargoTargetKind::CustomBuild)
                    .collect();
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
                let observations: Vec<CompilationObservationManifest> = self
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

                update_candidate_survivors(
                    &mut surviving_unused,
                    &candidate_targets,
                    member,
                    target,
                    &entry.evidence,
                );

                record_target_evidence(&mut result, &manifests_member.package_id, &entry.evidence);
                store.put(entry);
            }
        }

        store.flush()?;
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
        view: &AnalysisView,
        cargo_members: &[&str],
        typed_members: &BTreeSet<String>,
        producer_authority: &crate::compiler::facts::CompilerFactProducerAuthority,
    ) -> RailResult<CompilerFactCacheKey> {
        let packages = cargo_members
            .iter()
            .map(|member| {
                let manifest = self
                    .manifests
                    .members
                    .iter()
                    .find(|manifest| manifest.package_name == *member)
                    .ok_or_else(|| RailError::message(format!("missing manifest entry for member '{member}'")))?;
                self.key_for(manifest, view.platform().as_str(), view.features().clone())
            })
            .collect::<RailResult<Vec<_>>>()?;
        CompilerFactCacheKey::new(
            view.fact_cache_identity(cargo_members, typed_members)?,
            packages,
            typed_members.clone(),
            producer_authority.clone(),
            required_compiler_fact_coverage(),
        )
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
    if evidence.completeness != DiagnosticsCompleteness::Complete || evidence.compiled_units.is_empty() {
        return;
    }
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
        !applicable || evidence.dependency_state_for_kind(&candidate.1, candidate.0) != DependencyEvidenceState::Used
    });
}

struct WorkspaceCheckOutput {
    stdout: String,
    stderr: String,
    success: bool,
    invocations: Vec<crate::compiler::observation::RawCompilerInvocation>,
    compiler_facts: Vec<ValidatedCompilerFactFragment>,
}

struct TypedAcquisitionContext<'a> {
    snapshot: &'a WorkspaceSnapshot,
    driver: &'a PreparedCompilerFactDriver,
    doctest_sysroot: Option<&'a CompilerFactDoctestSysroot>,
    cargo_target: &'a Path,
    packages: &'a BTreeSet<String>,
}

fn run_workspace_check(
    workspace_root: &Path,
    identity: &CompilerCacheIdentity,
    view: &AnalysisView,
    members: &[&str],
    typed: Option<&TypedAcquisitionContext<'_>>,
) -> RailResult<WorkspaceCheckOutput> {
    let wrapper = compiler_observation_wrapper()?;
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
    let typed_cargo_target = typed.map(|typed| typed.cargo_target);
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
                typed_cargo_target.ok_or_else(|| RailError::message("typed Cargo output authority disappeared"))?,
                doctest_sysroot,
            )
        })
        .transpose()?;
    let fact_families = if typed.is_some() {
        view.fact_families().clone()
    } else {
        BTreeSet::from([crate::compiler::scheduler::CompilerFactFamily::StableDiagnostics])
    };
    let source_root = typed.map_or(workspace_root, |typed| typed.snapshot.source_root());
    let fact_session = CompilerFactSession::write_with_typed(
        observation_directory.path(),
        source_root,
        &fact_families,
        typed_session.clone(),
    )?;
    let args = view.cargo_arguments(members)?;

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
        .env_remove(CACHE_WRAPPER_MARKER)
        .args(&args);
    if let Some(target) = &typed_cargo_target {
        command.env("CARGO_TARGET_DIR", target);
    }
    if view.compiles_doctests() {
        command
            .env("RUSTDOC", &workspace_wrapper)
            .env(INNER_RUSTDOC_ENV, &identity.rustdoc_program)
            .env(RUSTDOC_WRAPPER_MARKER, "1");
    }
    if typed.is_some() && existing_workspace_wrapper.is_some() {
        return Err(RailError::message(
            "typed compiler fact acquisition cannot compose with a configured workspace wrapper",
        ));
    }
    if let Some(inner_wrapper) = existing_workspace_wrapper
        && inner_wrapper != wrapper.as_os_str()
    {
        command.env(INNER_WRAPPER_ENV, inner_wrapper);
    }

    #[cfg(test)]
    QUALIFICATION_CARGO_VIEWS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let output = command.output().with_context(|| {
        format!(
            "running cargo check for target '{target}' in {}",
            workspace_root.display(),
            target = view.platform().as_str()
        )
    })?;
    if typed_session.is_some() && !output.status.success() {
        let diagnostics = cargo_failure_diagnostics(&output.stdout);
        return Err(RailError::message(format!(
            "typed Cargo acquisition failed with status {}: {}{}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim(),
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
    #[cfg(test)]
    QUALIFICATION_COMPILER_INVOCATIONS.fetch_add(invocations.len(), std::sync::atomic::Ordering::Relaxed);
    let compiler_facts = typed_session.as_ref().map_or_else(
    || Ok(Vec::new()),
    |typed| {
      let expected_artifacts =
        selected_typed_artifact_count(&String::from_utf8_lossy(&output.stdout), source_root, typed)?;
      let fragments = load_compiler_fact_fragments(
        &String::from_utf8_lossy(&output.stdout),
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
    Ok(WorkspaceCheckOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        success: output.status.success(),
        invocations,
        compiler_facts,
    })
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

fn fact_invocation_cache_bypasses(
    invocations: &[crate::compiler::observation::RawCompilerInvocation],
) -> BTreeSet<String> {
    let mut observed = false;
    let mut bypasses = BTreeSet::new();
    for invocation in invocations
        .iter()
        .filter(|invocation| invocation.compiler_fact_unit.is_some())
    {
        observed = true;
        if !invocation.success {
            bypasses.insert("compiler_invocation_failed".to_string());
        }
        bypasses.extend(invocation.bypasses.iter().cloned());
    }
    if !observed {
        bypasses.insert("no_typed_compiler_invocation".to_string());
    }
    bypasses
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
    view: &AnalysisView,
    members: &[&str],
    observation_directory: &Path,
    typed_cargo_target: &Path,
    doctest_sysroot: Option<&CompilerFactDoctestSysroot>,
) -> RailResult<CompilerFactTypedSession> {
    if !view
        .fact_families()
        .contains(&crate::compiler::scheduler::CompilerFactFamily::TypedRustItems)
    {
        return Err(RailError::message(
            "typed compiler driver was supplied to a view that does not request typed facts",
        ));
    }
    let targets = CompilerFactTypedSession::targets_from_snapshot(context.snapshot, context.packages)?;
    let view_identity = view.fact_cache_identity(members, context.packages)?;
    let mut hasher = Sha256::new();
    hasher.update(b"cargo-rail-compiler-fact-run-v1\0");
    hasher.update((view_identity.len() as u64).to_le_bytes());
    hasher.update(view_identity.as_bytes());
    hasher.update((observation_directory.as_os_str().as_encoded_bytes().len() as u64).to_le_bytes());
    hasher.update(observation_directory.as_os_str().as_encoded_bytes());
    let run_identity = format!(
        "{RUN_IDENTITY_PREFIX}{}",
        ContentDigest::from_sha256_bytes(hasher.finalize().into())
    );
    let host_platform = context.snapshot.toolchain().host_target().to_string();
    let target_platform = if view.platform().as_str() == "default" {
        host_platform.clone()
    } else {
        view.platform().as_str().to_string()
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
    let generated_roots = vec![typed_cargo_target.to_path_buf()];
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
        if !invocation.success {
            return Err(RailError::message(
                "typed compiler fact invocation failed before publishing complete facts",
            ));
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
        .find(|target| target.package == unit.package && target.cargo_target == unit.cargo_target)
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
    if !kind_matches || target.target_kind != unit.target_kind {
        return Err(RailError::message(
            "compiler fact announcement does not match Cargo's target kind",
        ));
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
    let mut framed = Vec::from(&b"cargo-rail-executable-toolchain-v2\0"[..]);
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

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
const SYSROOT_MEMO_VERSION: u32 = 3;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
const MAX_SYSROOT_MEMO_BYTES: u64 = 4 * 1024 * 1024;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
const MAX_GENERATION_IDENTIFIER_BYTES: usize = 256;
#[cfg(any(windows, test))]
const WINDOWS_SYSROOT_CAPTURE_ATTEMPTS: usize = 3;
const MAX_SYSROOT_FILES: usize = 4096;
const MAX_SYSROOT_BYTES: u64 = 1024 * 1024 * 1024;

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
    let windows_before = capture_exact_sysroot_evidence(&inventory)
        .ok_or_else(|| RailError::message("native cache cannot prove a stable local NTFS compiler sysroot"))?;

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
    for _ in 0..WINDOWS_SYSROOT_CAPTURE_ATTEMPTS {
        if let Some(captured) = capture()? {
            return Ok(captured);
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
    for entry in std::fs::read_dir(&target_lib)? {
        let path = entry?.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(RailError::message(
                "compiler target sysroot contains a non-regular entry",
            ));
        }
        files.push(path);
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
        if total > MAX_SYSROOT_BYTES {
            return Err(RailError::message("compiler sysroot identity exceeds its byte limit"));
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
        let digest = ContentDigest::from_sha256_bytes(hasher.finalize().into());
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

fn transparent_native_compiler_process_env_fingerprint() -> RailResult<String> {
    let runtime = std::env::vars_os()
        .filter_map(|(name, value)| {
            let name = name.into_string().ok()?;
            native_compiler_process_environment(&name).then(|| {
                (
                    name,
                    Some(format!("sha256:{}", ContentDigest::sha256(value.as_encoded_bytes()))),
                )
            })
        })
        .collect::<BTreeMap<_, _>>();
    #[cfg(unix)]
    let default_regular_file_mode = transparent_default_regular_file_creation_mode();
    #[cfg(not(unix))]
    let default_regular_file_mode = 0o644_u32;
    let mut framed = Vec::from(&b"cargo-rail-native-compiler-process-environment-v2\0"[..]);
    append_identity_frame(&mut framed, b"runtime", &serde_json::to_vec(&runtime)?);
    append_identity_frame(
        &mut framed,
        b"default-regular-file-mode",
        &default_regular_file_mode.to_le_bytes(),
    );
    Ok(format!("sha256:{}", ContentDigest::sha256(&framed)))
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
                diagnostic_views + typed_views
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
    fn compiler_sysroot_identity_rehashes_target_and_driver_bytes() {
        let sysroot = tempfile::tempdir().expect("sysroot");
        let target_lib = sysroot.path().join("lib/rustlib/test-host/lib");
        std::fs::create_dir_all(&target_lib).expect("target lib");
        std::fs::write(target_lib.join("libcore-test.rlib"), b"target-one").expect("target library");
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
        assert_eq!(baseline.1, 20);
        std::fs::write(target_lib.join("libcore-test.rlib"), b"target-two").expect("target mutation");
        let target_changed =
            compiler_sysroot_fingerprint(sysroot.path(), "test-host", None).expect("target fingerprint");
        assert_ne!(baseline.0, target_changed.0);
        std::fs::write(&driver, b"driver-two").expect("driver mutation");
        let driver_changed =
            compiler_sysroot_fingerprint(sysroot.path(), "test-host", None).expect("driver fingerprint");
        assert_ne!(target_changed.0, driver_changed.0);
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
