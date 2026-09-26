//! Cargo's rerun inputs for executed build scripts.
//!
//! Cargo reruns a build script only when a declared `rerun-if-changed` path or
//! `rerun-if-env-changed` variable changes, or, when no path is declared, when its
//! package sources change. Compiler evidence records these inputs for every build script
//! in a view, so a reused view stays valid exactly while Cargo would keep each script's
//! output. Package sources are bound separately: registry sources by the lockfile
//! checksum and workspace-local sources by the member's source-closure fingerprint.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use rscrypto::Sha256;
use serde::{Deserialize, Serialize};

use crate::build_script::result::{RerunDeclaration, rerun_declaration};
use crate::compiler::observation::{FileObservation, ObservationPath, is_secret_name};
use crate::source::ContentDigest;

/// Entries hashed for one declared directory before the script is treated as unobservable.
const MAX_DIRECTORY_ENTRIES: usize = 10_000;

/// The rerun inputs of one executed build script.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BuildScriptFreshness {
    /// Cargo package identity of the script's package.
    package: String,
    /// Declared `rerun-if-changed` paths. Paths inside an immutable registry package are omitted.
    paths: Vec<RerunPath>,
    /// Declared `rerun-if-env-changed` variables, as value digests.
    environment: Vec<RerunEnvironment>,
    /// Names of variables the script set with `rustc-env`; their values are script output.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    rustc_environment: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum RerunPath {
    File { file: FileObservation },
    Directory { path: ObservationPath, tree_digest: String },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RerunEnvironment {
    name: String,
    /// `None` when the variable was unset.
    value_digest: Option<String>,
}

/// One build script that Cargo executed or replayed in a compiler-evidence view.
pub(crate) struct ExecutedBuildScript<'a> {
    pub(crate) package: &'a str,
    /// Cargo's `OUT_DIR`; the script's stdout is its sibling `output` file.
    pub(crate) out_dir: &'a Path,
    /// Directory containing the package manifest, which anchors relative declarations.
    pub(crate) package_root: &'a Path,
    /// The package comes from an immutable, checksummed source.
    pub(crate) immutable_source: bool,
    /// Names of variables Cargo reports the script set with `rustc-env`.
    pub(crate) rustc_environment: &'a [String],
}

/// Record a build script's rerun inputs, or the reason they cannot support reuse.
pub(crate) fn capture(
    script: &ExecutedBuildScript<'_>,
    source_root: &Path,
) -> Result<BuildScriptFreshness, &'static str> {
    let output = script
        .out_dir
        .parent()
        .map(|directory| directory.join("output"))
        .and_then(|path| fs::read(path).ok())
        .ok_or("build_script_output_unavailable")?;
    let output = String::from_utf8_lossy(&output);
    let mut paths = BTreeSet::new();
    let mut environment = BTreeSet::new();
    for line in output.lines() {
        match rerun_declaration(line) {
            Some(RerunDeclaration::Changed(declared)) => {
                let declared = Path::new(declared);
                let path = if declared.is_absolute() {
                    declared.to_path_buf()
                } else {
                    script.package_root.join(declared)
                };
                if script.immutable_source && path.starts_with(script.package_root) {
                    continue;
                }
                paths.insert(capture_path(&path, source_root)?);
            }
            Some(RerunDeclaration::EnvironmentChanged(name)) => {
                if is_secret_name(name) {
                    return Err("build_script_secret_environment");
                }
                environment.insert(RerunEnvironment {
                    name: name.to_string(),
                    value_digest: environment_digest(name),
                });
            }
            None => {}
        }
    }
    let mut rustc_environment = script.rustc_environment.to_vec();
    rustc_environment.sort();
    rustc_environment.dedup();
    Ok(BuildScriptFreshness {
        package: script.package.to_string(),
        paths: paths.into_iter().collect(),
        environment: environment.into_iter().collect(),
        rustc_environment,
    })
}

impl BuildScriptFreshness {
    /// Variables this script set with `rustc-env`.
    pub(crate) fn rustc_environment(&self) -> impl Iterator<Item = &str> {
        self.rustc_environment.iter().map(String::as_str)
    }

    /// Why Cargo would now rerun this build script, if it would.
    pub(crate) fn revalidation_reason(&self, source_root: &Path) -> Option<&'static str> {
        for path in &self.paths {
            let unchanged = match path {
                RerunPath::File { file } => file.revalidate(source_root),
                RerunPath::Directory { path, tree_digest } => {
                    directory_digest(&path.resolve(source_root)).is_ok_and(|current| current == *tree_digest)
                }
            };
            if !unchanged {
                return Some("build_script_input_changed");
            }
        }
        self.environment
            .iter()
            .any(|variable| environment_digest(&variable.name) != variable.value_digest)
            .then_some("build_script_environment_changed")
    }
}

fn capture_path(path: &Path, source_root: &Path) -> Result<RerunPath, &'static str> {
    // Cargo reruns a script on every build while a declared path is missing, so its output
    // is not a stable function of its declared inputs.
    let metadata = fs::symlink_metadata(path).map_err(|_| "build_script_rerun_path_missing")?;
    if metadata.is_dir() {
        return Ok(RerunPath::Directory {
            path: ObservationPath::capture(path, source_root, source_root),
            tree_digest: directory_digest(path)?,
        });
    }
    FileObservation::capture(path, source_root, source_root)
        .map(|file| RerunPath::File { file })
        .map_err(|_| "build_script_rerun_path_unreadable")
}

fn environment_digest(name: &str) -> Option<String> {
    std::env::var_os(name)
        .as_deref()
        .map(OsStr::as_encoded_bytes)
        .map(ContentDigest::sha256)
        .map(|digest| format!("sha256:{digest}"))
}

/// Digest every entry below `root`: relative path, kind, and file content or symlink target.
fn directory_digest(root: &Path) -> Result<String, &'static str> {
    let mut entries = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(|_| "build_script_rerun_path_unreadable")? {
            let path = entry.map_err(|_| "build_script_rerun_path_unreadable")?.path();
            if entries.len() == MAX_DIRECTORY_ENTRIES {
                return Err("build_script_rerun_directory_too_large");
            }
            let metadata = fs::symlink_metadata(&path).map_err(|_| "build_script_rerun_path_unreadable")?;
            let relative = path
                .strip_prefix(root)
                .map(crate::utils::path_to_git_format)
                .map_err(|_| "build_script_rerun_path_unreadable")?;
            let content = if metadata.file_type().is_symlink() {
                let target = fs::read_link(&path).map_err(|_| "build_script_rerun_path_unreadable")?;
                format!("symlink:{}", crate::utils::path_to_git_format(&target))
            } else if metadata.is_dir() {
                pending.push(path);
                "directory".to_string()
            } else {
                let bytes = fs::read(&path).map_err(|_| "build_script_rerun_path_unreadable")?;
                format!("file:{}", ContentDigest::sha256(&bytes))
            };
            entries.push((relative, content));
        }
    }
    entries.sort();
    let mut hasher = Sha256::new();
    hasher.update(b"cargo-rail-build-script-rerun-directory-v1\0");
    for (relative, content) in &entries {
        for part in [relative.as_bytes(), content.as_bytes()] {
            hasher.update(&(part.len() as u64).to_le_bytes());
            hasher.update(part);
        }
    }
    Ok(format!(
        "sha256:{}",
        ContentDigest::from_sha256_bytes(hasher.finalize())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Fixture {
        root: tempfile::TempDir,
    }

    impl Fixture {
        fn new(output: &str) -> Self {
            let root = tempfile::tempdir().expect("root");
            fs::create_dir_all(root.path().join("pkg/src")).expect("package");
            fs::create_dir_all(root.path().join("build/pkg-1/out")).expect("out dir");
            fs::write(root.path().join("build/pkg-1/output"), output).expect("output");
            Self { root }
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.root.path().join(relative)
        }

        fn capture(&self, immutable_source: bool) -> Result<BuildScriptFreshness, &'static str> {
            let out_dir = self.path("build/pkg-1/out");
            let package_root = self.path("pkg");
            capture(
                &ExecutedBuildScript {
                    package: "pkg 0.1.0",
                    out_dir: &out_dir,
                    package_root: &package_root,
                    immutable_source,
                    rustc_environment: &[],
                },
                self.root.path(),
            )
        }
    }

    #[test]
    fn declared_files_and_directories_decide_freshness() {
        let fixture = Fixture::new(
            "noise\ncargo:rerun-if-changed=wrapper.h\ncargo::rerun-if-changed=proto\ncargo:rustc-cfg=fast\n",
        );
        fs::write(fixture.path("pkg/wrapper.h"), "int a;").unwrap();
        fs::create_dir_all(fixture.path("pkg/proto/nested")).unwrap();
        fs::write(fixture.path("pkg/proto/nested/a.proto"), "message A {}").unwrap();
        let freshness = fixture.capture(false).expect("fresh");
        assert_eq!(freshness.paths.len(), 2);
        assert_eq!(freshness.revalidation_reason(fixture.root.path()), None);

        fs::write(fixture.path("pkg/proto/nested/a.proto"), "message B {}").unwrap();
        assert_eq!(
            freshness.revalidation_reason(fixture.root.path()),
            Some("build_script_input_changed")
        );
        fs::write(fixture.path("pkg/proto/nested/a.proto"), "message A {}").unwrap();
        fs::write(fixture.path("pkg/proto/b.proto"), "").unwrap();
        assert_eq!(
            freshness.revalidation_reason(fixture.root.path()),
            Some("build_script_input_changed"),
            "an added file changes a declared directory"
        );
        fs::remove_file(fixture.path("pkg/proto/b.proto")).unwrap();
        fs::write(fixture.path("pkg/wrapper.h"), "int b;").unwrap();
        assert_eq!(
            freshness.revalidation_reason(fixture.root.path()),
            Some("build_script_input_changed")
        );
    }

    #[test]
    fn immutable_sources_skip_package_paths_but_keep_host_paths_and_environment() {
        let host = tempfile::tempdir().unwrap();
        fs::write(host.path().join("lib.pc"), "Version: 1").unwrap();
        let fixture = Fixture::new(&format!(
            "cargo:rerun-if-changed=build.rs\ncargo:rerun-if-changed={}\ncargo:rerun-if-env-changed=D3_UNIT_UNSET_VARIABLE\n",
            host.path().join("lib.pc").display()
        ));
        let freshness = fixture.capture(true).expect("fresh");
        assert_eq!(freshness.paths.len(), 1, "build.rs is inside the immutable package");
        assert_eq!(freshness.environment.len(), 1);
        assert_eq!(freshness.environment[0].value_digest, None);
        assert_eq!(freshness.revalidation_reason(fixture.root.path()), None);
        fs::write(host.path().join("lib.pc"), "Version: 2").unwrap();
        assert_eq!(
            freshness.revalidation_reason(fixture.root.path()),
            Some("build_script_input_changed")
        );
    }

    #[test]
    fn unprovable_scripts_are_rejected_with_a_reason() {
        let missing = Fixture::new("cargo:rerun-if-changed=generated.h\n");
        assert_eq!(missing.capture(false), Err("build_script_rerun_path_missing"));
        let secret = Fixture::new("cargo:rerun-if-env-changed=SERVICE_API_TOKEN\n");
        assert_eq!(secret.capture(false), Err("build_script_secret_environment"));
        let unreadable = Fixture::new("");
        fs::remove_file(unreadable.path("build/pkg-1/output")).unwrap();
        assert_eq!(unreadable.capture(false), Err("build_script_output_unavailable"));
    }

    #[test]
    fn no_declared_path_leaves_freshness_to_the_package_sources() {
        let fixture = Fixture::new("cargo:rustc-cfg=fast\n");
        let freshness = fixture.capture(false).expect("fresh");
        assert!(freshness.paths.is_empty() && freshness.environment.is_empty());
        assert_eq!(freshness.revalidation_reason(fixture.root.path()), None);
    }
}
