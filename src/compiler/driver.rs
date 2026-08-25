//! Authenticated distribution boundary for toolchain-matched fact drivers.
//!
//! Repository configuration cannot select or download a driver. A release
//! build either embeds one complete component authority or embeds none. Source
//! installations therefore perform no driver filesystem work unless their
//! builder deliberately supplied an exact authenticated component.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::{self, File};
#[cfg(unix)]
use std::io::Seek as _;
use std::io::{ErrorKind, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest as _, Sha256};

use crate::cargo::ToolchainIdentity;
use crate::compiler::facts::{
    COMPILER_FACT_PROTOCOL_VERSION, COMPILER_IDENTITY_PREFIX, CompilerFactProducerAuthority, DRIVER_IDENTITY_PREFIX,
};
use crate::error::{RailError, RailResult};
use crate::source::ContentDigest;
use crate::workspace::WorkspaceSnapshot;

const MAX_FACT_DRIVER_BYTES: u64 = 256 * 1024 * 1024;
const MAX_FACT_DRIVER_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_COMPILER_LIBRARY_BYTES: u64 = 1024 * 1024 * 1024;
const FACT_DRIVER_PROTOCOL_ARGUMENT: &str = "--cargo-rail-fact-protocol-version";
#[cfg(windows)]
const MAX_DOCTEST_EXECUTABLE_BYTES: u64 = 1024 * 1024 * 1024;
const COMPILED_TARGET: &str = env!("CARGO_RAIL_COMPILED_TARGET");

const FACT_DRIVER_FILE: Option<&str> = option_env!("CARGO_RAIL_FACT_DRIVER_FILE");
const FACT_DRIVER_SHA256: Option<&str> = option_env!("CARGO_RAIL_FACT_DRIVER_SHA256");
const FACT_DRIVER_PROVENANCE: Option<&str> = option_env!("CARGO_RAIL_FACT_DRIVER_PROVENANCE");
const FACT_DRIVER_RUSTC_RELEASE: Option<&str> = option_env!("CARGO_RAIL_FACT_DRIVER_RUSTC_RELEASE");
const FACT_DRIVER_RUSTC_COMMIT: Option<&str> = option_env!("CARGO_RAIL_FACT_DRIVER_RUSTC_COMMIT");
const FACT_DRIVER_RUSTC_HOST: Option<&str> = option_env!("CARGO_RAIL_FACT_DRIVER_RUSTC_HOST");
const FACT_DRIVER_COMPILER_LIBRARY: Option<&str> = option_env!("CARGO_RAIL_FACT_DRIVER_COMPILER_LIBRARY");
const FACT_DRIVER_COMPILER_LIBRARY_SHA256: Option<&str> = option_env!("CARGO_RAIL_FACT_DRIVER_COMPILER_LIBRARY_SHA256");
const FACT_DRIVER_SOURCE_FILE: Option<&str> = option_env!("CARGO_RAIL_FACT_DRIVER_SOURCE_FILE");
const FACT_DRIVER_SOURCE_SHA256: Option<&str> = option_env!("CARGO_RAIL_FACT_DRIVER_SOURCE_SHA256");
const FACT_DRIVER_SOURCE_PROVENANCE: Option<&str> = option_env!("CARGO_RAIL_FACT_DRIVER_SOURCE_PROVENANCE");

/// Build-time release authority for exactly one sibling component.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompilerFactDriverAuthority {
    file_name: String,
    content_digest: String,
    provenance: String,
    rustc_release: String,
    rustc_commit: String,
    rustc_host: String,
    compiler_library: String,
    compiler_library_digest: String,
    identity: String,
}

#[derive(Debug, Clone)]
struct CompilerFactDriverSourceAuthority {
    file_name: String,
    content_digest: String,
    provenance: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CompilerFactDriverSourceBundle {
    version: u32,
    files: Vec<CompilerFactDriverSourceFile>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CompilerFactDriverSourceFile {
    path: String,
    hex: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CachedCompilerFactDriver {
    version: u32,
    source_digest: String,
    rustc_verbose: String,
    authority: CompilerFactDriverAuthority,
}

struct SelectedCompilerLibrary {
    path: PathBuf,
    relative: String,
    content_digest: String,
    rustup_toolchain: Option<String>,
}

/// Exact sibling component bytes accepted by embedded release authority.
pub(crate) struct CompilerFactDriverComponent {
    authority: CompilerFactDriverAuthority,
    path: PathBuf,
    compiler_library_directory: PathBuf,
    compiler_library_path: PathBuf,
}

/// Authenticated Surface producer readiness for one exact selected toolchain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompilerFactDriverReadiness {
    pub(crate) protocol: u32,
    pub(crate) driver_identity: String,
    pub(crate) driver_digest: String,
    pub(crate) compiler_library_digest: String,
    pub(crate) rustc_release: String,
    pub(crate) rustc_commit: String,
    pub(crate) rustc_host: String,
}

/// Runtime-library bytes authenticated once and retained through doctest staging.
struct AuthenticatedCompilerLibrary {
    path: PathBuf,
    #[cfg(unix)]
    file: File,
    #[cfg(unix)]
    generation: Vec<u8>,
    #[cfg(unix)]
    bytes: u64,
}

/// Handle-bound executable bytes retained for the lifetime of a compiler run.
pub(crate) struct CompilerFactDriverExecutionCapability {
    program: PathBuf,
    identity: String,
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    _file: File,
    #[cfg(windows)]
    _directory_file: File,
    #[cfg(windows)]
    _directory: tempfile::TempDir,
    #[cfg(target_os = "macos")]
    _directory: tempfile::TempDir,
    #[cfg(target_os = "macos")]
    sandbox_profile: String,
}

/// Authenticated compiler/driver pair retained across one combined acquisition.
pub(crate) struct PreparedCompilerFactDriver {
    execution: CompilerFactDriverExecutionCapability,
    producer_authority: CompilerFactProducerAuthority,
    compiler_library_directory: PathBuf,
    compiler_library: AuthenticatedCompilerLibrary,
    compiler_library_digest: String,
}

/// Private stable-rustdoc sysroot view whose test builder is cargo-rail.
pub(crate) struct CompilerFactDoctestSysroot {
    root: PathBuf,
    #[cfg(unix)]
    rustc_target: PathBuf,
    #[cfg(unix)]
    rustdoc_target: PathBuf,
    library_target: PathBuf,
    runtime_library: PathBuf,
    #[cfg(unix)]
    runtime_library_generation: Option<Vec<u8>>,
    #[cfg(unix)]
    runtime_library_bytes: u64,
    _runtime_library_file: File,
    #[cfg(windows)]
    _root_guard: File,
    #[cfg(windows)]
    _bin_guard: File,
    #[cfg(windows)]
    _rustc_guard: File,
    #[cfg(windows)]
    _rustdoc_guard: File,
    #[cfg(windows)]
    _library_junction_guard: File,
    #[cfg(windows)]
    _library_target_guard: File,
    _directory: tempfile::TempDir,
}

impl CompilerFactDriverAuthority {
    /// Fail before workspace acquisition when an installed surface command
    /// cannot authenticate its companion producer.
    pub(crate) fn require_surface_installation() -> RailResult<()> {
        if Self::embedded()?.is_some() || CompilerFactDriverSourceAuthority::embedded()?.is_some() {
            Ok(())
        } else {
            Err(RailError::with_help(
                "surface is unavailable in this source-built cargo-rail installation",
                "install a supported native cargo-rail archive with its adjacent authenticated compiler-fact driver; cargo install does not provide surface",
            ))
        }
    }

    /// Prepare and authenticate the complete producer capability without
    /// acquiring workspace compiler facts.
    pub(crate) fn prepare_surface(snapshot: &WorkspaceSnapshot) -> RailResult<CompilerFactDriverReadiness> {
        let cargo_rail_executable = std::env::current_exe()
            .map_err(|error| RailError::message(format!("failed to locate cargo-rail executable: {error}")))?;
        let component = CompilerFactDriverComponent::discover(snapshot, &cargo_rail_executable)?.ok_or_else(|| {
            RailError::with_help(
                "this cargo-rail installation does not include an authenticated compiler fact driver",
                "install a native cargo-rail release archive for the selected host",
            )
        })?;
        let compiler_library = authenticate_compiler_library(
            &component.compiler_library_path,
            &component.authority.compiler_library_digest,
        )?;
        let execution = component.stage()?;
        if execution.identity() != component.identity() {
            return Err(RailError::message(
                "staged compiler fact driver does not match its authenticated component authority",
            ));
        }
        probe_fact_driver_protocol(&execution, component.compiler_library_directory())?;
        drop(compiler_library);

        Ok(CompilerFactDriverReadiness {
            protocol: COMPILER_FACT_PROTOCOL_VERSION,
            driver_identity: component.authority.identity,
            driver_digest: component.authority.content_digest,
            compiler_library_digest: component.authority.compiler_library_digest,
            rustc_release: component.authority.rustc_release,
            rustc_commit: component.authority.rustc_commit,
            rustc_host: component.authority.rustc_host,
        })
    }

    fn embedded() -> RailResult<Option<Self>> {
        Self::from_fields([
            FACT_DRIVER_FILE,
            FACT_DRIVER_SHA256,
            FACT_DRIVER_PROVENANCE,
            FACT_DRIVER_RUSTC_RELEASE,
            FACT_DRIVER_RUSTC_COMMIT,
            FACT_DRIVER_RUSTC_HOST,
            FACT_DRIVER_COMPILER_LIBRARY,
            FACT_DRIVER_COMPILER_LIBRARY_SHA256,
        ])
    }

    fn from_fields(fields: [Option<&str>; 8]) -> RailResult<Option<Self>> {
        if fields.iter().all(Option::is_none) {
            return Ok(None);
        }
        let [
            Some(file_name),
            Some(content_digest),
            Some(provenance),
            Some(rustc_release),
            Some(rustc_commit),
            Some(rustc_host),
            Some(compiler_library),
            Some(compiler_library_digest),
        ] = fields
        else {
            return Err(RailError::message("compiler fact driver build authority is incomplete"));
        };
        let authority = Self {
            file_name: file_name.to_string(),
            content_digest: content_digest.to_string(),
            provenance: provenance.to_string(),
            rustc_release: rustc_release.to_string(),
            rustc_commit: rustc_commit.to_string(),
            rustc_host: rustc_host.to_string(),
            compiler_library: compiler_library.to_string(),
            compiler_library_digest: compiler_library_digest.to_string(),
            identity: String::new(),
        };
        authority.validate()?;
        let identity = authority.calculate_identity();
        Ok(Some(Self { identity, ..authority }))
    }

    fn validate(&self) -> RailResult<()> {
        if self.file_name != expected_driver_file_name() {
            return Err(RailError::message(
                "compiler fact driver build authority has an invalid component file name",
            ));
        }
        validate_sha256(&self.content_digest, "compiler fact driver content digest")?;
        validate_sha256(&self.provenance, "compiler fact driver provenance")?;
        semver::Version::parse(&self.rustc_release)
            .map_err(|error| RailError::message(format!("compiler fact driver rustc release is invalid: {error}")))?;
        if !valid_hex(&self.rustc_commit, 40) {
            return Err(RailError::message(
                "compiler fact driver rustc commit is not a lowercase 40-digit hash",
            ));
        }
        if self.rustc_host != COMPILED_TARGET {
            return Err(RailError::message(format!(
                "compiler fact driver host '{}' does not match cargo-rail build target '{COMPILED_TARGET}'",
                self.rustc_host
            )));
        }
        let compiler_library = Path::new(&self.compiler_library);
        if compiler_library.is_absolute()
            || self.compiler_library.contains(['\\', '\0'])
            || compiler_library
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(RailError::message(
                "compiler fact driver runtime library path is not a canonical relative path",
            ));
        }
        validate_sha256(
            &self.compiler_library_digest,
            "compiler fact driver runtime library digest",
        )?;
        Ok(())
    }

    fn validate_toolchain(&self, toolchain: &ToolchainIdentity) -> RailResult<PathBuf> {
        self.validate_toolchain_identity(toolchain)?;
        let library = toolchain.direct_rustc_sysroot().join(&self.compiler_library);
        library
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| RailError::message("compiler fact runtime library has no parent directory"))
    }

    fn validate_toolchain_identity(&self, toolchain: &ToolchainIdentity) -> RailResult<()> {
        let selected = RustcVerboseIdentity::parse(toolchain.direct_rustc_verbose_version())?;
        if selected.release != self.rustc_release
            || selected.commit != self.rustc_commit
            || selected.host != self.rustc_host
        {
            return Err(RailError::with_help(
                format!(
                    "compiler fact driver supports rustc {} ({}, {}), but Cargo selected rustc {} ({}, {})",
                    self.rustc_release,
                    self.rustc_commit,
                    self.rustc_host,
                    selected.release,
                    selected.commit,
                    selected.host
                ),
                "select the exact supported toolchain or install a cargo-rail release that bundles its authenticated driver",
            ));
        }
        Ok(())
    }

    fn calculate_identity(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"cargo-rail-compiler-fact-driver-authority-v1\0");
        for (name, value) in [
            (b"protocol".as_slice(), COMPILER_FACT_PROTOCOL_VERSION.to_string()),
            (b"file".as_slice(), self.file_name.clone()),
            (b"content".as_slice(), self.content_digest.clone()),
            (b"provenance".as_slice(), self.provenance.clone()),
            (b"rustc-release".as_slice(), self.rustc_release.clone()),
            (b"rustc-commit".as_slice(), self.rustc_commit.clone()),
            (b"rustc-host".as_slice(), self.rustc_host.clone()),
            (b"compiler-library".as_slice(), self.compiler_library.clone()),
            (
                b"compiler-library-content".as_slice(),
                self.compiler_library_digest.clone(),
            ),
        ] {
            hasher.update((name.len() as u64).to_le_bytes());
            hasher.update(name);
            hasher.update((value.len() as u64).to_le_bytes());
            hasher.update(value.as_bytes());
        }
        format!(
            "{DRIVER_IDENTITY_PREFIX}{}",
            ContentDigest::from_sha256_bytes(hasher.finalize().into())
        )
    }

    /// Derive cache producer authority without staging or reading component bytes.
    ///
    /// The embedded release authority already binds those bytes. A cache miss
    /// performs full component and runtime-library authentication immediately
    /// before execution.
    pub(crate) fn producer_authority(
        snapshot: &WorkspaceSnapshot,
        compiler_identity_seed: &str,
    ) -> RailResult<CompilerFactProducerAuthority> {
        let cargo_rail_executable = std::env::current_exe()
            .map_err(|error| RailError::message(format!("failed to locate cargo-rail executable: {error}")))?;
        let driver_identity = if let Some(authority) = CompilerFactDriverAuthority::embedded()?
            && authority.validate_toolchain_identity(snapshot.toolchain()).is_ok()
        {
            authority.identity
        } else if let Some(source) = CompilerFactDriverSourceAuthority::embedded()? {
            runtime_compiler_fact_driver(snapshot, &cargo_rail_executable, &source)?
                .authority
                .identity
        } else if let Some(authority) = CompilerFactDriverAuthority::embedded()? {
            authority.validate_toolchain_identity(snapshot.toolchain())?;
            authority.identity
        } else {
            return Err(RailError::with_help(
                "this cargo-rail installation does not include compiler fact producer authority",
                "install a complete native cargo-rail release archive with its authenticated driver source component",
            ));
        };
        Ok(CompilerFactProducerAuthority {
            compiler_identity: format!(
                "{COMPILER_IDENTITY_PREFIX}{}",
                ContentDigest::sha256(compiler_identity_seed.as_bytes())
            ),
            driver_identity,
        })
    }
}

fn probe_fact_driver_protocol(
    execution: &CompilerFactDriverExecutionCapability,
    compiler_library_directory: &Path,
) -> RailResult<()> {
    let mut command = Command::new(execution.program());
    command.arg(FACT_DRIVER_PROTOCOL_ARGUMENT);
    #[cfg(target_os = "macos")]
    command.env("DYLD_LIBRARY_PATH", compiler_library_directory);
    #[cfg(all(unix, not(target_os = "macos")))]
    command.env("LD_LIBRARY_PATH", compiler_library_directory);
    #[cfg(windows)]
    {
        let mut paths = vec![compiler_library_directory.to_path_buf()];
        if let Some(existing) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&existing));
        }
        command.env(
            "PATH",
            std::env::join_paths(paths)
                .map_err(|error| RailError::message(format!("failed to prepare compiler fact driver PATH: {error}")))?,
        );
    }
    let output = command
        .output()
        .map_err(|error| RailError::message(format!("failed to probe staged compiler fact driver: {error}")))?;
    let expected = COMPILER_FACT_PROTOCOL_VERSION.to_string();
    if !output.status.success()
        || !output.stderr.is_empty()
        || String::from_utf8(output.stdout).map(|stdout| stdout.trim().to_string()) != Ok(expected)
    {
        return Err(RailError::with_help(
            "staged compiler fact driver failed its authenticated protocol readiness probe",
            "reinstall the complete native Cargo-Rail archive and retry Surface preparation",
        ));
    }
    Ok(())
}

impl CompilerFactDriverSourceAuthority {
    fn embedded() -> RailResult<Option<Self>> {
        match [
            FACT_DRIVER_SOURCE_FILE,
            FACT_DRIVER_SOURCE_SHA256,
            FACT_DRIVER_SOURCE_PROVENANCE,
        ] {
            [None, None, None] => Ok(None),
            [Some(file_name), Some(content_digest), Some(provenance)] => {
                let source = Self {
                    file_name: file_name.to_string(),
                    content_digest: content_digest.to_string(),
                    provenance: provenance.to_string(),
                };
                source.validate()?;
                Ok(Some(source))
            }
            _ => Err(RailError::message(
                "compiler fact driver source authority is incomplete",
            )),
        }
    }

    fn validate(&self) -> RailResult<()> {
        if self.file_name != "cargo-rail-fact-driver-source-v1.json" {
            return Err(RailError::message(
                "compiler fact driver source authority has an invalid component file name",
            ));
        }
        validate_sha256(&self.content_digest, "compiler fact driver source digest")?;
        validate_sha256(&self.provenance, "compiler fact driver source provenance")
    }
}

impl CompilerFactDriverComponent {
    /// Authenticate the release sibling selected at build time.
    pub(crate) fn discover(snapshot: &WorkspaceSnapshot, cargo_rail_executable: &Path) -> RailResult<Option<Self>> {
        let toolchain = snapshot.toolchain();
        if let Some(authority) = CompilerFactDriverAuthority::embedded()?
            && authority.validate_toolchain_identity(toolchain).is_ok()
        {
            let compiler_library_directory = ensure_authority_compiler_library(snapshot, &authority)?;
            return Self::discover_with_authority(&authority, cargo_rail_executable, compiler_library_directory)
                .map(Some);
        }
        if let Some(source) = CompilerFactDriverSourceAuthority::embedded()? {
            return runtime_compiler_fact_driver(snapshot, cargo_rail_executable, &source).map(Some);
        }
        if let Some(authority) = CompilerFactDriverAuthority::embedded()? {
            authority.validate_toolchain_identity(toolchain)?;
        }
        Ok(None)
    }

    fn discover_with_authority(
        authority: &CompilerFactDriverAuthority,
        cargo_rail_executable: &Path,
        compiler_library_directory: PathBuf,
    ) -> RailResult<Self> {
        let executable = crate::utils::canonicalize_existing(cargo_rail_executable).map_err(|error| {
            RailError::message(format!(
                "failed to locate cargo-rail while authenticating its compiler fact driver: {error}"
            ))
        })?;
        let directory = executable.parent().ok_or_else(|| {
            RailError::message("cargo-rail executable has no parent directory for its compiler fact driver")
        })?;
        let path = directory.join(&authority.file_name);
        authenticate_component_file(&path, &authority.content_digest)?;
        let compiler_library_path = compiler_library_directory.join(
            Path::new(&authority.compiler_library)
                .file_name()
                .ok_or_else(|| RailError::message("compiler fact runtime library has no file name"))?,
        );
        Ok(Self {
            authority: authority.clone(),
            path,
            compiler_library_directory,
            compiler_library_path,
        })
    }

    pub(crate) fn identity(&self) -> &str {
        &self.authority.identity
    }

    pub(crate) fn compiler_library_directory(&self) -> &Path {
        &self.compiler_library_directory
    }

    /// Copy and reauthenticate the component before repository code can execute.
    pub(crate) fn stage(&self) -> RailResult<CompilerFactDriverExecutionCapability> {
        stage_component(self)
    }
}

fn runtime_compiler_fact_driver(
    snapshot: &WorkspaceSnapshot,
    cargo_rail_executable: &Path,
    source: &CompilerFactDriverSourceAuthority,
) -> RailResult<CompilerFactDriverComponent> {
    let executable = crate::utils::canonicalize_existing(cargo_rail_executable)?;
    let source_path = executable
        .parent()
        .ok_or_else(|| RailError::message("cargo-rail executable has no component directory"))?
        .join(&source.file_name);
    let source_bytes =
        read_authenticated_component(&source_path, &source.content_digest, MAX_FACT_DRIVER_SOURCE_BYTES)?;
    let bundle: CompilerFactDriverSourceBundle = serde_json::from_slice(&source_bytes)?;
    validate_source_bundle(&bundle)?;
    let selected = RustcVerboseIdentity::parse(snapshot.toolchain().direct_rustc_verbose_version())?;
    let compiler_library = selected_compiler_library(snapshot, selected.host)?;
    let compiler_library_path = compiler_library.path.clone();
    let cache_key = ContentDigest::sha256(
        format!(
            "cargo-rail-runtime-fact-driver-v1\0{}\0{}\0{}\0{}",
            source.content_digest,
            snapshot.toolchain().direct_rustc_verbose_version(),
            compiler_library.content_digest,
            COMPILER_FACT_PROTOCOL_VERSION
        )
        .as_bytes(),
    );
    let cargo_home = crate::cargo::CargoConfigSnapshot::cargo_home(snapshot.cargo_current_dir())?;
    let cargo_home = if cargo_home.is_absolute() {
        cargo_home
    } else {
        snapshot.cargo_current_dir().join(cargo_home)
    };
    fs::create_dir_all(&cargo_home)?;
    let cargo_home = crate::utils::canonicalize_existing(&cargo_home)?;
    let owner = create_private_real_directory(&cargo_home, "cargo-rail")?;
    let cache = create_private_real_directory(&owner, "fact-drivers-v1")?;
    let entry = cache.join(format!("driver-{cache_key}"));
    let lock_path = cache.join(format!("driver-{cache_key}.lock"));
    let lock = crate::utils::open_cache_lock_file(&lock_path, true)?;
    lock.lock()?;
    if let Some(component) = load_cached_runtime_driver(&entry, source, snapshot.toolchain(), &compiler_library_path)? {
        return Ok(component);
    }

    let build = tempfile::Builder::new()
        .prefix(".cargo-rail-fact-driver-build-")
        .tempdir_in(&cache)?;
    extract_source_bundle(&bundle, build.path())?;
    let manifest = build.path().join("tools/compiler-fact-driver/Cargo.toml");
    let target = build.path().join("target");
    let rustc = snapshot
        .toolchain()
        .direct_rustc_sysroot()
        .join("bin")
        .join(if cfg!(windows) { "rustc.exe" } else { "rustc" });
    let mut command = Command::new(snapshot.toolchain().cargo_program());
    command
        .current_dir(build.path())
        .args(["build", "--release", "--frozen", "--manifest-path"])
        .arg(&manifest)
        .args(["--target", selected.host])
        .env("RUSTC", &rustc)
        .env("RUSTC_BOOTSTRAP", "cargo_rail_fact_driver")
        .env("CARGO_TARGET_DIR", &target)
        .env(
            "RUSTFLAGS",
            format!(
                "--remap-path-prefix={}=/cargo-rail-fact-driver --remap-path-scope=object",
                build.path().display()
            ),
        )
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("CARGO_ENCODED_RUSTFLAGS");
    if let Some(toolchain) = &compiler_library.rustup_toolchain {
        command.env("RUSTUP_TOOLCHAIN", toolchain);
    }
    let output = command.output().map_err(|error| {
        RailError::message(format!(
            "failed to build the compiler fact driver for selected rustc {}: {error}",
            selected.release
        ))
    })?;
    if !output.status.success() {
        return Err(RailError::with_help(
            format!(
                "failed to build the compiler fact driver for selected rustc {} ({}): {}",
                selected.release,
                selected.commit,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            "ensure the selected toolchain supports this authenticated offline driver source and retry Surface",
        ));
    }
    let built = target
        .join(selected.host)
        .join("release")
        .join(expected_driver_file_name());
    let content_digest = digest_regular_file(&built, MAX_FACT_DRIVER_BYTES)?;
    let authority = CompilerFactDriverAuthority {
        file_name: expected_driver_file_name().to_string(),
        content_digest,
        provenance: source.provenance.clone(),
        rustc_release: selected.release.to_string(),
        rustc_commit: selected.commit.to_string(),
        rustc_host: selected.host.to_string(),
        compiler_library: compiler_library.relative,
        compiler_library_digest: compiler_library.content_digest,
        identity: String::new(),
    };
    authority.validate()?;
    let authority = CompilerFactDriverAuthority {
        identity: authority.calculate_identity(),
        ..authority
    };
    let cached = CachedCompilerFactDriver {
        version: 1,
        source_digest: source.content_digest.clone(),
        rustc_verbose: snapshot.toolchain().direct_rustc_verbose_version().to_string(),
        authority: authority.clone(),
    };
    let staged = tempfile::Builder::new()
        .prefix(".cargo-rail-fact-driver-entry-")
        .tempdir_in(&cache)?;
    let staged_driver = staged.path().join(expected_driver_file_name());
    fs::copy(&built, &staged_driver)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(&staged_driver, fs::Permissions::from_mode(0o700))?;
    }
    authenticate_component_file(&staged_driver, &authority.content_digest)?;
    fs::write(staged.path().join("authority.json"), serde_json::to_vec(&cached)?)?;
    let staged = staged.keep();
    fs::rename(&staged, &entry).map_err(|error| {
        RailError::message(format!(
            "failed to commit selected-toolchain fact driver '{}': {error}",
            entry.display()
        ))
    })?;
    load_cached_runtime_driver(&entry, source, snapshot.toolchain(), &compiler_library_path)?
        .ok_or_else(|| RailError::message("selected-toolchain fact driver disappeared after commit"))
}

fn read_authenticated_component(path: &Path, expected_digest: &str, maximum_bytes: u64) -> RailResult<Vec<u8>> {
    let bytes = read_bounded_regular_file(path, maximum_bytes)?;
    let actual = format!("sha256:{}", ContentDigest::sha256(&bytes));
    if actual != expected_digest {
        return Err(RailError::message(format!(
            "authenticated component '{}' does not match embedded release authority",
            path.display()
        )));
    }
    Ok(bytes)
}

fn read_bounded_regular_file(path: &Path, maximum_bytes: u64) -> RailResult<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file()
        || crate::utils::is_symlink_or_reparse(&metadata)
        || metadata.len() == 0
        || metadata.len() > maximum_bytes
    {
        return Err(RailError::message(format!(
            "component '{}' is not a bounded real file",
            path.display()
        )));
    }
    let mut file = File::open(path)?;
    if !crate::utils::private_file_matches_path(&file, path, metadata.len())? {
        return Err(RailError::message(format!(
            "component '{}' changed before it was opened or has multiple links",
            path.display()
        )));
    }
    let capacity = usize::try_from(metadata.len())
        .map_err(|_| RailError::message("component byte length does not fit in memory"))?;
    let mut bytes = Vec::with_capacity(capacity);
    (&mut file)
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.len() || !crate::utils::private_file_matches_path(&file, path, metadata.len())? {
        return Err(RailError::message(format!(
            "component '{}' changed while its bytes were read",
            path.display()
        )));
    }
    crate::instrumentation::record_hash(bytes.len());
    crate::instrumentation::record_hashed_file_bytes_read(bytes.len());
    Ok(bytes)
}

fn digest_regular_file(path: &Path, maximum_bytes: u64) -> RailResult<String> {
    let bytes = read_bounded_regular_file(path, maximum_bytes)?;
    Ok(format!("sha256:{}", ContentDigest::sha256(&bytes)))
}

fn validate_source_bundle(bundle: &CompilerFactDriverSourceBundle) -> RailResult<()> {
    const MAX_SOURCE_FILES: usize = 10_000;
    const REQUIRED: [&str; 7] = [
        ".cargo/config.toml",
        "src/compiler/fact_protocol.rs",
        "tools/compiler-fact-driver/Cargo.lock",
        "tools/compiler-fact-driver/Cargo.toml",
        "tools/compiler-fact-driver/build.rs",
        "tools/compiler-fact-driver/src/collection.rs",
        "tools/compiler-fact-driver/src/main.rs",
    ];

    if bundle.version != 1 || bundle.files.is_empty() || bundle.files.len() > MAX_SOURCE_FILES {
        return Err(RailError::message(
            "compiler fact driver source bundle has an incompatible inventory",
        ));
    }
    let mut previous = None;
    let mut paths = BTreeSet::new();
    for file in &bundle.files {
        validate_source_path(&file.path)?;
        if previous.is_some_and(|path: &str| path >= file.path.as_str()) {
            return Err(RailError::message(
                "compiler fact driver source bundle paths are not unique and sorted",
            ));
        }
        decode_hex(&file.hex)?;
        previous = Some(file.path.as_str());
        paths.insert(file.path.as_str());
    }
    if REQUIRED.iter().any(|required| !paths.contains(required))
        || !paths.iter().any(|path| path.starts_with("vendor/"))
    {
        return Err(RailError::message(
            "compiler fact driver source bundle is missing a required build input",
        ));
    }
    Ok(())
}

fn validate_source_path(path: &str) -> RailResult<()> {
    let parsed = Path::new(path);
    if path.is_empty()
        || path.len() > 512
        || path.contains(['\\', '\0'])
        || parsed.is_absolute()
        || parsed
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(RailError::message(
            "compiler fact driver source bundle contains an invalid path",
        ));
    }
    Ok(())
}

fn decode_hex(hex: &str) -> RailResult<Vec<u8>> {
    if !hex.len().is_multiple_of(2)
        || hex
            .bytes()
            .any(|byte| !(byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    {
        return Err(RailError::message(
            "compiler fact driver source bundle contains invalid file bytes",
        ));
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for pair in hex.as_bytes().as_chunks::<2>().0 {
        bytes.push(hex_digit(pair[0]) * 16 + hex_digit(pair[1]));
    }
    Ok(bytes)
}

fn hex_digit(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => 0,
    }
}

fn extract_source_bundle(bundle: &CompilerFactDriverSourceBundle, root: &Path) -> RailResult<()> {
    for source in &bundle.files {
        let path = root.join(&source.path);
        let parent = path
            .parent()
            .ok_or_else(|| RailError::message("compiler fact driver source path has no parent"))?;
        fs::create_dir_all(parent)?;
        let bytes = decode_hex(&source.hex)?;
        let mut output = fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
        output.write_all(&bytes)?;
        output.sync_all()?;
    }
    Ok(())
}

fn create_private_real_directory(parent: &Path, name: &str) -> RailResult<PathBuf> {
    let path = parent.join(name);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() && !crate::utils::is_symlink_or_reparse(&metadata) => {}
        Ok(_) => {
            return Err(RailError::message(format!(
                "compiler fact driver cache path '{}' is not a real directory",
                path.display()
            )));
        }
        Err(error) if error.kind() == ErrorKind::NotFound => fs::create_dir(&path)?,
        Err(error) => return Err(error.into()),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    }
    crate::utils::canonicalize_existing(&path).map_err(Into::into)
}

fn selected_compiler_library(snapshot: &WorkspaceSnapshot, rustc_host: &str) -> RailResult<SelectedCompilerLibrary> {
    let sysroot = crate::utils::canonicalize_existing(snapshot.toolchain().direct_rustc_sysroot())?;
    let rustup_toolchain = rustup_toolchain_for_sysroot(snapshot, &sysroot)?;
    let mut libraries = compiler_libraries(&sysroot)?;
    let mut development_support = compiler_development_support_present(&sysroot, rustc_host)?;
    if libraries.is_empty() || !development_support {
        install_selected_rustc_dev(snapshot, &sysroot, rustup_toolchain.as_deref())?;
        libraries = compiler_libraries(&sysroot)?;
        development_support = compiler_development_support_present(&sysroot, rustc_host)?;
    }
    if !development_support {
        return Err(RailError::message(format!(
            "selected rustc sysroot '{}' has no compiler development metadata after rustc-dev preparation",
            sysroot.display()
        )));
    }
    if libraries.len() != 1 {
        return Err(RailError::message(format!(
            "expected exactly one rustc-dev compiler library in selected sysroot '{}', found {}",
            sysroot.display(),
            libraries.len()
        )));
    }
    let path = libraries.remove(0);
    let relative_path = path
        .strip_prefix(&sysroot)
        .map_err(|_| RailError::message("selected compiler library is outside its rustc sysroot"))?;
    let relative = relative_path
        .components()
        .map(|component| {
            component.as_os_str().to_str().ok_or_else(|| {
                RailError::message("selected compiler library path cannot be represented in component authority")
            })
        })
        .collect::<RailResult<Vec<_>>>()?
        .join("/");
    let content_digest = digest_regular_file(&path, MAX_COMPILER_LIBRARY_BYTES)?;
    Ok(SelectedCompilerLibrary {
        path,
        relative,
        content_digest,
        rustup_toolchain,
    })
}

fn compiler_development_support_present(sysroot: &Path, rustc_host: &str) -> RailResult<bool> {
    // Current rustc distributions can retain the loadable rustc_driver library after rustc-dev is removed.
    // rustc_hir metadata is a direct build input of the bundled driver and therefore proves the distinct
    // compiler-development capability that a runtime source build needs.
    let directory = sysroot.join("lib").join("rustlib").join(rustc_host).join("lib");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("librustc_hir-") || !(name.ends_with(".rmeta") || name.ends_with(".rlib")) {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) {
            return Err(RailError::message(
                "selected rustc-dev compiler metadata is not a real file",
            ));
        }
        return Ok(true);
    }
    Ok(false)
}

fn ensure_authority_compiler_library(
    snapshot: &WorkspaceSnapshot,
    authority: &CompilerFactDriverAuthority,
) -> RailResult<PathBuf> {
    let directory = authority.validate_toolchain(snapshot.toolchain())?;
    let library = snapshot
        .toolchain()
        .direct_rustc_sysroot()
        .join(&authority.compiler_library);
    match fs::symlink_metadata(&library) {
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let sysroot = crate::utils::canonicalize_existing(snapshot.toolchain().direct_rustc_sysroot())?;
            let rustup_toolchain = rustup_toolchain_for_sysroot(snapshot, &sysroot)?;
            install_selected_rustc_dev(snapshot, &sysroot, rustup_toolchain.as_deref())?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(directory)
}

fn install_selected_rustc_dev(
    snapshot: &WorkspaceSnapshot,
    sysroot: &Path,
    rustup_toolchain: Option<&str>,
) -> RailResult<()> {
    let toolchain = rustup_toolchain.ok_or_else(|| {
        RailError::with_help(
            format!(
                "selected rustc sysroot '{}' has no rustc-dev compiler library",
                sysroot.display()
            ),
            "install rustc-dev for the exact selected toolchain and retry Surface",
        )
    })?;
    let output = Command::new("rustup")
        .current_dir(snapshot.cargo_current_dir())
        .args(["component", "add", "rustc-dev", "--toolchain", toolchain])
        .output()
        .map_err(|error| RailError::message(format!("failed to invoke rustup for selected rustc-dev: {error}")))?;
    if !output.status.success() {
        return Err(RailError::with_help(
            format!(
                "rustup could not install rustc-dev for exact selected toolchain '{toolchain}': {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            "install that exact rustc-dev component and retry Surface; cargo-rail did not change the default toolchain",
        ));
    }
    Ok(())
}

fn compiler_libraries(sysroot: &Path) -> RailResult<Vec<PathBuf>> {
    let mut libraries = Vec::new();
    for directory in [sysroot.join("lib"), sysroot.join("bin")] {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !((name.starts_with("librustc_driver-") && (name.ends_with(".so") || name.ends_with(".dylib")))
                || (name.starts_with("rustc_driver-") && name.ends_with(".dll")))
            {
                continue;
            }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) {
                return Err(RailError::message(
                    "selected rustc-dev compiler library is not a real file",
                ));
            }
            libraries.push(path);
        }
    }
    libraries.sort();
    Ok(libraries)
}

fn rustup_toolchain_for_sysroot(snapshot: &WorkspaceSnapshot, sysroot: &Path) -> RailResult<Option<String>> {
    let Some(toolchains) = sysroot.parent() else {
        return Ok(None);
    };
    if toolchains.file_name() != Some(OsStr::new("toolchains")) {
        return Ok(None);
    }
    let Some(name) = sysroot.file_name().and_then(OsStr::to_str) else {
        return Ok(None);
    };
    if name.is_empty() || name.starts_with('-') || name.bytes().any(|byte| byte.is_ascii_control()) {
        return Ok(None);
    }
    let output = match Command::new("rustup")
        .current_dir(snapshot.cargo_current_dir())
        .args(["run", name, "rustc", "--print", "sysroot"])
        .output()
    {
        Ok(output) => output,
        Err(_) => return Ok(None),
    };
    if !output.status.success() {
        return Ok(None);
    }
    let reported = String::from_utf8(output.stdout)
        .map_err(|_| RailError::message("rustup reported a non-UTF-8 selected sysroot"))?;
    let reported = crate::utils::canonicalize_existing(Path::new(reported.trim()))?;
    if reported != sysroot {
        return Ok(None);
    }
    Ok(Some(name.to_string()))
}

fn load_cached_runtime_driver(
    entry: &Path,
    source: &CompilerFactDriverSourceAuthority,
    toolchain: &ToolchainIdentity,
    compiler_library_path: &Path,
) -> RailResult<Option<CompilerFactDriverComponent>> {
    let metadata = match fs::symlink_metadata(entry) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::message(
            "selected-toolchain fact driver cache entry is invalid",
        ));
    }
    let manifest_path = entry.join("authority.json");
    let manifest_bytes = read_bounded_regular_file(&manifest_path, 64 * 1024)?;
    let cached: CachedCompilerFactDriver = serde_json::from_slice(&manifest_bytes)?;
    if cached.version != 1
        || cached.source_digest != source.content_digest
        || cached.rustc_verbose != toolchain.direct_rustc_verbose_version()
        || serde_json::to_vec(&cached)? != manifest_bytes
    {
        return Err(RailError::message(
            "selected-toolchain fact driver cache authority is incompatible",
        ));
    }
    cached.authority.validate_toolchain_identity(toolchain)?;
    authenticate_component_file(
        &entry.join(expected_driver_file_name()),
        &cached.authority.content_digest,
    )?;
    if digest_regular_file(compiler_library_path, MAX_COMPILER_LIBRARY_BYTES)?
        != cached.authority.compiler_library_digest
    {
        return Err(RailError::message(
            "selected rustc-dev compiler library does not match cached driver authority",
        ));
    }
    Ok(Some(CompilerFactDriverComponent {
        compiler_library_directory: compiler_library_path
            .parent()
            .ok_or_else(|| RailError::message("compiler fact runtime library has no parent"))?
            .to_path_buf(),
        compiler_library_path: compiler_library_path.to_path_buf(),
        path: entry.join(expected_driver_file_name()),
        authority: cached.authority,
    }))
}

impl CompilerFactDriverExecutionCapability {
    pub(crate) fn program(&self) -> &Path {
        &self.program
    }

    pub(crate) fn identity(&self) -> &str {
        &self.identity
    }

    /// Launch Cargo inside the platform guard that protects this capability.
    pub(crate) fn cargo_command(&self, cargo: &OsStr) -> Command {
        #[cfg(target_os = "macos")]
        {
            let mut command = Command::new("/usr/bin/sandbox-exec");
            command.args(["-p", &self.sandbox_profile]).arg(cargo);
            command
        }
        #[cfg(not(target_os = "macos"))]
        {
            Command::new(cargo)
        }
    }
}

impl PreparedCompilerFactDriver {
    pub(crate) fn prepare(
        snapshot: &WorkspaceSnapshot,
        expected_producer: &CompilerFactProducerAuthority,
    ) -> RailResult<Self> {
        let cargo_rail_executable = std::env::current_exe()
            .map_err(|error| RailError::message(format!("failed to locate cargo-rail executable: {error}")))?;
        let component = CompilerFactDriverComponent::discover(snapshot, &cargo_rail_executable)?.ok_or_else(|| {
      RailError::with_help(
        "this cargo-rail installation does not include an authenticated compiler fact driver",
        "install a native cargo-rail release archive for the selected host, or build the isolated companion explicitly",
      )
    })?;
        if component.identity() != expected_producer.driver_identity {
            return Err(RailError::message(
                "authenticated compiler fact component does not match cache producer authority",
            ));
        }
        let compiler_library_directory = component.compiler_library_directory().to_path_buf();
        let compiler_library_digest = component.authority.compiler_library_digest.clone();
        let compiler_library =
            authenticate_compiler_library(&component.compiler_library_path, &compiler_library_digest)?;
        let execution = component.stage()?;
        let producer_authority = CompilerFactProducerAuthority {
            compiler_identity: expected_producer.compiler_identity.clone(),
            driver_identity: execution.identity().to_string(),
        };
        if &producer_authority != expected_producer {
            return Err(RailError::message(
                "staged compiler fact driver does not match cache producer authority",
            ));
        }
        Ok(Self {
            execution,
            producer_authority,
            compiler_library_directory,
            compiler_library,
            compiler_library_digest,
        })
    }

    pub(crate) fn program(&self) -> &Path {
        self.execution.program()
    }

    pub(crate) fn producer_authority(&self) -> &CompilerFactProducerAuthority {
        &self.producer_authority
    }

    pub(crate) fn compiler_library_directory(&self) -> &Path {
        &self.compiler_library_directory
    }

    pub(crate) fn cargo_command(&self, cargo: &OsStr) -> Command {
        self.execution.cargo_command(cargo)
    }

    pub(crate) fn stage_doctest_sysroot(
        &self,
        snapshot: &WorkspaceSnapshot,
        wrapper: &Path,
        wrapper_digest: &str,
        rustdoc: &Path,
        rustdoc_digest: &str,
    ) -> RailResult<CompilerFactDoctestSysroot> {
        CompilerFactDoctestSysroot::stage(
            snapshot.toolchain().direct_rustc_sysroot(),
            wrapper,
            wrapper_digest,
            rustdoc,
            rustdoc_digest,
            &self.compiler_library,
            &self.compiler_library_digest,
        )
    }
}

impl CompilerFactDoctestSysroot {
    #[cfg(unix)]
    fn stage(
        toolchain_sysroot: &Path,
        wrapper: &Path,
        _wrapper_digest: &str,
        rustdoc: &Path,
        _rustdoc_digest: &str,
        compiler_library: &AuthenticatedCompilerLibrary,
        _compiler_library_digest: &str,
    ) -> RailResult<Self> {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let toolchain_sysroot = crate::utils::canonicalize_existing(toolchain_sysroot)?;
        let wrapper = crate::utils::canonicalize_existing(wrapper)?;
        let rustdoc = crate::utils::canonicalize_existing(rustdoc)?;
        let directory = tempfile::Builder::new()
            .prefix("cargo-rail-doctest-sysroot-")
            .tempdir()?;
        let root = crate::utils::canonicalize_existing(directory.path())?;
        let bin = root.join("bin");
        let library = root.join("lib");
        fs::create_dir(&bin)?;
        fs::create_dir(&library)?;
        symlink(&wrapper, bin.join("rustc"))?;
        symlink(&rustdoc, bin.join("rustdoc"))?;
        symlink(toolchain_sysroot.join("lib/rustlib"), library.join("rustlib"))?;
        let runtime_library = library.join(
            compiler_library
                .path
                .file_name()
                .ok_or_else(|| RailError::message("compiler fact runtime library has no file name"))?,
        );
        let runtime_library_file = clone_or_copy_runtime_library(compiler_library, &runtime_library)?;
        runtime_library_file.set_permissions(fs::Permissions::from_mode(0o400))?;
        let runtime_library_generation = crate::utils::stable_open_file_generation(&runtime_library_file)
            .ok_or_else(|| RailError::message("private doctest runtime library has no stable filesystem generation"))?;
        if crate::utils::stable_file_generation(&runtime_library).as_ref() != Some(&runtime_library_generation)
            || !crate::utils::opened_file_matches_path(&runtime_library_file, &runtime_library, compiler_library.bytes)?
        {
            return Err(RailError::message(
                "private doctest runtime library changed while it was retained",
            ));
        }
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o500))?;
        fs::set_permissions(&library, fs::Permissions::from_mode(0o500))?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o500))?;
        let capability = Self {
            root,
            rustc_target: wrapper,
            rustdoc_target: rustdoc,
            library_target: toolchain_sysroot.join("lib/rustlib"),
            runtime_library,
            runtime_library_generation: Some(runtime_library_generation),
            runtime_library_bytes: compiler_library.bytes,
            _runtime_library_file: runtime_library_file,
            _directory: directory,
        };
        capability.revalidate()?;
        Ok(capability)
    }

    #[cfg(windows)]
    fn stage(
        toolchain_sysroot: &Path,
        wrapper: &Path,
        wrapper_digest: &str,
        rustdoc: &Path,
        rustdoc_digest: &str,
        compiler_library: &AuthenticatedCompilerLibrary,
        compiler_library_digest: &str,
    ) -> RailResult<Self> {
        let toolchain_sysroot = crate::utils::canonicalize_existing(toolchain_sysroot)?;
        let wrapper = crate::utils::canonicalize_existing(wrapper)?;
        let rustdoc = crate::utils::canonicalize_existing(rustdoc)?;
        let compiler_library = crate::utils::canonicalize_existing(&compiler_library.path)?;
        let directory = tempfile::Builder::new()
            .prefix("cargo-rail-doctest-sysroot-")
            .tempdir()?;
        let root = crate::utils::canonicalize_existing(directory.path())?;
        let bin = root.join("bin");
        let library = root.join("lib");
        fs::create_dir(&bin)?;
        let library_target = toolchain_sysroot.join("lib");
        let library_target_guard = crate::windows_fs::open_for_execution_guard(&library_target)?;
        let library_target_observation = crate::windows_fs::observe_file(&library_target_guard)?;
        crate::windows_fs::prove_local_ntfs(&library_target_guard, library_target_observation.volume_serial_number)?;
        let library_junction_guard =
            crate::windows_fs::create_directory_junction(&library_target, &library).map_err(|error| {
                RailError::message(format!(
                    "failed to retain the compiler sysroot library as a private directory junction: {error}"
                ))
            })?;
        let followed_library_guard = crate::windows_fs::open_for_execution_guard_following_reparse(&library)?;
        if crate::windows_fs::observe_file(&followed_library_guard)? != library_target_observation {
            return Err(RailError::message(
                "private doctest compiler library junction resolved to a different directory",
            ));
        }
        drop(followed_library_guard);
        let rustc = bin.join("rustc.exe");
        let rustc_guard = stage_windows_execution_file(
            &wrapper,
            &rustc,
            wrapper_digest,
            MAX_DOCTEST_EXECUTABLE_BYTES,
            "cargo-rail typed-doctest compiler wrapper",
        )?;
        let staged_rustdoc = bin.join("rustdoc.exe");
        let rustdoc_guard = stage_windows_execution_file(
            &rustdoc,
            &staged_rustdoc,
            rustdoc_digest,
            MAX_DOCTEST_EXECUTABLE_BYTES,
            "selected rustdoc executable",
        )?;
        let runtime_library = root.join(
            compiler_library
                .strip_prefix(&toolchain_sysroot)
                .map_err(|_| RailError::message("compiler fact runtime library is outside the selected sysroot"))?,
        );
        let runtime_parent = runtime_library
            .parent()
            .ok_or_else(|| RailError::message("compiler fact runtime library has no parent"))?;
        let runtime_library_file = match fs::symlink_metadata(&runtime_library) {
            Ok(_) => authenticate_windows_execution_file(
                &runtime_library,
                compiler_library_digest,
                MAX_COMPILER_LIBRARY_BYTES,
                "compiler fact runtime library",
            )?,
            Err(error) if error.kind() == ErrorKind::NotFound && runtime_parent == bin => stage_windows_execution_file(
                &compiler_library,
                &runtime_library,
                compiler_library_digest,
                MAX_COMPILER_LIBRARY_BYTES,
                "compiler fact runtime library",
            )?,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Err(RailError::message(
                    "compiler fact runtime library is absent from the retained sysroot library and private bin",
                ));
            }
            Err(error) => return Err(error.into()),
        };
        let bin_guard = crate::windows_fs::open_for_execution_guard(&bin)?;
        let root_guard = crate::windows_fs::open_for_execution_guard(&root)?;
        let bin_observation = crate::windows_fs::observe_file(&bin_guard)?;
        let root_observation = crate::windows_fs::observe_file(&root_guard)?;
        crate::windows_fs::prove_local_ntfs(&bin_guard, bin_observation.volume_serial_number)?;
        crate::windows_fs::prove_local_ntfs(&root_guard, root_observation.volume_serial_number)?;
        if bin_observation.volume_serial_number != root_observation.volume_serial_number {
            return Err(RailError::message(
                "private doctest sysroot and compiler bin directory are on different volumes",
            ));
        }
        let capability = Self {
            root,
            library_target,
            runtime_library,
            _runtime_library_file: runtime_library_file,
            _root_guard: root_guard,
            _bin_guard: bin_guard,
            _rustc_guard: rustc_guard,
            _rustdoc_guard: rustdoc_guard,
            _library_junction_guard: library_junction_guard,
            _library_target_guard: library_target_guard,
            _directory: directory,
        };
        capability.revalidate()?;
        Ok(capability)
    }

    #[cfg(not(any(unix, windows)))]
    fn stage(
        _toolchain_sysroot: &Path,
        _wrapper: &Path,
        _wrapper_digest: &str,
        _rustdoc: &Path,
        _rustdoc_digest: &str,
        _compiler_library: &AuthenticatedCompilerLibrary,
        _compiler_library_digest: &str,
    ) -> RailResult<Self> {
        Err(RailError::message(
            "compile-only typed doctest staging is unavailable on this host",
        ))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.root
    }

    pub(crate) fn revalidate(&self) -> RailResult<()> {
        #[cfg(unix)]
        {
            if fs::read_link(self.root.join("bin/rustc"))? != self.rustc_target
                || fs::read_link(self.root.join("bin/rustdoc"))? != self.rustdoc_target
                || fs::read_link(self.root.join("lib/rustlib"))? != self.library_target
                || self.runtime_library_generation != crate::utils::stable_file_generation(&self.runtime_library)
                || !crate::utils::opened_file_matches_path(
                    &self._runtime_library_file,
                    &self.runtime_library,
                    self.runtime_library_bytes,
                )?
            {
                return Err(RailError::message(
                    "private doctest compiler sysroot changed during acquisition",
                ));
            }
        }
        #[cfg(windows)]
        {
            let rustc = self.root.join("bin/rustc.exe");
            let rustdoc = self.root.join("bin/rustdoc.exe");
            if crate::windows_fs::observe_file(&self._rustc_guard)?
                != crate::windows_fs::observe_file(&File::open(rustc)?)?
                || crate::windows_fs::observe_file(&self._rustdoc_guard)?
                    != crate::windows_fs::observe_file(&File::open(rustdoc)?)?
                || crate::windows_fs::observe_file(&self._runtime_library_file)?
                    != crate::windows_fs::observe_file(&File::open(&self.runtime_library)?)?
                || !crate::windows_fs::directory_junction_targets(&self._library_junction_guard, &self.library_target)?
                || crate::windows_fs::observe_file(&self._library_target_guard)?
                    != crate::windows_fs::observe_file(&crate::windows_fs::open_for_execution_guard_following_reparse(
                        &self.root.join("lib"),
                    )?)?
            {
                return Err(RailError::message(
                    "private doctest compiler sysroot changed during acquisition",
                ));
            }
        }
        Ok(())
    }
}

impl Drop for CompilerFactDoctestSysroot {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            drop(fs::set_permissions(
                self.root.join("bin"),
                fs::Permissions::from_mode(0o700),
            ));
            drop(fs::set_permissions(
                self.root.join("lib"),
                fs::Permissions::from_mode(0o700),
            ));
            drop(fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700)));
        }
    }
}

#[cfg(unix)]
fn clone_or_copy_runtime_library(source: &AuthenticatedCompilerLibrary, destination: &Path) -> RailResult<File> {
    if crate::utils::stable_open_file_generation(&source.file).as_ref() != Some(&source.generation)
        || !crate::utils::opened_file_matches_path(&source.file, &source.path, source.bytes)?
    {
        return Err(RailError::message(
            "authenticated compiler fact runtime library changed before doctest staging",
        ));
    }
    let output = if let Some(clone) = crate::utils::try_clone_regular_file(&source.file, destination) {
        clone
    } else {
        let mut input = source.file.try_clone()?;
        input.seek(std::io::SeekFrom::Start(0))?;
        let mut output = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(destination)?;
        let copied = std::io::copy(&mut input.take(source.bytes.saturating_add(1)), &mut output)?;
        if copied != source.bytes {
            return Err(RailError::message(
                "authenticated compiler fact runtime library changed while it was copied",
            ));
        }
        output
    };
    if crate::utils::stable_open_file_generation(&source.file).as_ref() != Some(&source.generation)
        || !crate::utils::opened_file_matches_path(&source.file, &source.path, source.bytes)?
        || !crate::utils::opened_file_matches_path(&output, destination, source.bytes)?
    {
        return Err(RailError::message(
            "authenticated compiler fact runtime library changed during doctest staging",
        ));
    }
    Ok(output)
}

#[cfg(windows)]
fn stage_windows_execution_file(
    source: &Path,
    destination: &Path,
    expected_digest: &str,
    maximum_bytes: u64,
    description: &str,
) -> RailResult<File> {
    let mut source_file = crate::windows_fs::open_for_stable_byte_observation(source)
        .map_err(|error| RailError::message(format!("failed to retain {description} source bytes: {error}")))?;
    let source_before = crate::windows_fs::observe_file(&source_file)?;
    crate::windows_fs::prove_local_ntfs(&source_file, source_before.volume_serial_number)?;
    validate_windows_execution_file_observation(&source_before, maximum_bytes, description)?;

    match fs::hard_link(source, destination) {
        Ok(()) => {
            let mut destination_file = crate::windows_fs::open_for_execution_guard(destination)?;
            let source_after = crate::windows_fs::observe_file(&source_file)?;
            let destination_before = crate::windows_fs::observe_file(&destination_file)?;
            crate::windows_fs::prove_local_ntfs(&destination_file, destination_before.volume_serial_number)?;
            if source_after != destination_before {
                return Err(RailError::message(format!(
                    "staged {description} hard link does not retain the selected source file"
                )));
            }
            authenticate_windows_open_file(
                &mut destination_file,
                destination_before,
                expected_digest,
                maximum_bytes,
                description,
            )?;
            if crate::windows_fs::observe_file(&source_file)? != crate::windows_fs::observe_file(&destination_file)? {
                return Err(RailError::message(format!(
                    "staged {description} hard link changed while it was authenticated"
                )));
            }
            Ok(destination_file)
        }
        Err(error) if crate::windows_fs::is_cross_volume_error(&error) => {
            let mut destination_file = crate::windows_fs::create_for_execution_copy(destination)
                .map_err(|error| RailError::message(format!("failed to create private {description} copy: {error}")))?;
            transfer_windows_execution_file(
                &mut source_file,
                &mut destination_file,
                source_before,
                expected_digest,
                maximum_bytes,
                description,
            )?;
            destination_file.flush()?;
            let destination_observation = crate::windows_fs::observe_file(&destination_file)?;
            crate::windows_fs::prove_local_ntfs(&destination_file, destination_observation.volume_serial_number)?;
            if destination_observation.size != source_before.size || destination_observation.number_of_links != 1 {
                return Err(RailError::message(format!(
                    "private {description} copy does not have exact single-file ownership"
                )));
            }
            drop(destination_file);
            let path_file = crate::windows_fs::open_for_execution_guard(destination)?;
            if crate::windows_fs::observe_file(&path_file)? != destination_observation {
                return Err(RailError::message(format!(
                    "private {description} copy changed before its path was retained"
                )));
            }
            Ok(path_file)
        }
        Err(error) => Err(RailError::message(format!(
            "failed to retain {description} in the private doctest sysroot: {error}"
        ))),
    }
}

#[cfg(windows)]
fn authenticate_windows_execution_file(
    path: &Path,
    expected_digest: &str,
    maximum_bytes: u64,
    description: &str,
) -> RailResult<File> {
    let mut file = crate::windows_fs::open_for_execution_guard(path)?;
    let observation = crate::windows_fs::observe_file(&file)?;
    crate::windows_fs::prove_local_ntfs(&file, observation.volume_serial_number)?;
    authenticate_windows_open_file(&mut file, observation, expected_digest, maximum_bytes, description)?;
    Ok(file)
}

#[cfg(windows)]
fn authenticate_windows_open_file(
    file: &mut File,
    observation: crate::windows_fs::FileObservation,
    expected_digest: &str,
    maximum_bytes: u64,
    description: &str,
) -> RailResult<u64> {
    validate_windows_execution_file_observation(&observation, maximum_bytes, description)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    loop {
        let read = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| RailError::message(format!("{description} byte count overflow")))?;
        hasher.update(&buffer[..read]);
    }
    finish_windows_execution_authentication(file, observation, bytes, hasher, expected_digest, description)
}

#[cfg(windows)]
fn transfer_windows_execution_file(
    source: &mut File,
    destination: &mut File,
    observation: crate::windows_fs::FileObservation,
    expected_digest: &str,
    maximum_bytes: u64,
    description: &str,
) -> RailResult<u64> {
    validate_windows_execution_file_observation(&observation, maximum_bytes, description)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    loop {
        let read = match source.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| RailError::message(format!("{description} byte count overflow")))?;
        hasher.update(&buffer[..read]);
        destination.write_all(&buffer[..read])?;
    }
    finish_windows_execution_authentication(source, observation, bytes, hasher, expected_digest, description)
}

#[cfg(windows)]
fn finish_windows_execution_authentication(
    file: &File,
    observation: crate::windows_fs::FileObservation,
    bytes: u64,
    hasher: Sha256,
    expected_digest: &str,
    description: &str,
) -> RailResult<u64> {
    if bytes != observation.size || crate::windows_fs::observe_file(file)? != observation {
        return Err(RailError::message(format!(
            "{description} changed while its bytes were authenticated"
        )));
    }
    crate::instrumentation::record_hash(usize::try_from(bytes).unwrap_or(usize::MAX));
    crate::instrumentation::record_hashed_file_bytes_read(usize::try_from(bytes).unwrap_or(usize::MAX));
    let actual = format!("sha256:{}", ContentDigest::from_sha256_bytes(hasher.finalize().into()));
    if actual != expected_digest {
        return Err(RailError::message(format!(
            "{description} bytes do not match the captured workspace authority"
        )));
    }
    Ok(bytes)
}

#[cfg(windows)]
fn validate_windows_execution_file_observation(
    observation: &crate::windows_fs::FileObservation,
    maximum_bytes: u64,
    description: &str,
) -> RailResult<()> {
    if observation.size == 0 || observation.size > maximum_bytes {
        return Err(RailError::message(format!(
            "{description} is not a bounded nonempty file"
        )));
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct RustcVerboseIdentity<'a> {
    release: &'a str,
    commit: &'a str,
    host: &'a str,
}

impl<'a> RustcVerboseIdentity<'a> {
    fn parse(verbose: &'a str) -> RailResult<Self> {
        let release = unique_verbose_field(verbose, "release")?;
        let commit = unique_verbose_field(verbose, "commit-hash")?;
        let host = unique_verbose_field(verbose, "host")?;
        semver::Version::parse(release)
            .map_err(|error| RailError::message(format!("selected rustc release is invalid: {error}")))?;
        if !valid_hex(commit, 40) {
            return Err(RailError::message(
                "selected rustc commit is not a lowercase 40-digit hash",
            ));
        }
        if host.is_empty() || host.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(RailError::message("selected rustc host is invalid"));
        }
        Ok(Self { release, commit, host })
    }
}

fn unique_verbose_field<'a>(verbose: &'a str, name: &str) -> RailResult<&'a str> {
    let prefix = format!("{name}: ");
    let mut values = verbose.lines().filter_map(|line| line.strip_prefix(&prefix));
    let value = values
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| RailError::message(format!("selected rustc verbose identity has no {name}")))?;
    if values.next().is_some() {
        return Err(RailError::message(format!(
            "selected rustc verbose identity repeats {name}"
        )));
    }
    Ok(value)
}

fn authenticate_component_file(path: &Path, expected_digest: &str) -> RailResult<u64> {
    transfer_authenticated_component(path, expected_digest, None)
}

fn authenticate_compiler_library(path: &Path, expected_digest: &str) -> RailResult<AuthenticatedCompilerLibrary> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        RailError::message(format!(
            "failed to inspect compiler fact runtime library '{}': {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file()
        || crate::utils::is_symlink_or_reparse(&metadata)
        || metadata.len() == 0
        || metadata.len() > MAX_COMPILER_LIBRARY_BYTES
    {
        return Err(RailError::message(
            "compiler fact runtime library is not a bounded real file; install the exact rustc-dev component",
        ));
    }
    let mut file = File::open(path)?;
    if !crate::utils::opened_file_matches_path(&file, path, metadata.len())? {
        return Err(RailError::message(
            "compiler fact runtime library changed before it was opened",
        ));
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    loop {
        let read = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| RailError::message("compiler fact runtime library byte count overflow"))?;
        hasher.update(&buffer[..read]);
    }
    if bytes != metadata.len() || !crate::utils::opened_file_matches_path(&file, path, metadata.len())? {
        return Err(RailError::message(
            "compiler fact runtime library changed while its bytes were authenticated",
        ));
    }
    crate::instrumentation::record_hash(usize::try_from(bytes).unwrap_or(usize::MAX));
    crate::instrumentation::record_hashed_file_bytes_read(usize::try_from(bytes).unwrap_or(usize::MAX));
    let actual = format!("sha256:{}", ContentDigest::from_sha256_bytes(hasher.finalize().into()));
    if actual != expected_digest {
        return Err(RailError::with_help(
            "compiler fact runtime library does not match embedded release authority",
            "install the exact rustc-dev component for the selected toolchain",
        ));
    }
    #[cfg(unix)]
    let generation = {
        let generation = crate::utils::stable_open_file_generation(&file)
            .ok_or_else(|| RailError::message("compiler fact runtime library has no stable filesystem generation"))?;
        if !crate::utils::opened_file_matches_path(&file, path, metadata.len())? {
            return Err(RailError::message(
                "compiler fact runtime library changed while its generation was retained",
            ));
        }
        generation
    };
    Ok(AuthenticatedCompilerLibrary {
        path: path.to_path_buf(),
        #[cfg(unix)]
        file,
        #[cfg(unix)]
        generation,
        #[cfg(unix)]
        bytes,
    })
}

fn transfer_authenticated_component(
    path: &Path,
    expected_digest: &str,
    mut destination: Option<&mut File>,
) -> RailResult<u64> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        RailError::message(format!(
            "failed to inspect compiler fact driver '{}': {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file()
        || crate::utils::is_symlink_or_reparse(&metadata)
        || metadata.len() == 0
        || metadata.len() > MAX_FACT_DRIVER_BYTES
        || !is_executable(&metadata)
    {
        return Err(RailError::message(
            "compiler fact driver is not a bounded real executable file",
        ));
    }
    let mut file = File::open(path)?;
    if !crate::utils::private_file_matches_path(&file, path, metadata.len())? {
        return Err(RailError::message(
            "compiler fact driver changed before it was opened or has multiple links",
        ));
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    loop {
        let read = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| RailError::message("compiler fact driver byte count overflow"))?;
        hasher.update(&buffer[..read]);
        if let Some(destination) = destination.as_deref_mut() {
            destination.write_all(&buffer[..read])?;
        }
    }
    if bytes != metadata.len() || !crate::utils::private_file_matches_path(&file, path, metadata.len())? {
        return Err(RailError::message(
            "compiler fact driver changed while its bytes were authenticated",
        ));
    }
    crate::instrumentation::record_hash(usize::try_from(bytes).unwrap_or(usize::MAX));
    crate::instrumentation::record_hashed_file_bytes_read(usize::try_from(bytes).unwrap_or(usize::MAX));
    let actual = format!("sha256:{}", ContentDigest::from_sha256_bytes(hasher.finalize().into()));
    if actual != expected_digest {
        return Err(RailError::message(
            "compiler fact driver bytes do not match embedded release authority",
        ));
    }
    Ok(bytes)
}

#[cfg(target_os = "linux")]
fn stage_component(component: &CompilerFactDriverComponent) -> RailResult<CompilerFactDriverExecutionCapability> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::PermissionsExt as _;

    let mut builder = tempfile::Builder::new();
    builder.prefix("cargo-rail-fact-driver-");
    builder.permissions(fs::Permissions::from_mode(0o700));
    let mut staged = builder.tempfile()?;
    let bytes = transfer_authenticated_component(
        &component.path,
        &component.authority.content_digest,
        Some(staged.as_file_mut()),
    )?;
    staged.as_file_mut().flush()?;
    staged.as_file().set_permissions(fs::Permissions::from_mode(0o500))?;

    let path = staged.path().to_path_buf();
    let file = File::open(&path)?;
    if !crate::utils::private_file_matches_path(&file, &path, bytes)? {
        return Err(RailError::message(
            "staged compiler fact driver changed before its execution handle was opened",
        ));
    }
    staged
        .close()
        .map_err(|error| RailError::message(format!("failed to unlink staged compiler fact driver: {error}")))?;
    rustix::io::fcntl_setfd(&file, rustix::io::FdFlags::empty()).map_err(|error| {
        RailError::message(format!(
            "failed to retain compiler fact driver execution handle across Cargo: {error}"
        ))
    })?;
    let descriptor = file.as_raw_fd();
    let program = PathBuf::from(format!("/proc/self/fd/{descriptor}"));
    Ok(CompilerFactDriverExecutionCapability {
        program,
        identity: component.authority.identity.clone(),
        _file: file,
    })
}

#[cfg(target_os = "macos")]
fn stage_component(component: &CompilerFactDriverComponent) -> RailResult<CompilerFactDriverExecutionCapability> {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::Builder::new().prefix("cargo-rail-fact-driver-").tempdir()?;
    let path = directory.path().join(expected_driver_file_name());
    let mut destination = fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
    let bytes = transfer_authenticated_component(
        &component.path,
        &component.authority.content_digest,
        Some(&mut destination),
    )?;
    destination.flush()?;
    destination.set_permissions(fs::Permissions::from_mode(0o500))?;
    drop(destination);

    let file = File::open(&path)?;
    if !crate::utils::private_file_matches_path(&file, &path, bytes)? {
        return Err(RailError::message(
            "staged compiler fact driver changed before its execution handle was opened",
        ));
    }
    let program = crate::utils::canonicalize_existing(&path)?;
    let directory_path = program
        .parent()
        .ok_or_else(|| RailError::message("staged compiler fact driver has no parent directory"))?;
    let directory_text = directory_path
        .to_str()
        .filter(|path| !path.contains(['"', '\\', '\n', '\r']))
        .ok_or_else(|| {
            RailError::message("staged compiler fact driver path cannot be expressed in a sandbox profile")
        })?;
    let sandbox_profile = format!(
        "(version 1)\n(allow default)\n(deny file-write-data file-write-create file-write-unlink (literal \"{directory_text}\"))\n(deny file-write-data file-write-create file-write-unlink (subpath \"{directory_text}\"))\n"
    );
    Ok(CompilerFactDriverExecutionCapability {
        program,
        identity: component.authority.identity.clone(),
        _file: file,
        _directory: directory,
        sandbox_profile,
    })
}

#[cfg(windows)]
fn stage_component(component: &CompilerFactDriverComponent) -> RailResult<CompilerFactDriverExecutionCapability> {
    let directory = tempfile::Builder::new().prefix("cargo-rail-fact-driver-").tempdir()?;
    let path = directory.path().join(expected_driver_file_name());
    let mut destination = fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
    let bytes = transfer_authenticated_component(
        &component.path,
        &component.authority.content_digest,
        Some(&mut destination),
    )?;
    destination.flush()?;
    drop(destination);

    let file = crate::windows_fs::open_for_execution_guard(&path)?;
    let file_observation = crate::windows_fs::observe_file(&file)?;
    crate::windows_fs::prove_local_ntfs(&file, file_observation.volume_serial_number)?;
    if file_observation.size != bytes || file_observation.number_of_links != 1 {
        return Err(RailError::message(
            "staged compiler fact driver does not have exact private file ownership",
        ));
    }
    let directory_file = crate::windows_fs::open_for_execution_guard(directory.path())?;
    let directory_observation = crate::windows_fs::observe_file(&directory_file)?;
    crate::windows_fs::prove_local_ntfs(&directory_file, directory_observation.volume_serial_number)?;
    if directory_observation.volume_serial_number != file_observation.volume_serial_number {
        return Err(RailError::message(
            "staged compiler fact driver and its execution directory are on different volumes",
        ));
    }
    let program = crate::utils::canonicalize_existing(&path)?;
    Ok(CompilerFactDriverExecutionCapability {
        program,
        identity: component.authority.identity.clone(),
        _file: file,
        _directory_file: directory_file,
        _directory: directory,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn stage_component(_component: &CompilerFactDriverComponent) -> RailResult<CompilerFactDriverExecutionCapability> {
    Err(RailError::message(
        "compiler fact driver execution capability is unavailable on this host",
    ))
}

fn expected_driver_file_name() -> &'static str {
    if cfg!(windows) {
        "cargo-rail-fact-driver.exe"
    } else {
        "cargo-rail-fact-driver"
    }
}

fn validate_sha256(value: &str, description: &str) -> RailResult<()> {
    if !value
        .strip_prefix("sha256:")
        .is_some_and(|digest| valid_hex(digest, 64))
    {
        return Err(RailError::message(format!(
            "{description} is not a lowercase SHA-256 identity"
        )));
    }
    Ok(())
}

fn valid_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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

    fn digest(bytes: &[u8]) -> String {
        format!("sha256:{}", ContentDigest::sha256(bytes))
    }

    fn authority(bytes: &[u8]) -> CompilerFactDriverAuthority {
        CompilerFactDriverAuthority::from_fields([
            Some(expected_driver_file_name()),
            Some(&digest(bytes)),
            Some(&format!("sha256:{}", "a".repeat(64))),
            Some("1.95.0"),
            Some(&"b".repeat(40)),
            Some(COMPILED_TARGET),
            Some("lib/librustc_driver-test.dylib"),
            Some(&format!("sha256:{}", "c".repeat(64))),
        ])
        .expect("valid authority")
        .expect("bundled authority")
    }

    fn write_executable(path: &Path, bytes: &[u8]) {
        let mut file = File::create(path).expect("create component");
        file.write_all(bytes).expect("write component");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            file.set_permissions(fs::Permissions::from_mode(0o700))
                .expect("make component executable");
        }
    }

    #[cfg(unix)]
    #[test]
    fn doctest_runtime_clone_reuses_only_retained_authenticated_bytes() {
        let directory = tempfile::tempdir().expect("runtime directory");
        let source = directory.path().join("librustc_driver-test");
        let destination = directory.path().join("private-runtime");
        let bytes = b"authenticated compiler runtime";
        fs::write(&source, bytes).expect("runtime bytes");
        let authenticated = authenticate_compiler_library(&source, &digest(bytes)).expect("authenticated runtime");

        let cloned = clone_or_copy_runtime_library(&authenticated, &destination).expect("retained runtime clone");
        assert!(
            crate::utils::opened_file_matches_path(
                &cloned,
                &destination,
                u64::try_from(bytes.len()).expect("runtime length")
            )
            .expect("destination identity")
        );
        assert_eq!(fs::read(&destination).expect("cloned bytes"), bytes);

        fs::write(&source, b"changed compiler runtime").expect("change runtime bytes");
        let rejected = directory.path().join("rejected-runtime");
        clone_or_copy_runtime_library(&authenticated, &rejected).unwrap_err();
        assert!(!rejected.exists());
    }

    #[test]
    fn absent_build_authority_is_an_explicit_source_installation_state() {
        assert_eq!(
            CompilerFactDriverAuthority::from_fields([None; 8]).expect("valid absence"),
            None
        );
    }

    #[test]
    fn source_bundle_requires_a_sorted_closed_offline_build_inventory() {
        let files = [
            ".cargo/config.toml",
            "src/compiler/fact_protocol.rs",
            "tools/compiler-fact-driver/Cargo.lock",
            "tools/compiler-fact-driver/Cargo.toml",
            "tools/compiler-fact-driver/build.rs",
            "tools/compiler-fact-driver/src/collection.rs",
            "tools/compiler-fact-driver/src/main.rs",
            "vendor/serde-1.0.0/Cargo.toml",
        ]
        .into_iter()
        .map(|path| CompilerFactDriverSourceFile {
            path: path.to_string(),
            hex: "00".to_string(),
        })
        .collect();
        let bundle = CompilerFactDriverSourceBundle { version: 1, files };
        validate_source_bundle(&bundle).expect("closed source inventory");

        let mut traversal = bundle.files;
        traversal[0].path = "../outside".to_string();
        validate_source_bundle(&CompilerFactDriverSourceBundle {
            version: 1,
            files: traversal,
        })
        .unwrap_err();
    }

    #[test]
    fn source_component_bytes_are_bound_before_parsing() {
        let directory = tempfile::tempdir().expect("source component directory");
        let path = directory.path().join("source.json");
        fs::write(&path, b"authenticated source").expect("source bytes");
        assert_eq!(
            read_authenticated_component(&path, &digest(b"authenticated source"), 1024).expect("authenticated source"),
            b"authenticated source"
        );
        read_authenticated_component(&path, &digest(b"different source"), 1024).unwrap_err();
    }

    #[test]
    fn runtime_library_does_not_claim_compiler_development_support() {
        let sysroot = tempfile::tempdir().expect("temporary sysroot");
        let host = "test-host";
        fs::create_dir_all(sysroot.path().join("lib")).expect("runtime library directory");
        fs::write(sysroot.path().join("lib/librustc_driver-residual.so"), b"runtime")
            .expect("residual runtime library");

        assert!(!compiler_development_support_present(sysroot.path(), host).expect("development support inspection"));

        let development = sysroot.path().join("lib/rustlib").join(host).join("lib");
        fs::create_dir_all(&development).expect("development library directory");
        fs::write(development.join("librustc_hir-test.rmeta"), b"metadata").expect("compiler metadata");
        assert!(compiler_development_support_present(sysroot.path(), host).expect("development support inspection"));
    }

    #[test]
    fn build_authority_is_all_or_nothing_and_target_bound() {
        let incomplete = [
            Some(expected_driver_file_name()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ];
        CompilerFactDriverAuthority::from_fields(incomplete).unwrap_err();

        let digest = format!("sha256:{}", "a".repeat(64));
        let commit = "b".repeat(40);
        let wrong_target = format!("{COMPILED_TARGET}-wrong");
        CompilerFactDriverAuthority::from_fields([
            Some(expected_driver_file_name()),
            Some(&digest),
            Some(&digest),
            Some("1.95.0"),
            Some(&commit),
            Some(&wrong_target),
            Some("lib/librustc_driver-test.dylib"),
            Some(&digest),
        ])
        .unwrap_err();
    }

    #[test]
    fn verbose_compiler_identity_requires_exact_unique_fields() {
        let commit = "b".repeat(40);
        let verbose = format!("rustc 1.95.0\nrelease: 1.95.0\ncommit-hash: {commit}\nhost: {COMPILED_TARGET}");
        assert_eq!(
            RustcVerboseIdentity::parse(&verbose).expect("valid compiler"),
            RustcVerboseIdentity {
                release: "1.95.0",
                commit: &commit,
                host: COMPILED_TARGET,
            }
        );
        RustcVerboseIdentity::parse("rustc 1.95.0").unwrap_err();
        RustcVerboseIdentity::parse(&format!("{verbose}\nhost: duplicate")).unwrap_err();
    }

    #[test]
    fn sibling_component_is_bound_to_exact_bytes_and_provenance() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let executable = directory
            .path()
            .join(if cfg!(windows) { "cargo-rail.exe" } else { "cargo-rail" });
        write_executable(&executable, b"frontend");
        let component_path = directory.path().join(expected_driver_file_name());
        write_executable(&component_path, b"matched driver");
        let authority = authority(b"matched driver");

        let component = CompilerFactDriverComponent::discover_with_authority(
            &authority,
            &executable,
            directory.path().to_path_buf(),
        )
        .expect("authenticated component");
        assert_eq!(
            component.path,
            crate::utils::canonicalize_existing(&component_path).expect("canonical component")
        );
        assert_eq!(component.authority.provenance, format!("sha256:{}", "a".repeat(64)));
        assert!(component.identity().starts_with(DRIVER_IDENTITY_PREFIX));

        write_executable(&component_path, b"tampered driver");
        assert!(
            CompilerFactDriverComponent::discover_with_authority(
                &authority,
                &executable,
                directory.path().to_path_buf()
            )
            .is_err()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn staged_component_executes_from_an_unlinked_read_only_handle() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let executable = directory.path().join("cargo-rail");
        write_executable(&executable, b"frontend");
        let component_path = directory.path().join(expected_driver_file_name());
        fs::copy("/bin/echo", &component_path).expect("copy native executable");
        let component_bytes = fs::read(&component_path).expect("read native executable");
        let authority = authority(&component_bytes);
        let component = CompilerFactDriverComponent::discover_with_authority(
            &authority,
            &executable,
            directory.path().to_path_buf(),
        )
        .expect("authenticated component");

        let capability = component.stage().expect("execution capability");
        assert_eq!(capability.identity(), component.identity());
        let output = std::process::Command::new(capability.program())
            .arg("handle-bound")
            .output()
            .expect("execute handle-bound component");
        assert!(
            output.status.success(),
            "handle-bound component failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"handle-bound\n");

        let mut parent = capability.cargo_command(OsStr::new("/bin/sh"));
        let output = parent
            .args([
                OsStr::new("-c"),
                OsStr::new("\"$1\" inherited"),
                OsStr::new("cargo-parent"),
            ])
            .arg(capability.program())
            .output()
            .expect("execute handle-bound component through a Cargo-like parent");
        assert!(
            output.status.success(),
            "inherited execution handle failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"inherited\n");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn staged_component_executes_while_cargo_descendants_cannot_mutate_its_directory() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let executable = directory.path().join("cargo-rail");
        write_executable(&executable, b"frontend");
        let component_path = directory.path().join(expected_driver_file_name());
        fs::copy(
            std::env::current_exe().expect("current test executable"),
            &component_path,
        )
        .expect("copy native executable");
        let component_bytes = fs::read(&component_path).expect("read native executable");
        let authority = authority(&component_bytes);
        let component = CompilerFactDriverComponent::discover_with_authority(
            &authority,
            &executable,
            directory.path().to_path_buf(),
        )
        .expect("authenticated component");

        let capability = component.stage().expect("execution capability");
        let mut allowed = capability.cargo_command(OsStr::new("/bin/sh"));
        let output = allowed
            .args([
                OsStr::new("-c"),
                OsStr::new("\"$1\" --list"),
                OsStr::new("cargo-parent"),
            ])
            .arg(capability.program())
            .output()
            .expect("execute staged component through a Cargo-like parent");
        assert!(
            output.status.success(),
            "sandboxed component failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains(
            "compiler::driver::tests::staged_component_executes_while_cargo_descendants_cannot_mutate_its_directory"
        ));

        let attack = capability
            .program()
            .parent()
            .expect("capability directory")
            .join("replacement");
        let mut denied = capability.cargo_command(OsStr::new("/usr/bin/touch"));
        let output = denied.arg(&attack).output().expect("attempt sandboxed mutation");
        assert!(!output.status.success());
        assert!(!attack.exists());
    }

    #[cfg(windows)]
    #[test]
    fn staged_component_executes_while_its_windows_path_remains_locked() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let executable = directory.path().join("cargo-rail.exe");
        write_executable(&executable, b"frontend");
        let component_path = directory.path().join(expected_driver_file_name());
        fs::copy(
            std::env::current_exe().expect("current test executable"),
            &component_path,
        )
        .expect("copy native executable");
        let component_bytes = fs::read(&component_path).expect("read native executable");
        let authority = authority(&component_bytes);
        let component = CompilerFactDriverComponent::discover_with_authority(
            &authority,
            &executable,
            directory.path().to_path_buf(),
        )
        .expect("authenticated component");

        let capability = component.stage().expect("execution capability");
        let output = Command::new(capability.program())
            .arg("--list")
            .output()
            .expect("execute staged component");
        assert!(output.status.success());

        let replacement = capability.program().with_extension("replacement.exe");
        let error = fs::rename(capability.program(), &replacement)
            .expect_err("the retained executable handle must exclude replacement");
        assert_eq!(error.raw_os_error(), Some(32));
        let directory_path = capability.program().parent().expect("execution directory");
        let error = fs::rename(directory_path, directory_path.with_extension("replacement"))
            .expect_err("the retained directory handle must exclude replacement");
        assert_eq!(error.raw_os_error(), Some(32));
    }

    #[cfg(windows)]
    #[test]
    fn private_windows_doctest_sysroot_is_exact_guarded_and_removed_on_drop() {
        let fixture = tempfile::tempdir().expect("temporary fixture");
        let sysroot = fixture.path().join("sysroot");
        let sysroot_bin = sysroot.join("bin");
        fs::create_dir_all(sysroot.join("lib/rustlib")).expect("sysroot library");
        fs::create_dir(&sysroot_bin).expect("sysroot bin");
        let wrapper = fixture.path().join("cargo-rail.exe");
        let rustdoc = fixture.path().join("rustdoc.exe");
        let compiler_library = sysroot_bin.join("rustc_driver-test.dll");
        write_executable(&wrapper, b"wrapper");
        write_executable(&rustdoc, b"rustdoc");
        fs::write(&compiler_library, b"compiler library").expect("compiler library");
        let compiler_library_digest = digest(b"compiler library");
        let compiler_library = authenticate_compiler_library(&compiler_library, &compiler_library_digest)
            .expect("authenticated compiler library");

        let capability = CompilerFactDoctestSysroot::stage(
            &sysroot,
            &wrapper,
            &digest(b"wrapper"),
            &rustdoc,
            &digest(b"rustdoc"),
            &compiler_library,
            &compiler_library_digest,
        )
        .expect("private doctest sysroot");
        let private_root = capability.path().to_path_buf();
        assert_eq!(
            fs::read(private_root.join("bin/rustc.exe")).expect("wrapper"),
            b"wrapper"
        );
        assert_eq!(
            fs::read(private_root.join("bin/rustdoc.exe")).expect("rustdoc"),
            b"rustdoc"
        );
        assert!(private_root.join("lib/rustlib").is_dir());
        capability.revalidate().expect("stable private doctest sysroot");

        drop(capability);
        assert!(
            !private_root.exists(),
            "guard handles must close before the private sysroot is removed"
        );
    }

    #[cfg(windows)]
    #[test]
    fn private_windows_doctest_sysroot_rejects_digest_drift() {
        let fixture = tempfile::tempdir().expect("temporary fixture");
        let sysroot = fixture.path().join("sysroot");
        let sysroot_bin = sysroot.join("bin");
        fs::create_dir_all(sysroot.join("lib/rustlib")).expect("sysroot library");
        fs::create_dir(&sysroot_bin).expect("sysroot bin");
        let wrapper = fixture.path().join("cargo-rail.exe");
        let rustdoc = fixture.path().join("rustdoc.exe");
        let compiler_library = sysroot_bin.join("rustc_driver-test.dll");
        write_executable(&wrapper, b"wrapper");
        write_executable(&rustdoc, b"rustdoc");
        fs::write(&compiler_library, b"compiler library").expect("compiler library");
        let compiler_library_digest = digest(b"compiler library");
        let compiler_library = authenticate_compiler_library(&compiler_library, &compiler_library_digest)
            .expect("authenticated compiler library");

        let error = CompilerFactDoctestSysroot::stage(
            &sysroot,
            &wrapper,
            &digest(b"different wrapper"),
            &rustdoc,
            &digest(b"rustdoc"),
            &compiler_library,
            &compiler_library_digest,
        )
        .err()
        .expect("captured wrapper digest drift must fail closed");
        assert!(
            error.to_string().contains("captured workspace authority"),
            "unexpected error: {error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn sibling_component_rejects_symlinks_hard_links_and_non_executables() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let directory = tempfile::tempdir().expect("temporary directory");
        let executable = directory.path().join("cargo-rail");
        write_executable(&executable, b"frontend");
        let component_path = directory.path().join(expected_driver_file_name());
        let authority = authority(b"matched driver");

        let target = directory.path().join("target-driver");
        write_executable(&target, b"matched driver");
        symlink(&target, &component_path).expect("create symlink");
        assert!(
            CompilerFactDriverComponent::discover_with_authority(
                &authority,
                &executable,
                directory.path().to_path_buf()
            )
            .is_err()
        );
        fs::remove_file(&component_path).expect("remove symlink");

        fs::hard_link(&target, &component_path).expect("create hard link");
        assert!(
            CompilerFactDriverComponent::discover_with_authority(
                &authority,
                &executable,
                directory.path().to_path_buf()
            )
            .is_err()
        );
        fs::remove_file(&component_path).expect("remove hard link");

        fs::copy(&target, &component_path).expect("copy component");
        fs::set_permissions(&component_path, fs::Permissions::from_mode(0o600)).expect("remove execute mode");
        assert!(
            CompilerFactDriverComponent::discover_with_authority(
                &authority,
                &executable,
                directory.path().to_path_buf()
            )
            .is_err()
        );
    }
}
