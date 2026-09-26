//! What Cargo reads when a compiler-side process asks it about the workspace.
//!
//! Clippy's `clippy::cargo` lints run `cargo metadata` from the package directory, and procedural macros
//! such as those built on `proc-macro-crate` run `cargo locate-project`. Both answers depend on the same
//! inputs: the `cargo` executable, Cargo configuration from files and non-secret `CARGO_*` variables, the
//! manifest search from the package up to the workspace root, the workspace manifests with every path
//! dependency they reach, and the lockfile. The lockfile pins registry and Git packages, whose sources are
//! immutable once unpacked. Binding this superset keeps a hit valid exactly while Cargo's answer is.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::error::{RailError, RailResult};
use crate::source::ContentDigest;

use super::{NativeCaptureBudget, NativeMetadataGuard, capture_guarded_file};

/// Compiler selections that Cargo reads when it queries rustc.
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

/// Cargo's inputs for one query, captured from the current process environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CargoInputs {
    executable: PathState,
    environment: Vec<EnvironmentValue>,
    inputs: Vec<PathState>,
    /// Generations of every bound file, so a recapture also rejects a change that restores the old bytes.
    #[serde(skip)]
    guards: BTreeMap<PathBuf, NativeMetadataGuard>,
}

impl CargoInputs {
    /// Capture what `cargo` reads for a query about the package in `manifest_directory`, run from
    /// `current_directory` with this process's environment.
    /// `roots` selects portable spelling; without it every path is spelled as the host names it.
    pub(super) fn capture(
        manifest_directory: &Path,
        current_directory: &Path,
        cargo: &Path,
        roots: Option<&PortableRoots>,
        budget: &mut NativeCaptureBudget,
    ) -> RailResult<Self> {
        let cargo_home = crate::cargo::CargoConfigSnapshot::cargo_home(current_directory)?;
        Self::capture_with_home(manifest_directory, current_directory, cargo, &cargo_home, roots, budget)
    }

    fn capture_with_home(
        manifest_directory: &Path,
        current_directory: &Path,
        cargo: &Path,
        cargo_home: &Path,
        roots: Option<&PortableRoots>,
        budget: &mut NativeCaptureBudget,
    ) -> RailResult<Self> {
        let started = Instant::now();
        let mut guards = BTreeMap::new();
        let guards_ref = &mut guards;
        let executable = path_state(cargo, file_state(cargo, roots, started, budget, guards_ref)?, roots)?
            .ok_or_else(|| RailError::message("the cargo executable is absent"))?;
        let guards = guards_ref;
        let mut inputs = BTreeMap::new();
        let mut configuration = Vec::new();
        for directory in current_directory.ancestors() {
            for name in [".cargo/config", ".cargo/config.toml"] {
                configuration.push(directory.join(name));
            }
        }
        for name in ["config", "config.toml"] {
            configuration.push(cargo_home.join(name));
        }
        for path in configuration {
            if let std::collections::btree_map::Entry::Vacant(entry) = inputs.entry(path) {
                let state = configuration_state(entry.key(), roots, started, budget, guards)?;
                entry.insert(state);
            }
        }
        let mut record = |path: PathBuf, budget: &mut NativeCaptureBudget| -> RailResult<()> {
            if let std::collections::btree_map::Entry::Vacant(entry) = inputs.entry(path) {
                let state = file_state(entry.key(), roots, started, budget, guards)?;
                entry.insert(state);
            }
            Ok(())
        };

        // Cargo walks up from the package manifest to the first manifest that declares a workspace.
        let mut root = None;
        let mut package_root = None;
        for directory in manifest_directory.ancestors() {
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
            .ok_or_else(|| RailError::message("the queried package has no Cargo manifest"))?;
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
        let inputs = inputs
            .into_iter()
            .filter_map(|(path, state)| path_state(&path, state, roots).transpose())
            .collect::<RailResult<Vec<_>>>()?;
        Ok(Self {
            executable,
            environment: cargo_environment(
                std::env::vars_os().filter_map(|(name, _)| name.into_string().ok()),
                roots,
            ),
            inputs,
            guards: std::mem::take(guards),
        })
    }
}

/// Cargo reads any `CARGO_*` variable as configuration. Secret-named values, Cargo-Rail's
/// private variables, the jobserver, and Clippy's primary-package flag stay out of the key.
pub(super) fn cargo_environment(
    names: impl Iterator<Item = String>,
    roots: Option<&PortableRoots>,
) -> Vec<EnvironmentValue> {
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
        .map(|name| environment_value(&name, roots))
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

pub(super) fn read_manifest(path: &Path) -> RailResult<Option<toml_edit::DocumentMut>> {
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
pub(super) fn normalized_directory(directory: &Path) -> PathBuf {
    crate::utils::canonicalize_existing(directory).unwrap_or_else(|_| directory.to_path_buf())
}

pub(super) fn file_state(
    path: &Path,
    roots: Option<&PortableRoots>,
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
        target: spelled_path(&target, roots)?,
        content_digest,
        bytes,
    })
}

/// A variable's value digest. Under portable spelling, a value that names a path below a bound root
/// is digested in its portable spelling, tagged so that it never equals a literal value.
pub(super) fn environment_value(name: &str, roots: Option<&PortableRoots>) -> EnvironmentValue {
    let digest = |bytes: &[u8]| format!("sha256:{}", ContentDigest::sha256(bytes));
    EnvironmentValue {
        name: name.to_string(),
        value_digest: std::env::var_os(name).map(|value| {
            let path = Path::new(&value);
            match roots.filter(|_| path.is_absolute()).and_then(|roots| roots.spell(path)) {
                Some(spelled) => digest(format!("\0portable\0{spelled}").as_bytes()),
                None => digest(value.as_encoded_bytes()),
            }
        }),
    }
}

/// Roots whose contents an action binds, so a path below one of them can be spelled relative to it and a
/// result can be shared across checkout roots under `--root-portability remap`.
///
/// Cargo and Clippy read these paths only for their contents or absence, which the capture binds, and a
/// relative path in a configuration file resolves against the file's own directory, which the spelling
/// keeps. A path outside every root keeps its host spelling, so such a result stays with its root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PortableRoots {
    roots: [(&'static str, PathBuf); 3],
    /// The installed compiler wrapper, which cache setup names in each Cargo home's configuration.
    installed_wrapper: PathBuf,
}

impl PortableRoots {
    pub(super) fn new(
        repository: &Path,
        cargo_home: &Path,
        sysroot: &Path,
        installed_wrapper: &Path,
    ) -> RailResult<Self> {
        Ok(Self {
            roots: [
                ("repository", crate::utils::canonicalize_existing(repository)?),
                ("cargo-home", normalized_directory(cargo_home)),
                ("sysroot", crate::utils::canonicalize_existing(sysroot)?),
            ],
            installed_wrapper: normalized_directory(installed_wrapper),
        })
    }

    /// A Cargo configuration without its `build.rustc-wrapper` entry when that entry selects the installed
    /// wrapper, else `None`.
    ///
    /// Setup writes the wrapper's absolute path into the Cargo home, so the entry differs on every machine.
    /// The wrapper passes Cargo's compiler queries through unchanged and the installation binds its identity,
    /// so the entry cannot change what Cargo answers. Any other wrapper stays bound.
    fn without_installed_wrapper(&self, text: &str) -> Option<String> {
        let mut document = text.parse::<toml_edit::DocumentMut>().ok()?;
        let build = document.get_mut("build")?.as_table_like_mut()?;
        let wrapper = Path::new(build.get("rustc-wrapper")?.as_str()?);
        if !wrapper.is_absolute() || normalized_directory(wrapper) != self.installed_wrapper {
            return None;
        }
        build.remove("rustc-wrapper");
        Some(document.to_string())
    }

    /// `root:relative` for a path below a root, resolving the parent directory through symlinks as the
    /// operating system does.
    pub(super) fn spell(&self, path: &Path) -> Option<String> {
        let resolved = path
            .parent()
            .zip(path.file_name())
            .map(|(parent, name)| normalized_directory(parent).join(name));
        self.roots.iter().find_map(|(name, root)| {
            let relative = path
                .strip_prefix(root)
                .ok()
                .or_else(|| resolved.as_deref()?.strip_prefix(root).ok())?;
            let relative = relative.to_str()?.replace('\\', "/");
            Some(format!("{name}:{relative}"))
        })
    }
}

/// A Cargo configuration file's state. Under portable spelling, the installed wrapper entry is left out of
/// its content digest; the tagged digest never equals the digest of a file's literal bytes.
fn configuration_state(
    path: &Path,
    roots: Option<&PortableRoots>,
    started: Instant,
    budget: &mut NativeCaptureBudget,
    guards: &mut BTreeMap<PathBuf, NativeMetadataGuard>,
) -> RailResult<PathKind> {
    let state = file_state(path, roots, started, budget, guards)?;
    let (
        Some(roots),
        PathKind::File {
            target,
            content_digest,
            bytes,
        },
    ) = (roots, &state)
    else {
        return Ok(state);
    };
    let text = fs::read(crate::utils::canonicalize_existing(path)?)?;
    if format!("sha256:{}", ContentDigest::sha256(&text)) != *content_digest {
        return Err(RailError::message("Cargo configuration changed during capture"));
    }
    let Some(normalized) = std::str::from_utf8(&text)
        .ok()
        .and_then(|text| roots.without_installed_wrapper(text))
    else {
        return Ok(state);
    };
    let mut tagged = b"\0cargo-configuration-without-installed-wrapper\0".to_vec();
    tagged.extend_from_slice(normalized.as_bytes());
    Ok(PathKind::File {
        target: target.clone(),
        content_digest: format!("sha256:{}", ContentDigest::sha256(&tagged)),
        bytes: *bytes,
    })
}

/// A path's spelling in the identity: portable below a bound root, else as the host names it.
pub(super) fn spelled_path(path: &Path, roots: Option<&PortableRoots>) -> RailResult<String> {
    match roots.and_then(|roots| roots.spell(path)) {
        Some(spelled) => Ok(spelled),
        None => utf8_path(path),
    }
}

/// One bound path, or `None` for an absent path outside every portable root. Cargo reads nothing there,
/// and every lookup recaptures the inputs, so a file that appears later still changes the identity.
pub(super) fn path_state(path: &Path, state: PathKind, roots: Option<&PortableRoots>) -> RailResult<Option<PathState>> {
    if let Some(roots) = roots {
        match roots.spell(path) {
            Some(spelled) => return Ok(Some(PathState { path: spelled, state })),
            None if state == PathKind::Absent => return Ok(None),
            None => {}
        }
    }
    Ok(Some(PathState {
        path: utf8_path(path)?,
        state,
    }))
}

pub(super) fn utf8_path(path: &Path) -> RailResult<String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| RailError::message("Cargo input path is not UTF-8"))
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EnvironmentValue {
    name: String,
    value_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PathState {
    pub(super) path: String,
    pub(super) state: PathKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum PathKind {
    Absent,
    Directory,
    File {
        target: String,
        content_digest: String,
        bytes: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::super::NATIVE_CAPTURE_LIMITS;
    use super::*;

    fn budget() -> NativeCaptureBudget {
        NativeCaptureBudget::new(NATIVE_CAPTURE_LIMITS)
    }

    /// A relative path with the host's separators, as Cargo's manifest search spells it.
    fn native(path: &str) -> PathBuf {
        path.split('/').collect()
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().expect("parent")).expect("parent directory");
        fs::write(path, contents).expect("fixture file");
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
            &workspace.join(native("crates/a/Cargo.toml")),
            "[package]\nname = \"a\"\n\n[target.'cfg(unix)'.dependencies]\nb = { path = \"../b\" }\n",
        );
        write(
            &workspace.join(native("crates/b/Cargo.toml")),
            "[package]\nname = \"b\"\n",
        );
        write(
            &base.join(native("outside/Cargo.toml")),
            "[package]\nname = \"outside\"\n",
        );
        write(&workspace.join("Cargo.lock"), "version = 4\n");
        write(&workspace.join(".cargo/config.toml"), "[build]\njobs = 2\n");
        write(&base.join("toolchain/cargo"), "cargo executable");
        let cargo_home = base.join("cargo-home");
        fs::create_dir(&cargo_home).expect("Cargo home");
        let package = workspace.join(native("crates/a"));
        let capture = || {
            CargoInputs::capture_with_home(
                &package,
                &package,
                &base.join("toolchain/cargo"),
                &cargo_home,
                None,
                &mut budget(),
            )
            .map(|inputs| inputs.inputs)
        };

        let inputs = capture().expect("Cargo inputs");
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
            workspace.join(native("crates/a/Cargo.toml")),
            workspace.join(native("crates/b/Cargo.toml")),
            base.join(native("outside/Cargo.toml")),
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
            workspace.join(native("crates/Cargo.toml")),
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
            (
                "path dependency outside the workspace",
                base.join(native("outside/Cargo.toml")),
            ),
            ("configuration", cargo_home.join("config.toml")),
            ("new member", workspace.join(native("crates/c/Cargo.toml"))),
        ] {
            let before = capture().expect("before");
            write(&change, "[package]\nname = \"changed\"\n");
            assert_ne!(capture().expect("after"), before, "{label} is bound");
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
        let environment = cargo_environment(names.iter().map(|name| (*name).to_string()), None);
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
    fn portable_spelling_shares_identical_inputs_across_checkout_roots() {
        let first = tempfile::tempdir().expect("first root");
        let second = tempfile::tempdir().expect("second root");
        // Setup writes each Cargo home's own absolute wrapper path into its configuration.
        let home_configuration = |base: &Path, retry: u8| {
            write(
                &base.join("home/config.toml"),
                &format!(
                    // A literal string, as a Windows path holds backslashes.
                    "[build]\nrustc-wrapper = '{}'\n\n[net]\nretry = {retry}\n",
                    base.join("home/cargo-rail/wrapper").display()
                ),
            );
        };
        let layout = |base: &Path| {
            let base = crate::utils::canonicalize_existing(base).expect("base");
            write(
                &base.join("workspace/Cargo.toml"),
                "[workspace]\nmembers = [\"crates/a\"]\n",
            );
            write(&base.join("workspace/crates/a/Cargo.toml"), "[package]\nname = \"a\"\n");
            write(&base.join("workspace/Cargo.lock"), "version = 4\n");
            home_configuration(&base, 3);
            write(&base.join("toolchain/bin/cargo"), "cargo executable");
            base
        };
        let (first, second) = (layout(first.path()), layout(second.path()));
        let capture = |base: &Path, portable: bool| {
            let workspace = base.join("workspace");
            let package = workspace.join("crates/a");
            let wrapper = base.join("home/cargo-rail/wrapper");
            let roots =
                PortableRoots::new(&workspace, &base.join("home"), &base.join("toolchain"), &wrapper).expect("roots");
            let inputs = CargoInputs::capture_with_home(
                &package,
                &package,
                &base.join("toolchain/bin/cargo"),
                &base.join("home"),
                portable.then_some(&roots),
                &mut budget(),
            )
            .expect("Cargo inputs");
            serde_json::to_string(&inputs).expect("identity")
        };

        assert_ne!(
            capture(&first, false),
            capture(&second, false),
            "physical spelling binds each root"
        );
        let portable = capture(&first, true);
        assert_eq!(
            portable,
            capture(&second, true),
            "identical inputs share one portable identity"
        );
        assert!(!portable.contains(first.to_str().unwrap()), "{portable}");

        home_configuration(&second, 4);
        assert_ne!(
            capture(&first, true),
            capture(&second, true),
            "the Cargo home configuration is bound"
        );
        home_configuration(&second, 3);
        let installed = fs::read_to_string(second.join("home/config.toml")).expect("home configuration");
        write(
            &second.join("home/config.toml"),
            &installed.replace("home/cargo-rail/wrapper", "home/other-wrapper"),
        );
        assert_ne!(
            capture(&first, true),
            capture(&second, true),
            "another wrapper stays bound"
        );
        write(&second.join("home/config.toml"), &installed);
        write(&second.join("workspace/.cargo/config.toml"), "[build]\njobs = 1\n");
        assert_ne!(
            capture(&first, true),
            capture(&second, true),
            "a new repository configuration is bound"
        );
        std::fs::remove_file(second.join("workspace/.cargo/config.toml")).expect("remove configuration");
        assert_eq!(capture(&first, true), capture(&second, true));

        // An input outside every bound root keeps its host path, so the roots stop sharing a result.
        for base in [&first, &second] {
            write(&base.join(".cargo/config.toml"), "[build]\njobs = 2\n");
        }
        assert_ne!(
            capture(&first, true),
            capture(&second, true),
            "an outside input is never shared"
        );
    }
}
