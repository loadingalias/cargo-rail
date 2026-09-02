//! Exact ownership, measurement, and reclamation for cargo-rail cache state.

pub(crate) mod cas;
pub(crate) mod installation;
pub(crate) mod profile;
pub(crate) mod result;

use crate::error::{RailError, RailResult};
use serde::Serialize;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

const STATUS_SCAN_MAX_ENTRIES: usize = 1_000_000;
const WORKSPACE_LOCK_BYTES: u64 = 0;
const V025_COMPILER_DIAGNOSTICS_MAX_BYTES: u64 = 256 * 1024 * 1024;
const V025_COMPILER_DIAGNOSTICS_DIRECTORY: &str = "cache";
const V025_COMPILER_DIAGNOSTICS_FILE: &str = "compiler-diags-v1.json";

struct WorkspaceCachePaths {
    state_root: PathBuf,
    predecessor_cache: PathBuf,
    predecessor_compiler_diagnostics: PathBuf,
    compiler_artifacts: PathBuf,
    lock: PathBuf,
}

/// Exclusive authority over cache-owned state inside one workspace.
pub(crate) struct WorkspaceCacheLock {
    _file: File,
}

/// Serialize workspace cache mutation without serializing the selected profile's local CAS.
pub(crate) fn lock_workspace(workspace_root: &Path) -> RailResult<WorkspaceCacheLock> {
    let workspace_root = crate::utils::canonicalize_existing(workspace_root)?;
    let mut directory = workspace_root.clone();
    for component in ["target", "cargo-rail"] {
        directory.push(component);
        match fs::create_dir(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let metadata = fs::symlink_metadata(&directory)?;
        if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
            return Err(RailError::message(format!(
                "workspace cache lock directory '{}' is not a real directory",
                directory.display()
            )));
        }
    }
    if crate::utils::canonicalize_existing(&directory)? != directory || !directory.starts_with(&workspace_root) {
        return Err(RailError::message(
            "workspace cache lock directory escaped the workspace",
        ));
    }

    let path = directory.join("cache.lock");
    let file = crate::utils::open_cache_lock_file(&path, true)?;
    if !crate::utils::private_file_matches_path(&file, &path, WORKSPACE_LOCK_BYTES)? {
        return Err(RailError::with_help(
            format!(
                "workspace cache lock '{}' is not a private regular file",
                path.display()
            ),
            "remove the hostile lock path; cargo-rail will not follow or share cache lock files",
        ));
    }
    file.lock()?;
    if !crate::utils::private_file_matches_path(&file, &path, WORKSPACE_LOCK_BYTES)? {
        return Err(RailError::message(format!(
            "workspace cache lock '{}' changed while it was acquired",
            path.display()
        )));
    }
    Ok(WorkspaceCacheLock { _file: file })
}
/// One cache-owned workspace artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct WorkspaceCacheArtifact {
    pub(crate) kind: &'static str,
    pub(crate) path: String,
    pub(crate) bytes: u64,
    pub(crate) files: u64,
    pub(crate) directories: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_bytes: Option<u64>,
}

/// Read-only measurements for reconstructible cache state in one workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct WorkspaceCacheStatus {
    pub(crate) root: String,
    pub(crate) bytes: u64,
    pub(crate) files: u64,
    pub(crate) directories: u64,
    pub(crate) fully_bounded: bool,
    pub(crate) artifacts: Vec<WorkspaceCacheArtifact>,
}

/// Read-only measurements for the selected profile's local cache scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SharedCacheStatus {
    pub(crate) present: bool,
    pub(crate) profile_scoped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cache: Option<crate::cache::cas::LocalCasStatus>,
}

/// Versioned read-only cache status projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CacheStatus {
    pub(crate) schema_version: u32,
    pub(crate) installation: crate::cache::installation::InstallationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) workspace: Option<WorkspaceCacheStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) local: Option<SharedCacheStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) remote: Option<crate::remote_cache::RemoteCacheConfigurationStatus>,
}

/// Cache state removed by one explicitly authorized cleanup.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct CacheRemoval {
    pub(crate) paths: Vec<String>,
    pub(crate) bytes: u64,
}

impl CacheRemoval {
    pub(crate) fn extend(&mut self, other: Self) -> RailResult<()> {
        self.bytes = self
            .bytes
            .checked_add(other.bytes)
            .ok_or_else(|| RailError::message("cache cleanup byte count overflow"))?;
        self.paths.extend(other.paths);
        Ok(())
    }
}

/// Inspect selected cache scopes without creating or modifying cache state.
pub(crate) fn status(workspace_root: &Path, workspace: bool, local: bool) -> RailResult<CacheStatus> {
    let installation = crate::cache::installation::status(workspace_root)?;
    let transparent_installed = installation.wrapper_path.is_some();
    let profile_scoped = installation.profile_id.is_some();
    let remote = if local {
        crate::remote_cache::configuration_status(workspace_root)
            .map_err(|error| RailError::message(format!("remote cache configuration is unavailable: {error}")))?
    } else {
        None
    };
    Ok(CacheStatus {
        schema_version: 15,
        installation,
        workspace: workspace.then(|| workspace_status(workspace_root)).transpose()?,
        local: local
            .then(|| {
                let cache = if transparent_installed {
                    crate::cache::installation::local_cache_status(workspace_root)?
                } else {
                    let selection = crate::cache::cas::LocalCacheSelection::from_environment()?;
                    selection
                        .configured_root()?
                        .map(|root| crate::cache::cas::status_at_with_max(&root, selection.max_bytes()))
                        .transpose()?
                        .flatten()
                };
                Ok::<_, RailError>(SharedCacheStatus {
                    present: cache.is_some(),
                    profile_scoped,
                    cache,
                })
            })
            .transpose()?,
        remote,
    })
}

/// Remove reconstructible cache state inside one workspace.
pub(crate) fn remove_workspace(workspace_root: &Path) -> RailResult<CacheRemoval> {
    let _lock = lock_workspace(workspace_root)?;
    let paths = workspace_cache_paths(workspace_root)?;

    // Validate and measure the complete owned scope before deleting any part of it.
    // The lifecycle lock keeps current cargo-rail processes from changing the view.
    let status = workspace_status_for_paths(&paths)?;
    let bytes = status
        .artifacts
        .iter()
        .filter(|artifact| artifact.kind != "workspace_cache_lock")
        .try_fold(0u64, |total, artifact| {
            total
                .checked_add(artifact.bytes)
                .ok_or_else(|| RailError::message("workspace cache cleanup byte count overflow"))
        })?;
    let expected_predecessor_diagnostics = status
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "predecessor_compiler_diagnostics")
        .map(|artifact| artifact.bytes);
    let mut paths = Vec::new();

    let revalidated = workspace_cache_paths(workspace_root)?;
    if remove_owned_file(
        &revalidated.predecessor_compiler_diagnostics,
        "v0.25 compiler diagnostics file",
        expected_predecessor_diagnostics,
    )? {
        paths.push(
            revalidated
                .predecessor_compiler_diagnostics
                .to_string_lossy()
                .into_owned(),
        );
        let revalidated = workspace_cache_paths(workspace_root)?;
        remove_empty_owned_directory(&revalidated.predecessor_cache)?;
    }

    let revalidated = workspace_cache_paths(workspace_root)?;
    if remove_owned_tree(&revalidated.compiler_artifacts)? {
        paths.push(revalidated.compiler_artifacts.to_string_lossy().into_owned());
    }
    Ok(CacheRemoval { paths, bytes })
}

/// Remove the validated local CAS in the selected profile's cache domain.
pub(crate) fn remove_local(workspace_root: &Path) -> RailResult<CacheRemoval> {
    let removed = match crate::cache::installation::remove_local_cache(workspace_root)? {
        Some(removed) => removed,
        None => {
            let selection = crate::cache::cas::LocalCacheSelection::from_environment()?;
            selection
                .configured_root()?
                .map(|root| crate::cache::cas::remove_owned_root_at(&root))
                .transpose()?
                .flatten()
                .into_iter()
                .collect()
        }
    };
    removed
        .into_iter()
        .try_fold(CacheRemoval::default(), |mut removal, (path, bytes)| {
            removal.bytes = removal
                .bytes
                .checked_add(bytes)
                .ok_or_else(|| RailError::message("cache cleanup byte count overflow"))?;
            removal.paths.push(path.to_string_lossy().into_owned());
            Ok(removal)
        })
}

fn workspace_status(workspace_root: &Path) -> RailResult<WorkspaceCacheStatus> {
    let paths = workspace_cache_paths(workspace_root)?;
    workspace_status_for_paths(&paths)
}

fn workspace_cache_paths(workspace_root: &Path) -> RailResult<WorkspaceCachePaths> {
    let workspace_root = crate::utils::canonicalize_existing(workspace_root)?;
    let metadata = fs::symlink_metadata(&workspace_root)?;
    if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::message(format!(
            "workspace cache root '{}' is not a real directory",
            workspace_root.display()
        )));
    }

    let target = workspace_root.join("target");
    let state_root = target.join("cargo-rail");
    let predecessor_cache = state_root.join(V025_COMPILER_DIAGNOSTICS_DIRECTORY);
    for (path, description) in [
        (&target, "workspace target directory"),
        (&state_root, "workspace cache state directory"),
        (&predecessor_cache, "v0.25 compiler diagnostics directory"),
    ] {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
            return Err(RailError::with_help(
                format!("{description} '{}' is not a real directory", path.display()),
                "remove the hostile path manually; cargo-rail will not inspect or reclaim linked cache state",
            ));
        }
        let canonical = crate::utils::canonicalize_existing(path)?;
        if canonical.as_path() != path.as_path() || !canonical.starts_with(&workspace_root) {
            return Err(RailError::message(format!(
                "{description} '{}' escaped the workspace",
                path.display()
            )));
        }
    }

    Ok(WorkspaceCachePaths {
        predecessor_compiler_diagnostics: predecessor_cache.join(V025_COMPILER_DIAGNOSTICS_FILE),
        compiler_artifacts: state_root.join("compiler-artifacts-v1"),
        lock: state_root.join("cache.lock"),
        state_root,
        predecessor_cache,
    })
}

fn private_file_status(path: &Path, description: &str) -> RailResult<Option<u64>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::with_help(
            format!("{description} '{}' is not a private regular file", path.display()),
            "remove the hostile path manually; cargo-rail will not inspect or reclaim linked cache state",
        ));
    }
    let opened = File::open(path)?;
    if !crate::utils::private_file_matches_path(&opened, path, metadata.len())? {
        return Err(RailError::with_help(
            format!("{description} '{}' is not a private regular file", path.display()),
            "remove the hostile path manually; cargo-rail will not inspect or reclaim linked cache state",
        ));
    }
    Ok(Some(metadata.len()))
}

fn workspace_status_for_paths(paths: &WorkspaceCachePaths) -> RailResult<WorkspaceCacheStatus> {
    let mut artifacts = Vec::new();
    if let Some(bytes) = private_file_status(
        &paths.predecessor_compiler_diagnostics,
        "v0.25 compiler diagnostics file",
    )? {
        artifacts.push(WorkspaceCacheArtifact {
            kind: "predecessor_compiler_diagnostics",
            path: paths.predecessor_compiler_diagnostics.to_string_lossy().into_owned(),
            bytes,
            files: 1,
            directories: 0,
            max_bytes: Some(V025_COMPILER_DIAGNOSTICS_MAX_BYTES),
        });
    }
    for (kind, path, max_bytes) in [
        ("compiler_artifacts", paths.compiler_artifacts.as_path(), None),
        ("workspace_cache_lock", paths.lock.as_path(), Some(WORKSPACE_LOCK_BYTES)),
    ] {
        let Some((bytes, files, directories)) = path_status(path)? else {
            continue;
        };
        artifacts.push(WorkspaceCacheArtifact {
            kind,
            path: path.to_string_lossy().into_owned(),
            bytes,
            files,
            directories,
            max_bytes,
        });
    }
    let bytes = artifacts.iter().try_fold(0u64, |total, artifact| {
        total
            .checked_add(artifact.bytes)
            .ok_or_else(|| RailError::message("workspace cache size overflow"))
    })?;
    let files = artifacts.iter().map(|artifact| artifact.files).sum();
    let directories = artifacts.iter().map(|artifact| artifact.directories).sum();
    let fully_bounded = artifacts
        .iter()
        .all(|artifact| artifact.max_bytes.is_some_and(|bound| artifact.bytes <= bound));
    Ok(WorkspaceCacheStatus {
        root: paths.state_root.to_string_lossy().into_owned(),
        bytes,
        files,
        directories,
        fully_bounded,
        artifacts,
    })
}

pub(crate) fn path_status(root: &Path) -> RailResult<Option<(u64, u64, u64)>> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if crate::utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::with_help(
            format!("cache status refused linked path '{}'", root.display()),
            "remove the hostile path; cargo-rail will not follow cache links",
        ));
    }
    if metadata.is_file() {
        return Ok(Some((metadata.len(), 1, 0)));
    }
    if !metadata.is_dir() {
        return Err(RailError::message(format!(
            "cache status found unsupported path '{}'",
            root.display()
        )));
    }

    let mut bytes = 0u64;
    let mut files = 0u64;
    let mut directories = 0u64;
    let mut visited = 0usize;
    let mut pending = vec![PathBuf::from(root)];
    while let Some(path) = pending.pop() {
        visited = visited.saturating_add(1);
        if visited > STATUS_SCAN_MAX_ENTRIES {
            return Err(RailError::message(format!(
                "cache status for '{}' exceeds its {STATUS_SCAN_MAX_ENTRIES}-entry scan bound",
                root.display()
            )));
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() && !crate::utils::is_symlink_or_reparse(&metadata) {
            directories = directories.saturating_add(1);
            let mut entries = fs::read_dir(&path)?.collect::<Result<Vec<_>, _>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            pending.extend(entries.into_iter().map(|entry| entry.path()));
        } else if metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) {
            files = files.saturating_add(1);
            bytes = bytes
                .checked_add(metadata.len())
                .ok_or_else(|| RailError::message("workspace cache size overflow"))?;
        } else {
            return Err(RailError::message(format!(
                "cache status found unsupported path '{}'",
                path.display()
            )));
        }
    }
    Ok(Some((bytes, files, directories)))
}

fn remove_owned_file(path: &Path, description: &str, expected_bytes: Option<u64>) -> RailResult<bool> {
    let observed_bytes = private_file_status(path, description)?;
    if observed_bytes != expected_bytes {
        return Err(RailError::message(format!(
            "{description} '{}' changed while workspace cache cleanup was planned",
            path.display()
        )));
    }
    if observed_bytes.is_none() {
        return Ok(false);
    }
    fs::remove_file(path)?;
    Ok(true)
}

fn remove_empty_owned_directory(path: &Path) -> RailResult<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::with_help(
            format!("workspace cache directory '{}' is not a real directory", path.display()),
            "remove the hostile path manually; cargo-rail will not reclaim linked cache state",
        ));
    }
    match fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn remove_owned_tree(root: &Path) -> RailResult<bool> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::with_help(
            format!("workspace cache root '{}' is not a real directory", root.display()),
            "remove the hostile path manually; cargo-rail will not reclaim linked cache state",
        ));
    }

    // Delegate non-following traversal to the platform-specific standard
    // library implementation after validating the owned root itself.
    fs::remove_dir_all(root)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn workspace_lock_rejects_links_without_touching_the_target() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::NamedTempFile::new().expect("outside lock target");
        fs::create_dir_all(workspace.path().join("target/cargo-rail")).expect("cache state root");
        let lock = workspace.path().join("target/cargo-rail/cache.lock");
        symlink(outside.path(), &lock).expect("hostile lock link");

        let error = lock_workspace(workspace.path()).err().expect("linked lock must fail");

        assert!(error.to_string().contains("not a private regular file"), "{error}");
        assert_eq!(fs::metadata(outside.path()).expect("outside metadata").len(), 0);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn workspace_lock_rejects_hard_links_without_touching_the_target() {
        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::NamedTempFile::new().expect("outside lock target");
        fs::create_dir_all(workspace.path().join("target/cargo-rail")).expect("cache state root");
        let lock = workspace.path().join("target/cargo-rail/cache.lock");
        fs::hard_link(outside.path(), &lock).expect("hostile hard-linked lock");

        let error = lock_workspace(workspace.path())
            .err()
            .expect("hard-linked lock must fail");

        assert!(error.to_string().contains("not a private regular file"), "{error}");
        assert!(outside.path().exists(), "outside lock target must survive");
    }

    #[cfg(unix)]
    #[test]
    fn status_counts_nested_links_without_following_them() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("status root");
        let outside = tempfile::tempdir().expect("outside root");
        fs::write(outside.path().join("large"), [0; 1024]).expect("outside payload");
        symlink(outside.path(), root.path().join("link")).expect("nested link");

        let (bytes, files, directories) = path_status(root.path()).expect("bounded status").expect("present root");

        assert!(bytes < 1024, "status followed the nested link");
        assert_eq!(files, 1);
        assert_eq!(directories, 1);
    }

    #[cfg(unix)]
    #[test]
    fn workspace_cleanup_unlinks_nested_links_without_following_them() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside root");
        let sentinel = outside.path().join("keep");
        fs::write(&sentinel, b"outside").expect("outside sentinel");
        let artifacts = workspace.path().join("target/cargo-rail/compiler-artifacts-v1");
        fs::create_dir_all(&artifacts).expect("workspace cache");
        symlink(outside.path(), artifacts.join("hostile-link")).expect("hostile nested link");

        assert!(remove_owned_tree(&artifacts).expect("owned cleanup"));

        assert!(!artifacts.exists());
        assert_eq!(fs::read(&sentinel).expect("outside sentinel"), b"outside");
    }

    #[test]
    fn workspace_cleanup_waits_for_an_active_cache_owner() {
        let workspace = tempfile::tempdir().expect("workspace");
        let diagnostics = workspace.path().join("target/cargo-rail/cache/compiler-diags-v1.json");
        let expected_removed = crate::utils::canonicalize_existing(workspace.path())
            .expect("canonical workspace")
            .join("target/cargo-rail/cache/compiler-diags-v1.json")
            .to_string_lossy()
            .into_owned();
        fs::create_dir_all(diagnostics.parent().expect("diagnostics parent")).expect("diagnostics parent");
        fs::write(&diagnostics, b"{\"version\":10,\"entries\":{}}").expect("v0.25 compiler diagnostics");
        let owner = lock_workspace(workspace.path()).expect("workspace cache owner");
        let workspace_root = workspace.path().to_path_buf();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();

        std::thread::scope(|scope| {
            scope.spawn(move || {
                let removed = remove_workspace(&workspace_root).expect("cleanup should wait, then finish");
                finished_tx.send(removed).expect("cleanup result");
            });
            assert!(
                finished_rx.recv_timeout(std::time::Duration::from_millis(100)).is_err(),
                "cleanup crossed the workspace cache lifecycle boundary"
            );
            assert!(diagnostics.is_file(), "blocked cleanup must not remove diagnostics");
            drop(owner);
            let removed = finished_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("cleanup should finish");
            assert!(!diagnostics.exists());
            assert_eq!(removed.paths, vec![expected_removed]);
        });
    }
}
