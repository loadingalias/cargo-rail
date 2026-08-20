//! Repository-boundary capabilities for split and sync mutations.

use crate::error::{GitError, RailError, RailResult};
use crate::git::SystemGit;
use crate::utils;
use std::fs;
use std::path::{Path, PathBuf};

/// Validated filesystem roots owned by one split target.
///
/// Construction proves that source crates are real directories inside the source
/// workspace and that the target cannot overlap the source Git worktree.
#[derive(Debug, Clone)]
pub struct SplitPathCapabilities {
    source_workspace: PathBuf,
    source_worktree: PathBuf,
    target_root: PathBuf,
    temporary_root: PathBuf,
    crate_roots: Vec<PathBuf>,
    asset_paths: Vec<PathBuf>,
}

impl SplitPathCapabilities {
    /// Validate and resolve all roots before any split mutation begins.
    pub fn new(
        workspace_root: &Path,
        worktree_root: &Path,
        crate_paths: &[PathBuf],
        target_root: &Path,
    ) -> RailResult<Self> {
        let source_workspace = canonical_existing(workspace_root, "source workspace")?;
        let source_worktree = canonical_existing(worktree_root, "source worktree")?;
        if !source_workspace.starts_with(&source_worktree) {
            return Err(boundary_error(
                "source workspace",
                &source_workspace,
                "is outside its Git worktree",
            ));
        }

        let mut crate_roots = Vec::with_capacity(crate_paths.len());
        for crate_path in crate_paths {
            let joined = if crate_path.is_absolute() {
                crate_path.clone()
            } else {
                source_workspace.join(crate_path)
            };
            let resolved = canonical_existing(&joined, "split crate path")?;
            if !resolved.starts_with(&source_workspace) {
                return Err(boundary_error(
                    "split crate path",
                    &resolved,
                    "escapes the source workspace (including through a symlink)",
                ));
            }
            if !resolved.is_dir() {
                return Err(boundary_error("split crate path", &resolved, "is not a directory"));
            }
            crate_roots.push(resolved);
        }

        let target_root = resolve_allow_missing(target_root, "split target")?;
        reject_overlap("split target", &target_root, "source worktree", &source_worktree)?;

        let temporary_root = resolve_allow_missing(
            &std::env::temp_dir().join("cargo-rail").join("split"),
            "split temporary root",
        )?;
        reject_overlap(
            "split temporary root",
            &temporary_root,
            "source worktree",
            &source_worktree,
        )?;
        reject_overlap("split temporary root", &temporary_root, "split target", &target_root)?;

        Ok(Self {
            source_workspace,
            source_worktree,
            target_root,
            temporary_root,
            crate_roots,
            asset_paths: Vec::new(),
        })
    }

    /// Add the exact non-Cargo asset paths owned by this split.
    pub fn with_asset_paths(mut self, asset_paths: &[PathBuf]) -> RailResult<Self> {
        let mut resolved = Vec::with_capacity(asset_paths.len());
        for path in asset_paths {
            let path = self.logical_source_mutation_path(path)?;
            if self.crate_roots.iter().any(|root| path.starts_with(root)) {
                return Err(boundary_error(
                    "split asset path",
                    &path,
                    "overlaps a Cargo-owned crate root",
                ));
            }
            resolved.push(path);
        }
        resolved.sort();
        resolved.dedup();
        self.asset_paths = resolved;
        Ok(self)
    }

    /// Canonical source workspace root.
    pub fn source_workspace(&self) -> &Path {
        &self.source_workspace
    }

    /// Canonical split target root, including a normalized non-existent suffix.
    pub fn target_root(&self) -> &Path {
        &self.target_root
    }

    /// Validate a target path immediately before mutation.
    ///
    /// Existing symlinks are resolved on every call so a destination cannot redirect
    /// a write outside the target after initial configuration validation.
    pub fn authorize_target(&self, path: &Path) -> RailResult<PathBuf> {
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.target_root.join(path)
        };
        let resolved = resolve_allow_missing(&candidate, "split mutation path")?;
        if !resolved.starts_with(&self.target_root) {
            return Err(boundary_error(
                "split mutation path",
                &resolved,
                "escapes the authorized target root (including through a symlink)",
            ));
        }
        Ok(resolved)
    }

    /// Validate a source path immediately before reading or copying it.
    pub fn authorize_source(&self, path: &Path) -> RailResult<PathBuf> {
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.source_workspace.join(path)
        };
        let resolved = canonical_existing(&candidate, "split source path")?;
        if !resolved.starts_with(&self.source_workspace) {
            return Err(boundary_error(
                "split source path",
                &resolved,
                "escapes the authorized source workspace (including through a symlink)",
            ));
        }
        Ok(resolved)
    }

    /// Validate a possibly new source-workspace path immediately before mutation.
    pub fn authorize_source_mutation(&self, path: &Path) -> RailResult<PathBuf> {
        let resolved = self.logical_source_mutation_path(path)?;
        if self.crate_roots.iter().any(|root| resolved.starts_with(root)) || self.asset_paths.contains(&resolved) {
            return Ok(resolved);
        }
        Err(boundary_error(
            "sync source mutation path",
            &resolved,
            "is outside the Cargo roots and explicit non-Cargo assets owned by this split",
        ))
    }

    fn logical_source_mutation_path(&self, path: &Path) -> RailResult<PathBuf> {
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.source_workspace.join(path)
        };
        let file_name = candidate
            .file_name()
            .ok_or_else(|| boundary_error("sync source mutation path", &candidate, "does not identify a file"))?;
        let parent = candidate
            .parent()
            .ok_or_else(|| boundary_error("sync source mutation path", &candidate, "has no parent directory"))?;
        let mut resolved = resolve_allow_missing(parent, "sync source mutation parent")?;
        resolved.push(file_name);
        if !resolved.starts_with(&self.source_workspace) {
            return Err(boundary_error(
                "sync source mutation path",
                &resolved,
                "escapes the authorized source workspace (including through a parent symlink)",
            ));
        }
        Ok(resolved)
    }

    /// Validate a temporary path before conflict-resolution writes.
    pub fn authorize_temporary(&self, path: &Path) -> RailResult<PathBuf> {
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.temporary_root.join(path)
        };
        let resolved = resolve_allow_missing(&candidate, "split temporary path")?;
        if !resolved.starts_with(&self.temporary_root) {
            return Err(boundary_error(
                "split temporary path",
                &resolved,
                "escapes the authorized temporary root (including through a symlink)",
            ));
        }
        Ok(resolved)
    }

    /// Revalidate repository identity before a Git operation mutates the target.
    pub fn validate_target_repository(&self) -> RailResult<()> {
        let target = resolve_allow_missing(&self.target_root, "split target")?;
        reject_overlap("split target", &target, "source worktree", &self.source_worktree)?;
        let repository_probe = nearest_existing_ancestor(&target)?;
        match SystemGit::open(repository_probe) {
            Ok(target_git) => {
                let target_worktree = canonical_existing(&target_git.worktree_root, "target worktree")?;
                if target_worktree != self.target_root {
                    return Err(boundary_error(
                        "split target",
                        &target,
                        &format!(
                            "is nested inside unrelated Git worktree '{}'",
                            target_worktree.display()
                        ),
                    ));
                }
            }
            Err(RailError::Git(GitError::RepoNotFound { .. })) => {}
            Err(error) => return Err(error),
        }
        Ok(())
    }

    /// Prove runtime parameters still match the crate roots authorized at construction.
    pub fn validate_crate_paths(&self, crate_paths: &[PathBuf]) -> RailResult<()> {
        let mut current = Vec::with_capacity(crate_paths.len());
        for crate_path in crate_paths {
            current.push(self.authorize_source(crate_path)?);
        }
        if current != self.crate_roots {
            return Err(RailError::with_help(
                "runtime split crate paths do not match the validated path capability",
                "rebuild split parameters from the current repository configuration",
            ));
        }
        Ok(())
    }

    /// Canonical temporary root reserved for this mutation class.
    pub fn temporary_root(&self) -> &Path {
        &self.temporary_root
    }
}

fn canonical_existing(path: &Path, label: &str) -> RailResult<PathBuf> {
    utils::canonicalize_existing(path).map_err(|error| {
        RailError::with_help(
            format!("invalid {} '{}': {}", label, path.display(), error),
            "use an existing path that stays within the configured repository boundary",
        )
    })
}

fn resolve_allow_missing(path: &Path, label: &str) -> RailResult<PathBuf> {
    utils::canonicalize_allow_missing(path).map_err(|error| {
        RailError::with_help(
            format!("invalid {} '{}': {}", label, path.display(), error),
            "use a path that stays within the configured repository boundary",
        )
    })
}

fn nearest_existing_ancestor(path: &Path) -> RailResult<&Path> {
    let mut current = path;
    loop {
        match fs::symlink_metadata(current) {
            Ok(_) => return Ok(current),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                current = current
                    .parent()
                    .ok_or_else(|| boundary_error("split target", path, "has no existing ancestor"))?;
            }
            Err(error) => {
                return Err(boundary_error(
                    "split target",
                    path,
                    &format!("cannot be inspected: {}", error),
                ));
            }
        }
    }
}

fn reject_overlap(left_label: &str, left: &Path, right_label: &str, right: &Path) -> RailResult<()> {
    if left.starts_with(right) || right.starts_with(left) {
        return Err(RailError::with_help(
            format!(
                "{} '{}' overlaps {} '{}'",
                left_label,
                left.display(),
                right_label,
                right.display()
            ),
            "choose a split target outside the source repository and its ancestors",
        ));
    }
    Ok(())
}

fn boundary_error(label: &str, path: &Path, reason: &str) -> RailError {
    RailError::with_help(
        format!("{} '{}' {}", label, path.display(), reason),
        "fix the split paths before retrying; cargo-rail did not mutate either repository",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn roots() -> (TempDir, PathBuf, PathBuf) {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let crate_root = source.join("crates/demo");
        fs::create_dir_all(&crate_root).unwrap();
        (temp, source, crate_root)
    }

    #[test]
    fn rejects_target_identity_and_ancestor_collisions() {
        let (_temp, source, _crate_root) = roots();
        let crate_paths = vec![PathBuf::from("crates/demo")];
        SplitPathCapabilities::new(&source, &source, &crate_paths, &source).unwrap_err();
        SplitPathCapabilities::new(&source, &source, &crate_paths, source.parent().unwrap()).unwrap_err();
    }

    #[test]
    fn rejects_crate_path_outside_workspace() {
        let (temp, source, _crate_root) = roots();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let paths = vec![PathBuf::from("../outside")];
        SplitPathCapabilities::new(&source, &source, &paths, &temp.path().join("target")).unwrap_err();
    }

    #[test]
    fn rejects_target_nested_in_another_repository() {
        let (temp, source, _crate_root) = roots();
        let other_repo = temp.path().join("other-repo");
        fs::create_dir(&other_repo).unwrap();
        crate::git::init_repo(&other_repo, "main").unwrap();
        let target = other_repo.join("nested-target");
        let capabilities =
            SplitPathCapabilities::new(&source, &source, &[PathBuf::from("crates/demo")], &target).unwrap();

        assert!(capabilities.validate_target_repository().is_err());
    }

    #[test]
    fn source_mutation_is_limited_to_crate_roots_and_exact_assets() {
        let (temp, source, _crate_root) = roots();
        let sibling = source.join("crates/sibling");
        let asset = source.join("NOTICE");
        fs::create_dir_all(&sibling).unwrap();
        fs::write(&asset, "notice").unwrap();
        let target = temp.path().join("target");
        let capabilities = SplitPathCapabilities::new(&source, &source, &[PathBuf::from("crates/demo")], &target)
            .unwrap()
            .with_asset_paths(&[PathBuf::from("NOTICE")])
            .unwrap();

        capabilities
            .authorize_source_mutation(Path::new("crates/demo/src/lib.rs"))
            .unwrap();
        capabilities.authorize_source_mutation(Path::new("NOTICE")).unwrap();
        capabilities
            .authorize_source_mutation(Path::new("crates/sibling/src/lib.rs"))
            .unwrap_err();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_source_and_target_symlink_escapes() {
        use std::os::unix::fs::symlink;

        let (temp, source, _crate_root) = roots();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, source.join("crates/escape")).unwrap();
        let escaped_crate = vec![PathBuf::from("crates/escape")];
        SplitPathCapabilities::new(&source, &source, &escaped_crate, &temp.path().join("target")).unwrap_err();

        let target_link = temp.path().join("target-link");
        symlink(&source, &target_link).unwrap();
        let valid_crate = vec![PathBuf::from("crates/demo")];
        SplitPathCapabilities::new(&source, &source, &valid_crate, &target_link).unwrap_err();
    }

    #[cfg(unix)]
    #[test]
    fn revalidates_destination_symlinks_before_write() {
        use std::os::unix::fs::symlink;

        let (temp, source, _crate_root) = roots();
        let target = temp.path().join("target");
        let outside = temp.path().join("outside");
        fs::create_dir(&target).unwrap();
        fs::create_dir(&outside).unwrap();
        let capabilities =
            SplitPathCapabilities::new(&source, &source, &[PathBuf::from("crates/demo")], &target).unwrap();
        symlink(&outside, target.join("redirect")).unwrap();
        capabilities
            .authorize_target(&target.join("redirect/file"))
            .unwrap_err();

        symlink(outside.join("missing"), target.join("dangling")).unwrap();
        capabilities.authorize_target(&target.join("dangling")).unwrap_err();
    }
}
