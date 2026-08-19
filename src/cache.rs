//! Exact ownership, measurement, and reclamation for cargo-rail cache state.

pub(crate) mod cas;
pub(crate) mod installation;
pub(crate) mod result;

use crate::error::{RailError, RailResult};
use serde::Serialize;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

const STATUS_SCAN_MAX_ENTRIES: usize = 1_000_000;
const WORKSPACE_LOCK_BYTES: u64 = 0;

/// Exclusive authority over cache-owned state inside one workspace.
pub(crate) struct WorkspaceCacheLock {
  _file: File,
}

/// Serialize workspace cache mutation without serializing the shared local CAS.
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

/// Read-only measurements for the shared local cache scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SharedCacheStatus {
  pub(crate) present: bool,
  pub(crate) cross_workspace: bool,
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
  let remote = if local {
    crate::remote_cache::configuration_status(workspace_root)
      .map_err(|error| RailError::message(format!("remote cache configuration is unavailable: {error}")))?
  } else {
    None
  };
  Ok(CacheStatus {
    schema_version: 11,
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
          cross_workspace: true,
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
  let root = crate::workspace::cargo_rail_state_root(workspace_root);
  let metadata = root.join("metadata.json");
  let legacy = root.join("cache");

  // Validate and measure the complete owned scope before deleting any part of it.
  // The lifecycle lock keeps current cargo-rail processes from changing the view.
  let status = workspace_status(workspace_root)?;
  let bytes = status
    .artifacts
    .iter()
    .filter(|artifact| artifact.kind != "workspace_cache_lock")
    .try_fold(0u64, |total, artifact| {
      total
        .checked_add(artifact.bytes)
        .ok_or_else(|| RailError::message("workspace cache cleanup byte count overflow"))
    })?;
  let mut paths = Vec::new();

  if remove_owned_file(&metadata)? {
    paths.push(metadata.to_string_lossy().into_owned());
  }
  if remove_owned_tree(&legacy)? {
    paths.push(legacy.to_string_lossy().into_owned());
  }
  Ok(CacheRemoval { paths, bytes })
}

/// Remove the validated shared local CAS in the selected local cache domain.
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
  let root = crate::workspace::cargo_rail_state_root(workspace_root);
  let candidates = [
    (
      "cargo_metadata",
      root.join("metadata.json"),
      Some(crate::workspace::context::METADATA_CACHE_MAX_BYTES),
    ),
    (
      "legacy_compiler_evidence",
      root.join("cache"),
      Some(crate::compiler::diagnostics_store::MAX_CACHE_BYTES as u64),
    ),
    (
      "workspace_cache_lock",
      root.join("cache.lock"),
      Some(WORKSPACE_LOCK_BYTES),
    ),
  ];
  let mut artifacts = Vec::new();
  for (kind, path, max_bytes) in candidates {
    let Some((bytes, files, directories)) = path_status(&path)? else {
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
    root: root.to_string_lossy().into_owned(),
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

fn remove_owned_file(path: &Path) -> RailResult<bool> {
  let metadata = match fs::symlink_metadata(path) {
    Ok(metadata) => metadata,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
    Err(error) => return Err(error.into()),
  };
  if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) {
    return Err(RailError::with_help(
      format!("workspace cache file '{}' is not a real file", path.display()),
      "remove the hostile path manually; cargo-rail will not reclaim linked cache state",
    ));
  }
  fs::remove_file(path)?;
  Ok(true)
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
    let cache = workspace.path().join("target/cargo-rail/cache");
    fs::create_dir_all(&cache).expect("workspace cache");
    symlink(outside.path(), cache.join("hostile-link")).expect("hostile nested link");

    assert!(remove_owned_tree(&cache).expect("owned cleanup"));

    assert!(!cache.exists());
    assert_eq!(fs::read(&sentinel).expect("outside sentinel"), b"outside");
  }

  #[test]
  fn workspace_cleanup_waits_for_an_active_cache_owner() {
    let workspace = tempfile::tempdir().expect("workspace");
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
      drop(owner);
      finished_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("cleanup should finish");
    });
  }
}
