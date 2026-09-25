//! Select and check Cargo-Rail policy before any Git or Cargo subprocess starts.
//!
//! Cargo owns workspace selection. Discovered policy lives at Cargo's workspace
//! root, so it can be checked early only when manifests alone prove that root.
//! The proof follows Cargo's root search and defers every layout whose outcome
//! depends on rules modeled only by Cargo. An early rejection therefore names
//! the same file that post-metadata discovery would select, and Cargo's reported
//! root still decides which checked policy the context captures.

use crate::config::{RailConfig, RetiredKeys};
use crate::error::RailResult;
use std::path::{Path, PathBuf};

/// Policy source whose bytes decoded and whose workspace-independent rules passed.
#[derive(Debug)]
pub(super) struct CheckedConfig {
    pub(super) path: PathBuf,
    pub(super) bytes: Vec<u8>,
    pub(super) config: RailConfig,
}

impl CheckedConfig {
    fn load(path: PathBuf, retired: RetiredKeys) -> RailResult<Self> {
        let (decoded, bytes) = crate::config::load_decoded_with(&path, retired)?;
        decoded
            .config
            .validate_policy()
            .map_err(|error| error.context(format!("configuration {}", path.display())))?;
        Ok(Self {
            path,
            bytes,
            config: decoded.config,
        })
    }
}

/// Policy selection made before Cargo resolves the workspace.
#[derive(Debug)]
pub(super) enum ConfigPreflight {
    /// An explicit override names policy independently of Cargo.
    Explicit(CheckedConfig),
    /// Discovery ran at the root Cargo selects for the requested manifest.
    Discovered {
        workspace_root: PathBuf,
        config: Option<CheckedConfig>,
    },
    /// Only Cargo can decide the workspace root; discovery waits for metadata.
    Deferred,
}

impl ConfigPreflight {
    /// Check an explicit override, or policy discovered at a provable Cargo root.
    ///
    /// `manifest_root` is the canonical directory whose `Cargo.toml` Cargo receives.
    pub(super) fn capture(
        manifest_root: &Path,
        config_override: Option<PathBuf>,
        retired: RetiredKeys,
    ) -> RailResult<Self> {
        if let Some(path) = config_override {
            return CheckedConfig::load(path, retired).map(Self::Explicit);
        }
        let Some(workspace_root) = cargo_workspace_root(manifest_root) else {
            return Ok(Self::Deferred);
        };
        let config = RailConfig::find_config_path(&workspace_root)
            .map(|path| CheckedConfig::load(path, retired))
            .transpose()?;
        Ok(Self::Discovered { workspace_root, config })
    }

    /// Return checked policy for the workspace root Cargo reported.
    pub(super) fn resolve(
        self,
        cargo_workspace_root: &Path,
        retired: RetiredKeys,
    ) -> RailResult<Option<CheckedConfig>> {
        match self {
            Self::Explicit(config) => Ok(Some(config)),
            Self::Discovered { workspace_root, config } if workspace_root == cargo_workspace_root => Ok(config),
            Self::Discovered { .. } | Self::Deferred => RailConfig::find_config_path(cargo_workspace_root)
                .map(|path| CheckedConfig::load(path, retired))
                .transpose(),
        }
    }
}

/// Directory whose policy governs `requested_root` before Cargo runs: the workspace root Cargo
/// provably selects, or the requested directory itself when only Cargo can decide.
pub(crate) fn discovery_root(requested_root: &Path) -> PathBuf {
    match crate::utils::canonicalize_existing(requested_root) {
        Ok(canonical) => cargo_workspace_root(&canonical).unwrap_or(canonical),
        Err(_) => requested_root.to_path_buf(),
    }
}

/// Workspace role that one manifest declares for Cargo's root search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManifestRole {
    /// `[workspace]` is present; `excludes` when `workspace.exclude` may name the package.
    WorkspaceRoot { excludes: bool },
    /// A package that leaves its root to Cargo's ancestor search.
    Package,
    /// A shape whose root only Cargo can resolve, such as `package.workspace`.
    Unresolved,
}

/// Return the root Cargo selects for `manifest_root/Cargo.toml` when manifests prove it.
///
/// A manifest with `[workspace]` is its own root. Otherwise Cargo selects the
/// nearest ancestor with `[workspace]` that does not exclude the package, or
/// the package itself when no ancestor claims it. Exclusion lists, explicit
/// root pointers, unreadable manifests, and Cargo's search boundaries defer.
fn cargo_workspace_root(manifest_root: &Path) -> Option<PathBuf> {
    if std::env::var_os("__CARGO_TEST_ROOT").is_some() || within_cargo_home(manifest_root) {
        return None;
    }
    match manifest_role(&manifest_root.join("Cargo.toml"))? {
        ManifestRole::WorkspaceRoot { .. } => return Some(manifest_root.to_path_buf()),
        ManifestRole::Package => {}
        ManifestRole::Unresolved => return None,
    }
    for ancestor in manifest_root.ancestors().skip(1) {
        // Cargo ends its search at packaged sources and treats the package as the root.
        if ancestor.ends_with("target/package") {
            return None;
        }
        let manifest = ancestor.join("Cargo.toml");
        if !manifest.try_exists().ok()? {
            continue;
        }
        match manifest_role(&manifest)? {
            ManifestRole::WorkspaceRoot { excludes: false } => return Some(ancestor.to_path_buf()),
            ManifestRole::WorkspaceRoot { excludes: true } => return None,
            ManifestRole::Package | ManifestRole::Unresolved => {}
        }
    }
    Some(manifest_root.to_path_buf())
}

/// Cargo does not search above its home directory. Every plausible home defers.
fn within_cargo_home(manifest_root: &Path) -> bool {
    let configured = std::env::var_os("CARGO_HOME").map(PathBuf::from);
    let defaults = ["HOME", "USERPROFILE"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(|home| PathBuf::from(home).join(".cargo"));
    configured.into_iter().chain(defaults).any(|home| {
        manifest_root.starts_with(&home)
            || crate::utils::canonicalize_existing(&home).is_ok_and(|home| manifest_root.starts_with(home))
    })
}

fn manifest_role(path: &Path) -> Option<ManifestRole> {
    let manifest: toml_edit::DocumentMut = std::fs::read_to_string(path).ok()?.parse().ok()?;
    if let Some(workspace) = manifest.get("workspace") {
        let excludes = workspace
            .as_table_like()?
            .get("exclude")
            .is_some_and(|exclude| exclude.as_array().is_none_or(|paths| !paths.is_empty()));
        return Some(ManifestRole::WorkspaceRoot { excludes });
    }
    let package = manifest.get("package").or_else(|| manifest.get("project"))?;
    Some(if package.as_table_like()?.contains_key("workspace") {
        ManifestRole::Unresolved
    } else {
        ManifestRole::Package
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const PACKAGE: &str = "[package]\nname = 'member'\nversion = '0.1.0'\nedition.workspace = true\n";

    fn root() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = crate::utils::canonicalize_existing(directory.path()).expect("canonical temporary directory");
        (directory, path)
    }

    fn manifest(directory: &Path, contents: &str) {
        fs::create_dir_all(directory).expect("manifest directory");
        fs::write(directory.join("Cargo.toml"), contents).expect("manifest");
    }

    #[test]
    fn workspace_manifest_is_its_own_root() {
        let (_directory, root) = root();
        manifest(&root, "[workspace]\nmembers = ['crates/*']\n");
        let nested = root.join("crates/nested");
        manifest(&nested, "[workspace]\n[package]\nname = 'nested'\nversion = '0.1.0'\n");
        assert_eq!(cargo_workspace_root(&root), Some(root.clone()));
        assert_eq!(cargo_workspace_root(&nested), Some(nested));
    }

    #[test]
    fn member_selects_nearest_claiming_ancestor() {
        let (_directory, root) = root();
        manifest(&root, "workspace = { members = ['crates/*'] }\n");
        manifest(
            &root.join("crates"),
            "[package]\nname = 'intermediate'\nversion = '0.1.0'\n",
        );
        let member = root.join("crates/member");
        manifest(&member, PACKAGE);
        assert_eq!(cargo_workspace_root(&member), Some(root));
    }

    #[test]
    fn package_without_claiming_ancestor_is_its_own_root() {
        let (_directory, root) = root();
        let package = root.join("package");
        manifest(&package, PACKAGE);
        assert_eq!(cargo_workspace_root(&package), Some(package.clone()));
        manifest(&root, "[workspace]\nexclude = []\n");
        assert_eq!(cargo_workspace_root(&package), Some(root));
    }

    #[test]
    fn cargo_owned_membership_rules_defer() {
        let (_directory, root) = root();
        let member = root.join("member");
        manifest(
            &member,
            "[package]\nname = 'member'\nversion = '0.1.0'\nworkspace = '..'\n",
        );
        assert_eq!(cargo_workspace_root(&member), None, "explicit root pointer");

        manifest(&member, PACKAGE);
        manifest(&root, "[workspace]\nexclude = ['member']\n");
        assert_eq!(cargo_workspace_root(&member), None, "exclusion list");

        manifest(&root, "[workspace\n");
        assert_eq!(cargo_workspace_root(&member), None, "malformed ancestor");
        assert_eq!(cargo_workspace_root(&root), None, "malformed requested manifest");

        manifest(&root, "[dependencies]\n");
        assert_eq!(cargo_workspace_root(&root), None, "virtual manifest without workspace");
        assert_eq!(cargo_workspace_root(&root.join("missing")), None, "missing manifest");

        let packaged = root.join("target/package/member-0.1.0");
        manifest(&packaged, PACKAGE);
        assert_eq!(cargo_workspace_root(&packaged), None, "packaged source");
    }
}
