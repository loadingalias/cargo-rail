//! Clippy inputs that the equivalent rustc compilation does not read.
//!
//! `clippy-driver` runs the rustc compilation Cargo requested with lints added.
//! Its result also depends on inputs that rustc never reads:
//!
//! - the `clippy-driver` executable, which links the compiler library the session already binds;
//! - the arguments it appends from `CLIPPY_ARGS`, and whether it runs lints at all;
//! - environment it reads without recording it in dep-info;
//! - the configuration file lookup, which checks every ancestor directory until one holds a file;
//! - the inputs of `cargo metadata`, which the `clippy::cargo` lints run during linting.
//!   A source attribute can enable those lints, so every Clippy action binds them.
//!
//! The capture below is part of the base action. A change to any bound input selects another action.
//! An input that cannot be bound exactly keeps the invocation on the ordinary compiler path.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use crate::error::{RailError, RailResult};
use crate::source::ContentDigest;

use super::{NativeCaptureBudget, NativeInputFailure, NativeMetadataGuard, capture_guarded_file};

const CLIPPY_CAPTURE_VERSION: u32 = 1;
/// `cargo clippy` joins the arguments after `--` with this separator in `CLIPPY_ARGS`.
const CLIPPY_ARGUMENT_SEPARATOR: &str = "__CLIPPY_HACKERY__";
/// Clippy checks both names in each directory, in this order.
const CONFIGURATION_NAMES: [&str; 2] = [".clippy.toml", "clippy.toml"];
/// Environment the driver or its configuration lookup reads. Dep-info records only the first two.
const CLIPPY_ENVIRONMENT: [&str; 5] = [
    "CLIPPY_ARGS",
    "CLIPPY_CONF_DIR",
    "CLIPPY_DISABLE_DOCS_LINKS",
    "CARGO_PKG_RUST_VERSION",
    "CARGO_MANIFEST_DIR",
];
/// Compiler selections that `cargo metadata` reads when it queries rustc.
const CARGO_TOOLCHAIN_ENVIRONMENT: [&str; 2] = ["RUSTC", "RUSTUP_TOOLCHAIN"];
const DEPENDENCY_TABLES: [&str; 5] = [
    "dependencies",
    "dev-dependencies",
    "dev_dependencies",
    "build-dependencies",
    "build_dependencies",
];
const MAX_CARGO_MANIFESTS: usize = 4096;
const MAX_CARGO_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;

pub(super) const SYSROOT_OVERRIDE_REASON: &str = "clippy_sysroot_override_unavailable";
const DRIVER_REASON: &str = "clippy_driver_identity_unavailable";
const CONFIGURATION_REASON: &str = "clippy_configuration_unavailable";
const EXTERNAL_CONFIGURATION_REASON: &str = "clippy_configuration_outside_repository";
const CARGO_REASON: &str = "clippy_cargo_inputs_unavailable";

/// Exact Clippy inputs beyond the rustc-equivalent action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct ClippyActionCapture {
    version: u32,
    driver_digest: String,
    lints: bool,
    appended_arguments: Vec<String>,
    environment: Vec<EnvironmentValue>,
    configuration: Vec<PathState>,
    cargo_executable: PathState,
    cargo_environment: Vec<EnvironmentValue>,
    cargo_inputs: Vec<PathState>,
    /// Generations of every bound file, so revalidation also rejects a change that restores the old bytes.
    #[serde(skip)]
    guards: BTreeMap<PathBuf, NativeMetadataGuard>,
    #[serde(skip)]
    authority: ClippyCaptureAuthority,
}

/// Recapture inputs retained outside the serialized identity.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ClippyCaptureAuthority {
    driver: PathBuf,
    sysroot: PathBuf,
    workspace_root: PathBuf,
    compiler_arguments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct EnvironmentValue {
    name: String,
    value_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct PathState {
    path: String,
    state: PathKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PathKind {
    Absent,
    Directory,
    File {
        target: String,
        content_digest: String,
        bytes: u64,
    },
}

impl ClippyActionCapture {
    /// Capture Clippy's inputs for one invocation run from the current directory and environment.
    pub(super) fn capture(
        driver: &Path,
        sysroot: &Path,
        workspace_root: &Path,
        compiler_arguments: &[String],
        budget: &mut NativeCaptureBudget,
    ) -> Result<Self, NativeInputFailure> {
        let started = Instant::now();
        if std::env::var_os("SYSROOT").is_some() {
            return Err(failure(
                SYSROOT_OVERRIDE_REASON,
                "Clippy appends a SYSROOT override that the matched compiler session does not bind",
            ));
        }
        let current_directory = std::env::current_dir().map_err(|error| failure_from(CONFIGURATION_REASON, error))?;
        let mut guards = BTreeMap::new();
        let driver_digest = capture_driver(driver, sysroot, &current_directory, started, budget, &mut guards)?;
        let (lints, appended_arguments) = clippy_arguments(
            compiler_arguments,
            std::env::var("CLIPPY_ARGS").ok().as_deref(),
            std::env::var_os("CARGO_PRIMARY_PACKAGE").is_some(),
        );
        let environment = CLIPPY_ENVIRONMENT
            .iter()
            .map(|name| environment_value(name))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let workspace = crate::utils::canonicalize_existing(workspace_root)
            .map_err(|error| failure_from(CONFIGURATION_REASON, error))?;
        let configuration_start = current_directory.join(
            std::env::var_os("CLIPPY_CONF_DIR")
                .or_else(|| std::env::var_os("CARGO_MANIFEST_DIR"))
                .map_or_else(|| PathBuf::from("."), PathBuf::from),
        );
        let configuration = configuration_trail(&configuration_start, &workspace, started, budget, &mut guards)?;
        let (cargo_executable, cargo_inputs) = (|| {
            // Cargo gives every compiler process its own executable; without it Clippy searches PATH.
            let cargo = std::env::var_os("CARGO")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .ok_or_else(|| RailError::message("Clippy's cargo executable is not an absolute CARGO path"))?;
            let cargo_home = crate::cargo::CargoConfigSnapshot::cargo_home(&current_directory)?;
            cargo_metadata_inputs(&current_directory, &cargo, &cargo_home, started, budget, &mut guards)
        })()
        .map_err(|error| failure_from(CARGO_REASON, error))?;
        let cargo_environment = cargo_environment(std::env::vars_os().filter_map(|(name, _)| name.into_string().ok()));
        Ok(Self {
            version: CLIPPY_CAPTURE_VERSION,
            driver_digest,
            lints,
            appended_arguments,
            environment,
            configuration,
            cargo_executable,
            cargo_environment,
            cargo_inputs,
            guards,
            authority: ClippyCaptureAuthority {
                driver: driver.to_path_buf(),
                sysroot: sysroot.to_path_buf(),
                workspace_root: workspace_root.to_path_buf(),
                compiler_arguments: compiler_arguments.to_vec(),
            },
        })
    }

    /// Arguments `clippy-driver` passes to its compiler after it removes the rustc program.
    pub(super) fn effective_arguments(&self, compiler_arguments: &[String]) -> Vec<String> {
        compiler_arguments
            .iter()
            .chain(&self.appended_arguments)
            .cloned()
            .collect()
    }

    /// Recapture every bound input and require identical bytes and file generations.
    pub(super) fn revalidate(&self, budget: &mut NativeCaptureBudget) -> RailResult<()> {
        let current = Self::capture(
            &self.authority.driver,
            &self.authority.sysroot,
            &self.authority.workspace_root,
            &self.authority.compiler_arguments,
            budget,
        )
        .map_err(RailError::from)?;
        if current != *self {
            return Err(RailError::message("Clippy inputs changed after action capture"));
        }
        Ok(())
    }
}

/// Replicate how `clippy-driver` selects lints and appends `CLIPPY_ARGS` and `--cfg clippy`.
///
/// Clippy runs lints unless `--cap-lints allow` is present without a forced Clippy lint,
/// or `--no-deps` limits linting to Cargo's primary packages.
/// Without lints it compiles the unchanged arguments.
fn clippy_arguments(arguments: &[String], clippy_args: Option<&str>, primary_package: bool) -> (bool, Vec<String>) {
    let mut no_deps = false;
    let mut appended = clippy_args
        .unwrap_or_default()
        .split(CLIPPY_ARGUMENT_SEPARATOR)
        .filter_map(|argument| match argument {
            "" => None,
            "--no-deps" => {
                no_deps = true;
                None
            }
            argument => Some(argument.to_string()),
        })
        .collect::<Vec<_>>();
    appended.extend(["--cfg".to_string(), "clippy".to_string()]);
    let cap_lints_allow = argument_value(arguments, "--cap-lints", |value| value == "allow")
        && !argument_value(arguments, "--force-warn", |value| value.contains("clippy::"));
    let lints = !cap_lints_allow && (!no_deps || primary_package);
    if lints { (true, appended) } else { (false, Vec::new()) }
}

/// Clippy's option matcher: `--name=value` or `--name value`.
fn argument_value(arguments: &[String], name: &str, predicate: impl Fn(&str) -> bool) -> bool {
    let mut arguments = arguments.iter().map(String::as_str);
    while let Some(argument) = arguments.next() {
        let mut parts = argument.splitn(2, '=');
        if parts.next() != Some(name) {
            continue;
        }
        if parts.next().or_else(|| arguments.next()).is_some_and(&predicate) {
            return true;
        }
    }
    false
}

fn capture_driver(
    driver: &Path,
    sysroot: &Path,
    current_directory: &Path,
    started: Instant,
    budget: &mut NativeCaptureBudget,
    guards: &mut BTreeMap<PathBuf, NativeMetadataGuard>,
) -> Result<String, NativeInputFailure> {
    let selected = crate::executable::resolve_executable_path(driver.as_os_str(), current_directory)
        .map_err(|error| failure_from(DRIVER_REASON, error))?;
    let matched = crate::utils::canonicalize_existing(
        &sysroot
            .join("bin")
            .join(format!("clippy-driver{}", std::env::consts::EXE_SUFFIX)),
    )
    .map_err(|error| failure_from(DRIVER_REASON, error))?;
    if selected != matched {
        return Err(failure(
            DRIVER_REASON,
            "clippy-driver is not the matched compiler sysroot's driver",
        ));
    }
    let (digest, guard, _) =
        capture_guarded_file(&matched, started, budget).map_err(|error| failure_from(DRIVER_REASON, error))?;
    guards.insert(matched, guard);
    Ok(digest)
}

/// Follow Clippy's configuration lookup and bind every candidate it checks.
///
/// The lookup starts at `CLIPPY_CONF_DIR`, else `CARGO_MANIFEST_DIR`, else the working directory,
/// checks both names in each directory, and stops after the first directory that holds a file.
/// Clippy records the loaded file in dep-info, so that file must be a repository input.
fn configuration_trail(
    start: &Path,
    workspace_root: &Path,
    started: Instant,
    budget: &mut NativeCaptureBudget,
    guards: &mut BTreeMap<PathBuf, NativeMetadataGuard>,
) -> Result<Vec<PathState>, NativeInputFailure> {
    let mut directory =
        crate::utils::canonicalize_existing(start).map_err(|error| failure_from(CONFIGURATION_REASON, error))?;
    let mut trail = Vec::new();
    loop {
        let mut loaded = None;
        for name in CONFIGURATION_NAMES {
            let candidate = directory.join(name);
            // Clippy treats a candidate it cannot canonicalize as absent.
            let state = match crate::utils::canonicalize_existing(&candidate) {
                Ok(target) => match fs::metadata(&target) {
                    Ok(metadata) if metadata.is_dir() => PathKind::Directory,
                    Ok(_) => {
                        let state = file_state(&target, started, budget, guards)
                            .map_err(|error| failure_from(CONFIGURATION_REASON, error))?;
                        loaded.get_or_insert(target);
                        state
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => PathKind::Absent,
                    Err(error) => return Err(failure_from(CONFIGURATION_REASON, error)),
                },
                Err(_) => PathKind::Absent,
            };
            trail.push(PathState {
                path: utf8_path(&candidate).map_err(|error| failure_from(CONFIGURATION_REASON, error))?,
                state,
            });
        }
        if let Some(loaded) = loaded {
            if !loaded.starts_with(workspace_root) {
                return Err(failure(
                    EXTERNAL_CONFIGURATION_REASON,
                    "Clippy loads a configuration file outside the repository",
                ));
            }
            return Ok(trail);
        }
        if !directory.pop() {
            return Ok(trail);
        }
    }
}

type CargoMetadataInputs = (PathState, Vec<PathState>);

/// Bind what `cargo metadata` reads when a `clippy::cargo` lint runs it from this package.
///
/// These are the `cargo` executable, Cargo configuration from files and non-secret `CARGO_*`
/// variables, the workspace manifests with every path dependency they reach, and the lockfile.
/// The lockfile pins registry and Git packages, whose sources are immutable once unpacked.
fn cargo_metadata_inputs(
    current_directory: &Path,
    cargo: &Path,
    cargo_home: &Path,
    started: Instant,
    budget: &mut NativeCaptureBudget,
    guards: &mut BTreeMap<PathBuf, NativeMetadataGuard>,
) -> RailResult<CargoMetadataInputs> {
    let cargo_executable = PathState {
        path: utf8_path(cargo)?,
        state: file_state(cargo, started, budget, guards)?,
    };

    let mut inputs = BTreeMap::new();
    let mut record = |path: PathBuf, budget: &mut NativeCaptureBudget| -> RailResult<()> {
        if let std::collections::btree_map::Entry::Vacant(entry) = inputs.entry(path) {
            let state = file_state(entry.key(), started, budget, guards)?;
            entry.insert(state);
        }
        Ok(())
    };
    for directory in current_directory.ancestors() {
        for name in [".cargo/config", ".cargo/config.toml"] {
            record(directory.join(name), budget)?;
        }
    }
    for name in ["config", "config.toml"] {
        record(cargo_home.join(name), budget)?;
    }

    // Cargo walks up from the package manifest to the first manifest that declares a workspace.
    let mut root = None;
    let mut package_root = None;
    for directory in current_directory.ancestors() {
        let manifest = directory.join("Cargo.toml");
        record(manifest.clone(), budget)?;
        let Some(document) = read_manifest(&manifest)? else {
            continue;
        };
        if package_root.is_none() {
            if document
                .get("package")
                .and_then(|package| package.get("workspace"))
                .is_some()
            {
                return Err(RailError::message(
                    "a package that names its workspace root is not modeled",
                ));
            }
            package_root = Some(directory.to_path_buf());
        }
        if document.get("workspace").is_some() {
            root = Some(directory.to_path_buf());
            break;
        }
    }
    let root = root
        .or(package_root)
        .ok_or_else(|| RailError::message("Clippy's package has no Cargo manifest"))?;
    record(root.join("Cargo.lock"), budget)?;

    let mut pending = vec![root.join("Cargo.toml")];
    let mut visited = BTreeSet::new();
    while let Some(manifest) = pending.pop() {
        if !visited.insert(manifest.clone()) {
            continue;
        }
        if visited.len() > MAX_CARGO_MANIFESTS {
            return Err(RailError::message("the workspace manifest closure exceeds its bound"));
        }
        record(manifest.clone(), budget)?;
        let Some(document) = read_manifest(&manifest)? else {
            continue;
        };
        let directory = manifest
            .parent()
            .ok_or_else(|| RailError::message("Cargo manifest has no directory"))?;
        if manifest == root.join("Cargo.toml")
            && let Some(members) = document
                .get("workspace")
                .and_then(|workspace| workspace.get("members"))
                .and_then(toml_edit::Item::as_array)
        {
            for member in members.iter() {
                let member = member
                    .as_str()
                    .ok_or_else(|| RailError::message("workspace member is not a string"))?;
                // Only the member entry is a pattern; the workspace path is matched literally.
                let pattern = format!("{}/{member}", glob::Pattern::escape(&utf8_path(directory)?));
                for matched in glob::glob(&pattern).map_err(|error| RailError::message(error.to_string()))? {
                    let matched = matched.map_err(|error| RailError::message(error.to_string()))?;
                    if matched.is_dir() {
                        pending.push(normalized_directory(&matched).join("Cargo.toml"));
                    }
                }
            }
        }
        for path in path_dependencies(&document) {
            pending.push(normalized_directory(&directory.join(path)).join("Cargo.toml"));
        }
    }
    let cargo_inputs = inputs
        .into_iter()
        .map(|(path, state)| {
            Ok(PathState {
                path: utf8_path(&path)?,
                state,
            })
        })
        .collect::<RailResult<Vec<_>>>()?;
    Ok((cargo_executable, cargo_inputs))
}

/// Cargo reads any `CARGO_*` variable as configuration. Secret-named values, Cargo-Rail's
/// private variables, the jobserver, and Clippy's primary-package flag stay out of the key.
fn cargo_environment(names: impl Iterator<Item = String>) -> Vec<EnvironmentValue> {
    names
        .filter(|name| {
            name.starts_with("CARGO_")
                && !name.starts_with("CARGO_RAIL_")
                && !matches!(name.as_str(), "CARGO_MAKEFLAGS" | "CARGO_PRIMARY_PACKAGE")
                && !crate::compiler::observation::is_secret_name(name)
        })
        .chain(CARGO_TOOLCHAIN_ENVIRONMENT.iter().map(|name| (*name).to_string()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|name| environment_value(&name))
        .collect()
}

fn path_dependencies(document: &toml_edit::DocumentMut) -> Vec<String> {
    fn dependency_paths(table: Option<&dyn toml_edit::TableLike>, paths: &mut Vec<String>) {
        let Some(table) = table else {
            return;
        };
        for (_, dependency) in table.iter() {
            if let Some(path) = dependency
                .as_table_like()
                .and_then(|dependency| dependency.get("path"))
                .and_then(toml_edit::Item::as_str)
            {
                paths.push(path.to_string());
            }
        }
    }

    let mut paths = Vec::new();
    let item = document.as_item();
    for name in DEPENDENCY_TABLES {
        dependency_paths(item.get(name).and_then(toml_edit::Item::as_table_like), &mut paths);
    }
    if let Some(targets) = item.get("target").and_then(toml_edit::Item::as_table_like) {
        for (_, target) in targets.iter() {
            for name in DEPENDENCY_TABLES {
                dependency_paths(target.get(name).and_then(toml_edit::Item::as_table_like), &mut paths);
            }
        }
    }
    dependency_paths(
        item.get("workspace")
            .and_then(|workspace| workspace.get("dependencies"))
            .and_then(toml_edit::Item::as_table_like),
        &mut paths,
    );
    if let Some(patches) = item.get("patch").and_then(toml_edit::Item::as_table_like) {
        for (_, source) in patches.iter() {
            dependency_paths(source.as_table_like(), &mut paths);
        }
    }
    dependency_paths(item.get("replace").and_then(toml_edit::Item::as_table_like), &mut paths);
    paths
}

fn read_manifest(path: &Path) -> RailResult<Option<toml_edit::DocumentMut>> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() {
        return Ok(None);
    }
    if metadata.len() > MAX_CARGO_MANIFEST_BYTES {
        return Err(RailError::message("Cargo manifest exceeds its byte bound"));
    }
    let text = fs::read_to_string(path)?;
    text.parse::<toml_edit::DocumentMut>()
        .map(Some)
        .map_err(|error| RailError::message(format!("Cargo manifest '{}' is not TOML: {error}", path.display())))
}

/// Resolve a directory through symlinks when it exists, as Cargo reads it.
fn normalized_directory(directory: &Path) -> PathBuf {
    crate::utils::canonicalize_existing(directory).unwrap_or_else(|_| directory.to_path_buf())
}

fn file_state(
    path: &Path,
    started: Instant,
    budget: &mut NativeCaptureBudget,
    guards: &mut BTreeMap<PathBuf, NativeMetadataGuard>,
) -> RailResult<PathKind> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(PathKind::Absent),
        Err(error) => return Err(error.into()),
    };
    if metadata.is_dir() {
        return Ok(PathKind::Directory);
    }
    let target = crate::utils::canonicalize_existing(path)?;
    let (content_digest, guard, bytes) = capture_guarded_file(&target, started, budget)?;
    guards.insert(target.clone(), guard);
    Ok(PathKind::File {
        target: utf8_path(&target)?,
        content_digest,
        bytes,
    })
}

fn environment_value(name: &str) -> EnvironmentValue {
    EnvironmentValue {
        name: name.to_string(),
        value_digest: std::env::var_os(name)
            .map(|value| format!("sha256:{}", ContentDigest::sha256(value.as_encoded_bytes()))),
    }
}

fn utf8_path(path: &Path) -> RailResult<String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| RailError::message("Clippy input path is not UTF-8"))
}

fn failure(reason: &'static str, message: &str) -> NativeInputFailure {
    NativeInputFailure::new(reason, RailError::message(message))
}

fn failure_from(reason: &'static str, error: impl Into<RailError>) -> NativeInputFailure {
    NativeInputFailure::new(reason, error.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget() -> NativeCaptureBudget {
        NativeCaptureBudget::new(super::super::NATIVE_CAPTURE_LIMITS)
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().expect("parent")).expect("directory");
        fs::write(path, contents).expect("file");
    }

    #[test]
    fn clippy_arguments_follow_the_driver_transform() {
        let plain = strings(&["--crate-name", "member", "src/lib.rs"]);
        assert_eq!(
            clippy_arguments(&plain, None, false),
            (true, strings(&["--cfg", "clippy"]))
        );
        assert_eq!(
            clippy_arguments(&plain, Some("-D__CLIPPY_HACKERY__warnings__CLIPPY_HACKERY__"), false),
            (true, strings(&["-D", "warnings", "--cfg", "clippy"]))
        );
        let capped = strings(&["--cap-lints", "allow", "src/lib.rs"]);
        assert_eq!(
            clippy_arguments(&capped, Some("-Dwarnings"), false),
            (false, Vec::new())
        );
        let forced = strings(&["--cap-lints=allow", "--force-warn=clippy::pedantic"]);
        assert!(clippy_arguments(&forced, None, false).0);
        let no_deps = Some("--no-deps__CLIPPY_HACKERY__-Wclippy::pedantic");
        assert_eq!(clippy_arguments(&plain, no_deps, false), (false, Vec::new()));
        assert_eq!(
            clippy_arguments(&plain, no_deps, true),
            (true, strings(&["-Wclippy::pedantic", "--cfg", "clippy"]))
        );
    }

    #[test]
    fn configuration_trail_binds_every_candidate_up_to_the_loaded_file() {
        let root = tempfile::tempdir().expect("root");
        let workspace = crate::utils::canonicalize_existing(root.path()).expect("workspace");
        let member = workspace.join("crates/member");
        fs::create_dir_all(&member).expect("member");
        write(&workspace.join("clippy.toml"), "msrv = \"1.80\"\n");
        let trail = |start: &Path, root: &Path| {
            configuration_trail(start, root, Instant::now(), &mut budget(), &mut BTreeMap::new())
        };

        let loaded = trail(&member, &workspace).expect("trail");
        let checked = loaded.iter().map(|entry| entry.path.as_str()).collect::<Vec<_>>();
        let expected = [&member, &workspace.join("crates"), &workspace]
            .iter()
            .flat_map(|directory| CONFIGURATION_NAMES.map(|name| directory.join(name)))
            .collect::<Vec<_>>();
        assert_eq!(
            checked,
            expected.iter().map(|path| path.to_str().unwrap()).collect::<Vec<_>>()
        );
        assert!(matches!(loaded.last().unwrap().state, PathKind::File { .. }));
        assert!(
            loaded[..loaded.len() - 1]
                .iter()
                .all(|entry| entry.state == PathKind::Absent)
        );

        write(&workspace.join("clippy.toml"), "msrv = \"1.81\"\n");
        let edited = trail(&member, &workspace).expect("edited trail");
        assert_ne!(edited, loaded, "configuration bytes are bound");

        fs::create_dir(workspace.join("crates/.clippy.toml")).expect("directory candidate");
        let directory = trail(&member, &workspace).expect("directory candidate trail");
        assert_eq!(directory[2].state, PathKind::Directory);
        assert_eq!(
            directory.len(),
            edited.len(),
            "a directory candidate does not stop the lookup"
        );

        write(&workspace.join("crates/clippy.toml"), "msrv = \"1.82\"\n");
        let nearer = trail(&member, &workspace).expect("nearer trail");
        assert_eq!(nearer.len(), 4, "the lookup stops at the nearer file");
        assert_ne!(nearer, directory);

        let failure = trail(&member, &member).expect_err("configuration outside the repository");
        assert_eq!(failure.reason, EXTERNAL_CONFIGURATION_REASON);
    }

    #[test]
    fn cargo_metadata_inputs_bind_manifests_lockfile_and_configuration() {
        let root = tempfile::tempdir().expect("root");
        let base = crate::utils::canonicalize_existing(root.path()).expect("base");
        let workspace = base.join("workspace");
        write(
            &workspace.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.dependencies]\noutside = { path = \"../outside\" }\n",
        );
        write(
            &workspace.join("crates/a/Cargo.toml"),
            "[package]\nname = \"a\"\n\n[target.'cfg(unix)'.dependencies]\nb = { path = \"../b\" }\n",
        );
        write(&workspace.join("crates/b/Cargo.toml"), "[package]\nname = \"b\"\n");
        write(&base.join("outside/Cargo.toml"), "[package]\nname = \"outside\"\n");
        write(&workspace.join("Cargo.lock"), "version = 4\n");
        write(&workspace.join(".cargo/config.toml"), "[build]\njobs = 2\n");
        write(&base.join("toolchain/cargo"), "cargo executable");
        let cargo_home = base.join("cargo-home");
        fs::create_dir(&cargo_home).expect("Cargo home");
        let package = workspace.join("crates/a");
        let capture = || {
            cargo_metadata_inputs(
                &package,
                &base.join("toolchain/cargo"),
                &cargo_home,
                Instant::now(),
                &mut budget(),
                &mut BTreeMap::new(),
            )
        };

        let (executable, inputs) = capture().expect("Cargo inputs");
        assert!(matches!(executable.state, PathKind::File { .. }));
        let bound = |inputs: &[PathState], path: &Path| {
            inputs
                .iter()
                .find(|entry| entry.path == path.to_str().unwrap())
                .map(|entry| entry.state.clone())
        };
        for file in [
            workspace.join("Cargo.toml"),
            workspace.join("Cargo.lock"),
            workspace.join(".cargo/config.toml"),
            workspace.join("crates/a/Cargo.toml"),
            workspace.join("crates/b/Cargo.toml"),
            base.join("outside/Cargo.toml"),
        ] {
            assert!(
                matches!(bound(&inputs, &file), Some(PathKind::File { .. })),
                "{} is bound",
                file.display()
            );
        }
        for absent in [
            package.join(".cargo/config.toml"),
            base.join(".cargo/config.toml"),
            cargo_home.join("config.toml"),
            workspace.join("crates/Cargo.toml"),
        ] {
            assert_eq!(bound(&inputs, &absent), Some(PathKind::Absent), "{}", absent.display());
        }
        assert_eq!(
            bound(&inputs, &base.join("Cargo.toml")),
            None,
            "Cargo stops its manifest search at the workspace root"
        );

        for (label, change) in [
            ("lockfile", workspace.join("Cargo.lock")),
            ("path dependency outside the workspace", base.join("outside/Cargo.toml")),
            ("configuration", cargo_home.join("config.toml")),
            ("new member", workspace.join("crates/c/Cargo.toml")),
        ] {
            let before = capture().expect("before").1;
            write(&change, "[package]\nname = \"changed\"\n");
            assert_ne!(capture().expect("after").1, before, "{label} is bound");
        }

        write(
            &package.join("Cargo.toml"),
            "[package]\nname = \"a\"\nworkspace = \"../..\"\n",
        );
        assert!(capture().is_err(), "an explicit workspace pointer is not modeled");
    }

    #[test]
    fn cargo_environment_excludes_private_secret_and_per_process_values() {
        let names = [
            "CARGO_HOME",
            "CARGO_BUILD_TARGET_DIR",
            "CARGO_PKG_RUST_VERSION",
            "CARGO_RAIL_CACHE",
            "CARGO_MAKEFLAGS",
            "CARGO_PRIMARY_PACKAGE",
            "CARGO_REGISTRY_TOKEN",
            "CARGO_REGISTRIES_PRIVATE_TOKEN",
            "PATH",
        ];
        let environment = cargo_environment(names.iter().map(|name| (*name).to_string()));
        assert_eq!(
            environment.iter().map(|value| value.name.as_str()).collect::<Vec<_>>(),
            [
                "CARGO_BUILD_TARGET_DIR",
                "CARGO_HOME",
                "CARGO_PKG_RUST_VERSION",
                "RUSTC",
                "RUSTUP_TOOLCHAIN",
            ]
        );
    }

    #[test]
    fn driver_identity_is_the_matched_sysroot_executable() {
        let root = tempfile::tempdir().expect("root");
        let sysroot = crate::utils::canonicalize_existing(root.path()).expect("sysroot");
        let driver = sysroot.join(format!("bin/clippy-driver{}", std::env::consts::EXE_SUFFIX));
        write(&driver, "first driver");
        let capture = |driver: &Path| {
            capture_driver(
                driver,
                &sysroot,
                &sysroot,
                Instant::now(),
                &mut budget(),
                &mut BTreeMap::new(),
            )
        };
        let first = capture(&driver).expect("driver digest");
        write(&driver, "second driver");
        assert_ne!(capture(&driver).expect("changed driver digest"), first);
        let other = sysroot.join("other/clippy-driver");
        write(&other, "second driver");
        assert_eq!(capture(&other).expect_err("unmatched driver").reason, DRIVER_REASON);
    }
}
