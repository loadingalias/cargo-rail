//! Workspace context and state management
//!
//! This module unifies optional Git, Cargo, graph, configuration, and one
//! explicitly requested immutable snapshot in a single `WorkspaceContext`.
//!
//! # Architecture
//!
//! WorkspaceContext = optional GitState + CargoState + DependencyGraph + Config + optional WorkspaceSnapshot
//!
//! Built once at startup, passed by reference to all commands.

use std::path::{Path, PathBuf};

mod config_preflight;
pub(crate) use config_preflight::discovery_root;
/// Unified workspace context (includes optional GitState and CargoState)
pub mod context;
/// Immutable authoritative workspace capture.
pub mod snapshot;

// Re-export workspace types from context module
pub use context::{CargoState, GitState, WorkspaceContext};
pub use snapshot::{
    LockedPackageIdentity, LockfileSnapshot, SnapshotFile, SnapshotId, SnapshotPackage, WorkspaceSnapshot,
};

/// Return the workspace-owned root for generated cargo-rail state.
pub(crate) fn cargo_rail_state_root(workspace_root: &Path) -> PathBuf {
    workspace_root.join("target").join("cargo-rail")
}

pub(crate) fn uses_user_storage() -> bool {
    std::env::var_os("CARGO_RAIL_WORKSPACE_STORAGE").as_deref() == Some(std::ffi::OsStr::new("user"))
}

#[derive(Clone, Copy)]
pub(crate) enum WorkspaceStorage {
    Cache,
    State,
}

/// Resolve an owned directory beneath the selected storage base. In user
/// storage mode, only the platform base may be an alias.
pub(crate) fn workspace_storage_dir(
    workspace_root: &Path,
    storage: WorkspaceStorage,
    child: Option<&str>,
    create: bool,
) -> crate::error::RailResult<PathBuf> {
    resolve_storage_dir(workspace_root, storage, child, create, uses_user_storage())
}

fn resolve_storage_dir(
    workspace_root: &Path,
    storage: WorkspaceStorage,
    child: Option<&str>,
    create: bool,
    user_storage: bool,
) -> crate::error::RailResult<PathBuf> {
    let workspace_root = crate::utils::canonicalize_existing(workspace_root)?;
    let (base, relative) = if user_storage {
        let (base, kind, subdirectory) = match storage {
            WorkspaceStorage::Cache => (dirs::cache_dir(), "cache", cfg!(windows).then_some("cache")),
            WorkspaceStorage::State if cfg!(any(target_os = "macos", windows)) => {
                (dirs::data_local_dir(), "state", Some("state"))
            }
            WorkspaceStorage::State => (dirs::state_dir(), "state", None),
        };
        let base = base
            .filter(|path| path.is_absolute())
            .ok_or_else(|| crate::error::RailError::message(format!("cannot determine the user's {kind} directory")))?;
        let base = crate::utils::canonicalize_allow_missing(&base)?;
        let mut relative = PathBuf::from("cargo-rail");
        if let Some(subdirectory) = subdirectory {
            relative.push(subdirectory);
        }
        let workspace_id = crate::mutation::git_effect::physical_directory_identity(&workspace_root)?;
        (base, relative.join("workspaces").join(workspace_id))
    } else {
        (workspace_root, PathBuf::from("target").join("cargo-rail"))
    };
    resolve_storage_directory(base, &relative, child, create, user_storage)
}

pub(crate) fn resolve_storage_directory(
    mut directory: PathBuf,
    relative: &Path,
    child: Option<&str>,
    create: bool,
    private: bool,
) -> crate::error::RailResult<PathBuf> {
    #[cfg(not(unix))]
    let _ = private;
    if create {
        std::fs::create_dir_all(&directory)?;
    }
    for name in relative.iter().chain(child.map(std::ffi::OsStr::new)) {
        directory.push(name);
        if create {
            #[cfg(not(unix))]
            let builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            if private {
                use std::os::unix::fs::DirBuilderExt as _;
                builder.mode(0o700);
            }
            match builder.create(&directory) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        match std::fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.is_dir() && !crate::utils::is_symlink_or_reparse(&metadata) =>
            {
                #[cfg(unix)]
                if private {
                    use std::os::unix::fs::MetadataExt as _;
                    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o022 != 0 {
                        return Err(crate::error::RailError::message(format!(
                            "workspace storage directory '{}' must be owned by the current user and writable only by its owner",
                            directory.display()
                        )));
                    }
                }
            }
            Ok(_) => {
                return Err(crate::error::RailError::message(format!(
                    "workspace storage directory '{}' is not a real directory",
                    directory.display()
                )));
            }
            Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    if crate::utils::canonicalize_allow_missing(&directory)? != directory {
        return Err(crate::error::RailError::message(
            "workspace storage directory changed during resolution",
        ));
    }
    Ok(directory)
}

pub(crate) fn report_dir(workspace_root: &Path) -> crate::error::RailResult<PathBuf> {
    if uses_user_storage() {
        resolve_storage_directory(
            crate::utils::canonicalize_existing(workspace_root)?,
            Path::new(".cargo-rail"),
            None,
            false,
            false,
        )
    } else {
        Ok(cargo_rail_state_root(workspace_root))
    }
}

/// An old path is eligible for recovery only if its legacy hierarchy is real.
pub(crate) fn legacy_storage_child(workspace_root: &Path, child: &str) -> Option<PathBuf> {
    resolve_storage_dir(workspace_root, WorkspaceStorage::State, Some(child), false, false).ok()
}

pub(crate) fn receipt_dir(workspace_root: &Path, create: bool) -> crate::error::RailResult<PathBuf> {
    workspace_storage_dir(workspace_root, WorkspaceStorage::State, Some("receipts"), create)
}

pub(crate) fn receipt_read_dirs(workspace_root: &Path) -> crate::error::RailResult<Vec<PathBuf>> {
    let mut roots = vec![receipt_dir(workspace_root, false)?];
    if uses_user_storage()
        && let Some(legacy) = legacy_storage_child(workspace_root, "receipts")
    {
        roots.push(legacy);
    }
    Ok(roots)
}

/// Resolve Cargo-owned filesystem paths once before structural consumers receive metadata.
/// Package IDs and compiler invocation spellings remain owned by Cargo.
pub(crate) fn capture_metadata_paths(
    mut metadata: cargo_metadata::Metadata,
) -> crate::error::RailResult<cargo_metadata::Metadata> {
    let mut paths = std::collections::HashMap::<PathBuf, String>::new();
    let mut resolve = |path: &Path| -> crate::error::RailResult<String> {
        if let Some(resolved) = paths.get(path) {
            return Ok(resolved.clone());
        }
        let resolved = crate::utils::canonicalize_allow_missing(path)?;
        let resolved = resolved.into_os_string().into_string().map_err(|_| {
            crate::error::RailError::message(format!("resolved Cargo path '{}' is not UTF-8", path.display()))
        })?;
        paths.insert(path.to_path_buf(), resolved.clone());
        Ok(resolved)
    };
    metadata.workspace_root = resolve(metadata.workspace_root.as_std_path())?.into();
    metadata.target_directory = resolve(metadata.target_directory.as_std_path())?.into();
    if let Some(path) = &mut metadata.build_directory {
        *path = resolve(path.as_std_path())?.into();
    }
    for package in &mut metadata.packages {
        package.manifest_path = resolve(package.manifest_path.as_std_path())?.into();
        for target in &mut package.targets {
            target.src_path = resolve(target.src_path.as_std_path())?.into();
        }
        for dependency in &mut package.dependencies {
            if let Some(path) = &mut dependency.path {
                *path = resolve(path.as_std_path())?.into();
            }
        }
    }
    Ok(metadata)
}
