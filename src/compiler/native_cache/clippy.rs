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

use super::cargo_inputs::{
    CargoInputs, EnvironmentValue, PathKind, PathState, PortableRoots, environment_value, file_state, path_state,
};
use super::{NativeCaptureBudget, NativeInputFailure, NativeMetadataGuard, capture_guarded_file};

const CLIPPY_CAPTURE_VERSION: u32 = 2;
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
    /// What `cargo metadata` reads when a `clippy::cargo` lint runs it from the package.
    cargo: CargoInputs,
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
    portable: Option<PathBuf>,
}

impl ClippyActionCapture {
    /// Capture Clippy's inputs for one invocation run from the current directory and environment.
    ///
    /// `portable` names the installed compiler wrapper when `--root-portability remap` shares results across
    /// checkout roots; paths below the repository, the Cargo home, and the sysroot are then spelled relative
    /// to those roots.
    pub(super) fn capture(
        driver: &Path,
        sysroot: &Path,
        workspace_root: &Path,
        compiler_arguments: &[String],
        portable: Option<&Path>,
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
        let roots = portable
            .map(|installed_wrapper| {
                crate::cargo::CargoConfigSnapshot::cargo_home(&current_directory)
                    .and_then(|cargo_home| PortableRoots::new(workspace_root, &cargo_home, sysroot, installed_wrapper))
            })
            .transpose()
            .map_err(|error| failure_from(CARGO_REASON, error))?;
        let roots = roots.as_ref();
        let mut guards = BTreeMap::new();
        let driver_digest = capture_driver(driver, sysroot, &current_directory, started, budget, &mut guards)?;
        let (lints, appended_arguments) = clippy_arguments(
            compiler_arguments,
            std::env::var("CLIPPY_ARGS").ok().as_deref(),
            std::env::var_os("CARGO_PRIMARY_PACKAGE").is_some(),
        );
        let environment = CLIPPY_ENVIRONMENT
            .iter()
            .map(|name| environment_value(name, roots))
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
        let configuration = configuration_trail(&configuration_start, &workspace, roots, started, budget, &mut guards)?;
        let cargo = (|| {
            // Cargo gives every compiler process its own executable; without it Clippy searches PATH.
            let cargo = std::env::var_os("CARGO")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .ok_or_else(|| RailError::message("Clippy's cargo executable is not an absolute CARGO path"))?;
            CargoInputs::capture(&current_directory, &current_directory, &cargo, roots, budget)
        })()
        .map_err(|error| failure_from(CARGO_REASON, error))?;
        Ok(Self {
            version: CLIPPY_CAPTURE_VERSION,
            driver_digest,
            lints,
            appended_arguments,
            environment,
            configuration,
            cargo,
            guards,
            authority: ClippyCaptureAuthority {
                driver: driver.to_path_buf(),
                sysroot: sysroot.to_path_buf(),
                workspace_root: workspace_root.to_path_buf(),
                compiler_arguments: compiler_arguments.to_vec(),
                portable: portable.map(Path::to_path_buf),
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
            self.authority.portable.as_deref(),
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
    roots: Option<&PortableRoots>,
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
                        let state = file_state(&target, roots, started, budget, guards)
                            .map_err(|error| failure_from(CONFIGURATION_REASON, error))?;
                        loaded.get_or_insert(target);
                        state
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => PathKind::Absent,
                    Err(error) => return Err(failure_from(CONFIGURATION_REASON, error)),
                },
                Err(_) => PathKind::Absent,
            };
            trail.extend(
                path_state(&candidate, state, roots).map_err(|error| failure_from(CONFIGURATION_REASON, error))?,
            );
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
        let member = workspace.join("crates").join("member");
        fs::create_dir_all(&member).expect("member");
        write(&workspace.join("clippy.toml"), "msrv = \"1.80\"\n");
        let trail = |start: &Path, root: &Path| {
            configuration_trail(start, root, None, Instant::now(), &mut budget(), &mut BTreeMap::new())
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
