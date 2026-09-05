//! Typed `rail.toml` configuration and discovery.

mod compatibility;
pub(crate) mod plan;
mod release;
pub(crate) mod schema;
mod split;
mod surface;
mod unify;
pub(crate) use compatibility::Compatibility;

pub use plan::{
    CargoPrerequisiteConfig, CargoRootConfig, CargoTargetRootConfig, PlanConfig, PlanWorkConfig, PlanWorkScope,
};
pub use release::{
    ChangelogConfig, ChangelogFilters, ChangelogRelativeTo, ChangelogShape, CommitPolicy, CrateReleaseConfig,
    GroupSpec, Pre1BreakingBump, ReleaseConfig, ReleaseRegistryPublication, ReleaseRemoteEffects, ReleaseSource,
    RequireChangeFiles, SemverCheckPolicy,
};
pub use split::{CrateSplitConfig, SplitConfig, SplitMode, WorkspaceMode};
pub use surface::{
    SurfaceConfig, SurfaceConsumerScope, SurfaceCrateVisibility, SurfaceDoctest, SurfaceDoctestCoverage,
    SurfaceExclude, SurfaceExternal, SurfaceFeatureProfile, SurfaceLintDirective, SurfaceLintLevel, SurfaceOverride,
    SurfaceProduct, SurfaceTargetSelection,
};
pub use unify::{
    ConsumerScope, ExactPinHandling, MajorVersionConflict, MsrvPolicy, MsrvSource, TransitiveFeatureHost,
    TransitivePinning, UnifyConfig,
};

use crate::error::{ConfigError, RailError, RailResult};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Configuration for cargo-rail
/// Searched in order: rail.toml, .rail.toml, .cargo/rail.toml, .config/rail.toml
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RailConfig {
    /// Target triples for multi-platform validation (workspace-wide)
    /// Detected via `cargo rail init`, used by multiple commands
    #[serde(default)]
    pub targets: Vec<String>,
    /// Dependency unification settings
    #[serde(default)]
    pub unify: UnifyConfig,
    /// Release management settings
    #[serde(default)]
    pub release: ReleaseConfig,
    /// Rust source-surface analysis policy
    #[serde(default)]
    pub surface: SurfaceConfig,
    /// Input-only declarations for repository-owned planner work.
    #[serde(default)]
    pub plan: PlanConfig,
    /// Per-crate configuration (overrides workspace defaults)
    #[serde(default)]
    pub crates: BTreeMap<String, CrateConfig>,
}

/// Per-crate configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CrateConfig {
    /// Split/sync configuration for this crate
    pub split: Option<CrateSplitConfig>,
    /// Release configuration for this crate
    pub release: Option<CrateReleaseConfig>,
    /// Changelog configuration for this crate
    pub changelog: Option<ChangelogConfig>,
}

/// Result of attempting to load configuration
#[derive(Debug)]
pub enum ConfigLoadResult {
    /// Config loaded successfully
    Loaded(Box<RailConfig>),
    /// Config file found but failed to parse
    ParseError {
        /// Path to the config file that failed to parse
        path: PathBuf,
        /// Error message describing the parse failure
        message: String,
    },
    /// No config file found
    NotFound,
}

/// One parsed source and its effective policy. Source bytes remain owned by the caller.
#[derive(Debug)]
pub(crate) struct DecodedConfig {
    pub(crate) config: RailConfig,
    pub(crate) document: toml_edit::DocumentMut,
    pub(crate) compatibility: Vec<compatibility::Compatibility>,
}

/// Decode captured input once. Only predecessor split paths require the resolver.
pub(crate) fn decode(
    bytes: &[u8],
    resolve_member: impl FnMut(&Path) -> RailResult<String>,
) -> RailResult<DecodedConfig> {
    let content = std::str::from_utf8(bytes)
        .map_err(|error| RailError::message(format!("configuration is not valid UTF-8: {error}")))?;
    let mut document: toml_edit::DocumentMut = content
        .parse()
        .map_err(|error: toml_edit::TomlError| RailError::message(error.to_string()))?;
    let compatibility = compatibility::normalize_document(&mut document, resolve_member)?;
    let config = RailConfig::from_document(document.clone()).map_err(RailError::message)?;
    Ok(DecodedConfig {
        config,
        document,
        compatibility,
    })
}

/// Decode policy without inferring workspace facts from the caller's working directory.
pub(crate) fn decode_without_workspace(bytes: &[u8]) -> RailResult<DecodedConfig> {
    decode(bytes, |_| Err(workspace_context_required("split.paths")))
}

/// Decode a file while retaining its exact bytes for capture and drift validation.
pub(crate) fn load_decoded(
    path: &Path,
    resolve_member: impl FnMut(&Path) -> RailResult<String>,
) -> RailResult<(DecodedConfig, Vec<u8>)> {
    let bytes =
        fs::read(path).map_err(|error| RailError::message(format!("failed to read {}: {error}", path.display())))?;
    let decoded =
        decode(&bytes, resolve_member).map_err(|error| error.context(format!("configuration {}", path.display())))?;
    Ok((decoded, bytes))
}

/// Resolve a predecessor path using the caller's captured Cargo package facts.
pub(crate) fn resolve_split_member(metadata: &cargo_metadata::Metadata, relative: &Path) -> RailResult<String> {
    let manifest = split_member_manifest(metadata.workspace_root.as_std_path(), relative)?;
    metadata
        .packages
        .iter()
        .find(|package| {
            metadata.workspace_members.contains(&package.id) && package.manifest_path.as_std_path() == manifest
        })
        .map(|package| package.name.to_string())
        .ok_or_else(|| {
            RailError::message(format!(
                "split member path '{}' does not name a captured Cargo workspace member",
                relative.display()
            ))
        })
}

/// Validate the live lookup boundary without reading another copy of captured Cargo facts.
fn split_member_manifest(workspace_root: &Path, relative: &Path) -> RailResult<PathBuf> {
    let manifest = crate::source::RepositoryPath::new(&relative.join("Cargo.toml"))?;
    let absolute = workspace_root.join(manifest.as_path());
    let resolved = crate::utils::path_relative_to(workspace_root, &absolute)?;
    if resolved != manifest.as_path() || !fs::symlink_metadata(&absolute)?.is_file() {
        return Err(RailError::message(format!(
            "split member manifest '{}' must be a regular file inside the workspace without symbolic links",
            absolute.display()
        )));
    }
    Ok(absolute)
}

/// Extract a package fact from manifest bytes captured by the caller's source boundary.
pub(crate) fn split_member_name(bytes: &[u8], relative: &Path) -> RailResult<String> {
    let content = std::str::from_utf8(bytes).map_err(|error| RailError::message(error.to_string()))?;
    let document: toml_edit::DocumentMut = content
        .parse()
        .map_err(|error: toml_edit::TomlError| RailError::message(error.to_string()))?;
    document
        .get("package")
        .and_then(toml_edit::Item::as_table_like)
        .and_then(|package| package.get("name"))
        .and_then(toml_edit::Item::as_str)
        .map(str::to_owned)
        .ok_or_else(|| RailError::message(format!("split member '{}' has no package.name", relative.display())))
}

pub(crate) fn workspace_context_required(field: &str) -> RailError {
    RailError::message(format!(
        "{field} requires Cargo workspace context; inspect a configuration file in its owning workspace"
    ))
}

impl RailConfig {
    fn from_document(doc: toml_edit::DocumentMut) -> Result<Self, String> {
        if let Some(path) = schema::document_paths(&doc)
            .into_iter()
            .find(|path| !schema::is_known_config_path(path))
        {
            return Err(format!("unknown configuration key '{path}'"));
        }
        toml_edit::de::from_document(doc).map_err(|error| error.to_string())
    }

    /// Validate policy independently of any filesystem or workspace membership.
    pub(crate) fn validate_policy(&self) -> RailResult<()> {
        self.plan.validate().map_err(RailError::Config)?;
        self.surface.validate().map_err(RailError::Config)?;
        self.surface
            .validate_workspace_targets(&self.targets)
            .map_err(RailError::Config)?;
        self.unify
            .validate_workspace_targets(&self.targets)
            .map_err(RailError::Config)?;
        self.unify.validate_policy().map_err(RailError::Config)?;
        self.release.validate_policy().map_err(RailError::Config)?;
        for split in self.build_split_configs() {
            split.validate()?;
        }
        crate::release::changelog::ChangelogSpec::resolve(&self.release.changelog, None)?;
        for config in self.crates.values().filter_map(|config| config.changelog.as_ref()) {
            crate::release::changelog::ChangelogSpec::resolve(&self.release.changelog, Some(config))?;
        }
        Ok(())
    }

    /// A standalone source cannot claim validation of repository-dependent policy.
    pub(crate) fn validate_without_workspace(&self) -> RailResult<()> {
        self.validate_policy()?;
        if self.unify.transitive_host_path().is_some() {
            return Err(workspace_context_required("unify.transitive_pinning.host"));
        }
        if self
            .crates
            .values()
            .any(|config| config.split.is_some() || config.release.is_some() || config.changelog.is_some())
            || !self.release.version_groups.is_empty()
            || matches!(&self.release.require_change_files, RequireChangeFiles::Crates(names) if !names.is_empty())
        {
            return Err(workspace_context_required("package selection"));
        }
        if !self.targets.is_empty() {
            crate::targets::validate_targets(&self.targets)?;
        }
        Ok(())
    }

    /// Validate every semantic configuration rule available at this boundary.
    pub(crate) fn validate(
        &self,
        workspace_root: &Path,
        workspace_members: Option<&[String]>,
    ) -> RailResult<Vec<String>> {
        self.validate_policy()?;
        self.unify.validate_host(workspace_root).map_err(RailError::Config)?;
        let Some(workspace_members) = workspace_members else {
            return Ok(Vec::new());
        };
        if !self.targets.is_empty() {
            crate::targets::validate_targets(&self.targets)?;
        }
        let warnings = self.release.validate(workspace_members).map_err(RailError::Config)?;
        for (crate_name, crate_config) in &self.crates {
            // A split table may name a combined-repository boundary rather
            // than one Cargo package. Per-package release/changelog tables
            // without that boundary must still resolve to a real member.
            if crate_config.split.is_none()
                && (crate_config.release.is_some() || crate_config.changelog.is_some())
                && !workspace_members.contains(crate_name)
            {
                return Err(RailError::Config(ConfigError::CrateNotFound {
                    name: crate_name.clone(),
                }));
            }
        }
        for split in self.build_split_configs() {
            if let Some(member) = split.members.iter().find(|member| !workspace_members.contains(*member)) {
                return Err(RailError::Config(ConfigError::CrateNotFound { name: member.clone() }));
            }
        }
        Ok(warnings)
    }

    /// Load policy without Cargo discovery, capturing only explicitly referenced predecessor manifests.
    pub(crate) fn load_path_with_bytes(path: &Path, workspace_root: &Path) -> RailResult<(Self, Vec<u8>)> {
        let (decoded, bytes) = load_decoded(path, |relative| {
            let manifest = split_member_manifest(workspace_root, relative)?;
            let bytes = fs::read(&manifest)?;
            split_member_manifest(workspace_root, relative)?;
            split_member_name(&bytes, relative)
        })?;
        Ok((decoded.config, bytes))
    }

    /// Find config file in search order: rail.toml, .rail.toml, .cargo/rail.toml, .config/rail.toml
    ///
    /// On Windows, this handles path canonicalization issues (UNC paths, 8.3 short names)
    /// by checking both the original path and its parent's canonicalization.
    pub fn find_config_path(path: &Path) -> Option<PathBuf> {
        let candidates = [
            path.join("rail.toml"),
            path.join(".rail.toml"),
            path.join(".cargo").join("rail.toml"),
            path.join(".config").join("rail.toml"),
        ];

        // First, try the candidates as-is
        if let Some(found) = candidates.iter().find(|p| match p.symlink_metadata() {
            Ok(_) => true,
            Err(error) => error.kind() != std::io::ErrorKind::NotFound,
        }) {
            return Some(found.to_path_buf());
        }

        // On Windows, if path is canonicalized (e.g., from cargo metadata),
        // we may need to check using the original non-canonicalized path.
        #[cfg(target_os = "windows")]
        {
            // 1. Try canonicalizing the path and searching there
            // This handles 8.3 short paths vs long paths issues (RUNNER~1 vs runneradmin)
            if let Ok(canonical) = path.canonicalize() {
                let canonical_candidates = [
                    canonical.join("rail.toml"),
                    canonical.join(".rail.toml"),
                    canonical.join(".cargo").join("rail.toml"),
                    canonical.join(".config").join("rail.toml"),
                ];
                if let Some(found) = canonical_candidates.iter().find(|p| match p.symlink_metadata() {
                    Ok(_) => true,
                    Err(error) => error.kind() != std::io::ErrorKind::NotFound,
                }) {
                    return Some(found.to_path_buf());
                }
            }

            // 2. Try to find the config by reading the directory entries
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    let file_name = entry.file_name();
                    let file_name_str = file_name.to_string_lossy();

                    if file_name_str == "rail.toml" || file_name_str == ".rail.toml" {
                        return Some(entry.path());
                    }
                }
            }

            // Also check subdirectories .cargo and .config via read_dir
            for subdir in &[".cargo", ".config"] {
                let subdir_path = path.join(subdir);
                if let Ok(entries) = std::fs::read_dir(&subdir_path) {
                    for entry in entries.flatten() {
                        let file_name = entry.file_name();
                        if file_name.to_string_lossy() == "rail.toml" {
                            return Some(entry.path());
                        }
                    }
                }
            }
        }

        None
    }

    /// Load config from rail.toml (searches multiple locations).
    ///
    /// Searches: `rail.toml`, `.rail.toml`, `.cargo/rail.toml`, `.config/rail.toml`
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::NotFound`] if no config file exists.
    ///
    /// Returns [`ConfigError::ParseError`] if the config file cannot be read or parsed.
    pub fn load(path: &Path) -> RailResult<Self> {
        match Self::try_load(path) {
            ConfigLoadResult::Loaded(config) => Ok(*config),
            ConfigLoadResult::ParseError { path, message } => {
                Err(RailError::Config(ConfigError::ParseError { path, message }))
            }
            ConfigLoadResult::NotFound => Err(RailError::Config(ConfigError::NotFound {
                workspace_root: path.to_path_buf(),
            })),
        }
    }

    /// Try to load config, returning a result that distinguishes between
    /// "not found" and "parse error" without Cargo discovery.
    /// Supported predecessor split paths read only their declared package manifests.
    pub fn try_load(path: &Path) -> ConfigLoadResult {
        let config_path = match Self::find_config_path(path) {
            Some(p) => p,
            None => return ConfigLoadResult::NotFound,
        };

        match Self::load_path_with_bytes(&config_path, path) {
            Ok((config, _)) => ConfigLoadResult::Loaded(Box::new(config)),
            Err(error) => ConfigLoadResult::ParseError {
                path: config_path,
                message: error.to_string(),
            },
        }
    }

    /// Get all crates that have split configuration
    pub fn get_split_crates(&self) -> Vec<(&str, &CrateSplitConfig)> {
        self.crates
            .iter()
            .filter_map(|(name, config)| config.split.as_ref().map(|split| (name.as_str(), split)))
            .collect()
    }

    /// Build all SplitConfigs from unified crate config
    pub fn build_split_configs(&self) -> Vec<SplitConfig> {
        self.crates
            .iter()
            .filter_map(|(name, config)| {
                config.split.as_ref().map(|split_cfg| {
                    split::build_split_config(
                        name.clone(),
                        split_cfg,
                        config.release.as_ref().map(|r| r.publish),
                        config.changelog.as_ref(),
                    )
                })
            })
            .collect()
    }
}
