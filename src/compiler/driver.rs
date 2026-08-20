//! Authenticated distribution boundary for toolchain-matched fact drivers.
//!
//! Repository configuration cannot select or download a driver. A release
//! build either embeds one complete component authority or embeds none. Source
//! installations therefore perform no driver filesystem work unless their
//! builder deliberately supplied an exact authenticated component.

use std::ffi::OsStr;
use std::fs::{self, File};
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
const MAX_COMPILER_LIBRARY_BYTES: u64 = 1024 * 1024 * 1024;
const COMPILED_TARGET: &str = env!("CARGO_RAIL_COMPILED_TARGET");

const FACT_DRIVER_FILE: Option<&str> = option_env!("CARGO_RAIL_FACT_DRIVER_FILE");
const FACT_DRIVER_SHA256: Option<&str> = option_env!("CARGO_RAIL_FACT_DRIVER_SHA256");
const FACT_DRIVER_PROVENANCE: Option<&str> = option_env!("CARGO_RAIL_FACT_DRIVER_PROVENANCE");
const FACT_DRIVER_RUSTC_RELEASE: Option<&str> = option_env!("CARGO_RAIL_FACT_DRIVER_RUSTC_RELEASE");
const FACT_DRIVER_RUSTC_COMMIT: Option<&str> = option_env!("CARGO_RAIL_FACT_DRIVER_RUSTC_COMMIT");
const FACT_DRIVER_RUSTC_HOST: Option<&str> = option_env!("CARGO_RAIL_FACT_DRIVER_RUSTC_HOST");
const FACT_DRIVER_COMPILER_LIBRARY: Option<&str> = option_env!("CARGO_RAIL_FACT_DRIVER_COMPILER_LIBRARY");
const FACT_DRIVER_COMPILER_LIBRARY_SHA256: Option<&str> = option_env!("CARGO_RAIL_FACT_DRIVER_COMPILER_LIBRARY_SHA256");

/// Build-time release authority for exactly one sibling component.
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Exact sibling component bytes accepted by embedded release authority.
pub(crate) struct CompilerFactDriverComponent {
    authority: CompilerFactDriverAuthority,
    path: PathBuf,
    compiler_library_directory: PathBuf,
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
    compiler_library_path: PathBuf,
    compiler_library_digest: String,
}

/// Private stable-rustdoc sysroot view whose test builder is cargo-rail.
pub(crate) struct CompilerFactDoctestSysroot {
    root: PathBuf,
    #[cfg(unix)]
    rustc_target: PathBuf,
    #[cfg(unix)]
    rustdoc_target: PathBuf,
    #[cfg(unix)]
    library_target: PathBuf,
    runtime_library: PathBuf,
    #[cfg(unix)]
    runtime_library_generation: Option<Vec<u8>>,
    _runtime_library_file: File,
    _directory: tempfile::TempDir,
    #[cfg(windows)]
    _root_guard: File,
    #[cfg(windows)]
    _bin_guard: File,
    #[cfg(windows)]
    _rustc_guard: File,
    #[cfg(windows)]
    _rustdoc_guard: File,
}

impl CompilerFactDriverAuthority {
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
        let library = toolchain.rustc_sysroot().join(&self.compiler_library);
        authenticate_compiler_library(&library, &self.compiler_library_digest)?;
        library
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| RailError::message("compiler fact runtime library has no parent directory"))
    }

    fn validate_toolchain_identity(&self, toolchain: &ToolchainIdentity) -> RailResult<()> {
        let selected = RustcVerboseIdentity::parse(toolchain.rustc_verbose_version())?;
        if selected.release != self.rustc_release
            || selected.commit != self.rustc_commit
            || selected.host != self.rustc_host
            || toolchain.host_target() != self.rustc_host
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
        toolchain: &ToolchainIdentity,
        compiler_identity_seed: &str,
    ) -> RailResult<CompilerFactProducerAuthority> {
        let authority = Self::embedded()?.ok_or_else(|| {
      RailError::with_help(
        "this cargo-rail installation does not include compiler fact producer authority",
        "install a native cargo-rail release archive for the selected host, or build the isolated companion explicitly",
      )
    })?;
        authority.validate_toolchain_identity(toolchain)?;
        validate_exclusive_wrapper_authority(
            toolchain.rustc_wrapper_program(),
            toolchain.rustc_workspace_wrapper_program(),
        )?;
        Ok(CompilerFactProducerAuthority {
            compiler_identity: format!(
                "{COMPILER_IDENTITY_PREFIX}{}",
                ContentDigest::sha256(compiler_identity_seed.as_bytes())
            ),
            driver_identity: authority.identity,
        })
    }
}

impl CompilerFactDriverComponent {
    /// Authenticate the release sibling selected at build time.
    pub(crate) fn discover(toolchain: &ToolchainIdentity, cargo_rail_executable: &Path) -> RailResult<Option<Self>> {
        let Some(authority) = CompilerFactDriverAuthority::embedded()? else {
            return Ok(None);
        };
        validate_exclusive_wrapper_authority(
            toolchain.rustc_wrapper_program(),
            toolchain.rustc_workspace_wrapper_program(),
        )?;
        let compiler_library_directory = authority.validate_toolchain(toolchain)?;
        Self::discover_with_authority(&authority, cargo_rail_executable, compiler_library_directory).map(Some)
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
        Ok(Self {
            authority: authority.clone(),
            path,
            compiler_library_directory,
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

fn validate_exclusive_wrapper_authority(global: Option<&OsStr>, workspace: Option<&OsStr>) -> RailResult<()> {
    if global.is_some() || workspace.is_some() {
        return Err(RailError::with_help(
            "typed compiler facts require an exclusive authenticated rustc driver, but Cargo selected another compiler wrapper",
            "remove build.rustc-wrapper and build.rustc-workspace-wrapper (or their environment overrides) for this analysis run",
        ));
    }
    Ok(())
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
        let component = CompilerFactDriverComponent::discover(snapshot.toolchain(), &cargo_rail_executable)?.ok_or_else(|| {
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
        let compiler_library_path = compiler_library_directory.join(
            Path::new(&component.authority.compiler_library)
                .file_name()
                .ok_or_else(|| RailError::message("compiler fact runtime library has no file name"))?,
        );
        let compiler_library_digest = component.authority.compiler_library_digest.clone();
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
            compiler_library_path,
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
        rustdoc: &Path,
    ) -> RailResult<CompilerFactDoctestSysroot> {
        CompilerFactDoctestSysroot::stage(
            snapshot.toolchain().rustc_sysroot(),
            wrapper,
            rustdoc,
            &self.compiler_library_path,
            &self.compiler_library_digest,
        )
    }
}

impl CompilerFactDoctestSysroot {
    #[cfg(unix)]
    fn stage(
        toolchain_sysroot: &Path,
        wrapper: &Path,
        rustdoc: &Path,
        compiler_library: &Path,
        compiler_library_digest: &str,
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
                .file_name()
                .ok_or_else(|| RailError::message("compiler fact runtime library has no file name"))?,
        );
        clone_or_copy_runtime_library(compiler_library, &runtime_library)?;
        authenticate_compiler_library(&runtime_library, compiler_library_digest)?;
        fs::set_permissions(&runtime_library, fs::Permissions::from_mode(0o400))?;
        let runtime_library_file = File::open(&runtime_library)?;
        let runtime_library_generation =
            Some(crate::utils::stable_file_generation(&runtime_library).ok_or_else(|| {
                RailError::message("private doctest runtime library has no stable filesystem generation")
            })?);
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o500))?;
        fs::set_permissions(&library, fs::Permissions::from_mode(0o500))?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o500))?;
        let capability = Self {
            root,
            rustc_target: wrapper,
            rustdoc_target: rustdoc,
            library_target: toolchain_sysroot.join("lib/rustlib"),
            runtime_library,
            runtime_library_generation,
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
        rustdoc: &Path,
        compiler_library: &Path,
        compiler_library_digest: &str,
    ) -> RailResult<Self> {
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
        mirror_directory_with_hard_links(&toolchain_sysroot.join("lib"), &library)?;
        let rustc = bin.join("rustc.exe");
        fs::hard_link(&wrapper, &rustc).map_err(|error| {
            RailError::message(format!(
                "failed to retain cargo-rail as the private doctest compiler on the selected volume: {error}"
            ))
        })?;
        let staged_rustdoc = bin.join("rustdoc.exe");
        fs::hard_link(&rustdoc, &staged_rustdoc).map_err(|error| {
            RailError::message(format!(
                "failed to retain rustdoc in the private doctest sysroot on the selected volume: {error}"
            ))
        })?;
        let rustc_guard = crate::windows_fs::open_for_execution_guard(&rustc)?;
        let rustdoc_guard = crate::windows_fs::open_for_execution_guard(&staged_rustdoc)?;
        let runtime_library = root.join(
            compiler_library
                .strip_prefix(&toolchain_sysroot)
                .map_err(|_| RailError::message("compiler fact runtime library is outside the selected sysroot"))?,
        );
        if !runtime_library.exists() {
            fs::create_dir_all(
                runtime_library
                    .parent()
                    .ok_or_else(|| RailError::message("compiler fact runtime library has no parent"))?,
            )?;
            fs::copy(compiler_library, &runtime_library)?;
        }
        authenticate_compiler_library(&runtime_library, compiler_library_digest)?;
        let runtime_library_file = crate::windows_fs::open_for_execution_guard(&runtime_library)?;
        let bin_guard = crate::windows_fs::open_for_execution_guard(&bin)?;
        let root_guard = crate::windows_fs::open_for_execution_guard(&root)?;
        let capability = Self {
            root,
            runtime_library,
            _runtime_library_file: runtime_library_file,
            _directory: directory,
            _root_guard: root_guard,
            _bin_guard: bin_guard,
            _rustc_guard: rustc_guard,
            _rustdoc_guard: rustdoc_guard,
        };
        capability.revalidate()?;
        Ok(capability)
    }

    #[cfg(not(any(unix, windows)))]
    fn stage(
        _toolchain_sysroot: &Path,
        _wrapper: &Path,
        _rustdoc: &Path,
        _compiler_library: &Path,
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
                || !self.root.join("lib").is_dir()
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
fn clone_or_copy_runtime_library(source: &Path, destination: &Path) -> RailResult<()> {
    let source_file = File::open(source)?;
    if let Some(clone) = crate::utils::try_clone_regular_file(&source_file, destination) {
        drop(clone);
        return Ok(());
    }
    fs::copy(source, destination)?;
    Ok(())
}

#[cfg(windows)]
fn mirror_directory_with_hard_links(source: &Path, destination: &Path) -> RailResult<()> {
    const MAX_ENTRIES: usize = 16_384;

    fs::create_dir(destination)?;
    let mut pending = vec![(source.to_path_buf(), destination.to_path_buf())];
    let mut entries = 0usize;
    while let Some((source, destination)) = pending.pop() {
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            entries = entries
                .checked_add(1)
                .ok_or_else(|| RailError::message("doctest sysroot mirror entry count overflow"))?;
            if entries > MAX_ENTRIES {
                return Err(RailError::message("doctest sysroot mirror exceeds its entry bound"));
            }
            let metadata = entry.metadata()?;
            let target = destination.join(entry.file_name());
            if metadata.is_dir() && !crate::utils::is_symlink_or_reparse(&metadata) {
                fs::create_dir(&target)?;
                pending.push((entry.path(), target));
            } else if metadata.is_file() && !crate::utils::is_symlink_or_reparse(&metadata) {
                fs::hard_link(entry.path(), target)?;
            } else {
                return Err(RailError::message(
                    "doctest sysroot mirror contains an unsupported file kind",
                ));
            }
        }
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

fn authenticate_compiler_library(path: &Path, expected_digest: &str) -> RailResult<u64> {
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
    Ok(bytes)
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

    #[test]
    fn absent_build_authority_is_an_explicit_source_installation_state() {
        assert_eq!(
            CompilerFactDriverAuthority::from_fields([None; 8]).expect("valid absence"),
            None
        );
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
    fn typed_driver_rejects_every_untrusted_outer_or_inner_wrapper() {
        validate_exclusive_wrapper_authority(None, None).unwrap();
        assert!(validate_exclusive_wrapper_authority(Some(OsStr::new("sccache")), None).is_err());
        assert!(validate_exclusive_wrapper_authority(None, Some(OsStr::new("workspace-wrapper"))).is_err());
        assert!(
            validate_exclusive_wrapper_authority(
                Some(OsStr::new("outer-wrapper")),
                Some(OsStr::new("workspace-wrapper")),
            )
            .is_err()
        );
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
