//! Typed `rail.toml` configuration and discovery.

mod change_detection;
mod release;
pub(crate) mod schema;
mod split;
mod surface;
mod unify;

pub use change_detection::{ChangeDetectionConfig, ConfidenceProfile, UnknownFilePolicy};
pub use release::{
    ChangelogConfig, ChangelogFilters, ChangelogRelativeTo, ChangelogShape, CommitPolicy, CrateReleaseConfig,
    GroupSpec, Pre1BreakingBump, ReleaseConfig, ReleaseRemoteEffects, ReleaseSource, RequireChangeFiles,
    SemverCheckPolicy,
};
pub use split::{CratePath, CrateSplitConfig, SplitConfig, SplitMode, WorkspaceMode};
pub use surface::{
    SurfaceConfig, SurfaceConsumerScope, SurfaceCrateVisibility, SurfaceDoctest, SurfaceDoctestCoverage,
    SurfaceExclude, SurfaceExternal, SurfaceFeatureProfile, SurfaceLintDirective, SurfaceLintLevel, SurfaceOverride,
    SurfaceProduct,
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

pub(crate) const REMOVED_RUN_CONFIG_MESSAGE: &str = "[run] configuration is no longer supported; execute Cargo and cargo-nextest directly, keep repository tasks in Just or CI, and consume typed package scope from `cargo rail plan`";
pub(crate) const REMOVED_CACHE_CONFIG_MESSAGE: &str = "[cache] repository configuration is no longer supported; select optional L2 authority from machine or CI state with CARGO_RAIL_CACHE_REMOTE";

pub(crate) fn reject_removed_configuration(doc: &toml_edit::DocumentMut) -> Result<(), String> {
    for (name, message) in [
        ("run", REMOVED_RUN_CONFIG_MESSAGE),
        ("cache", REMOVED_CACHE_CONFIG_MESSAGE),
    ] {
        if doc.as_table().contains_key(name) {
            return Err(message.to_string());
        }
    }
    Ok(())
}

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
    /// Change detection settings (for planner classification)
    #[serde(default, rename = "change-detection")]
    pub change_detection: ChangeDetectionConfig,
    /// Rust source-surface analysis policy
    #[serde(default)]
    pub surface: SurfaceConfig,
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

impl RailConfig {
    fn parse_path(path: &Path) -> Result<(Self, Vec<u8>), String> {
        let bytes = fs::read(path).map_err(|error| format!("failed to read file: {error}"))?;
        let content = std::str::from_utf8(&bytes).map_err(|error| format!("file is not valid UTF-8: {error}"))?;
        let doc: toml_edit::DocumentMut = content
            .parse()
            .map_err(|error: toml_edit::TomlError| error.to_string())?;
        reject_removed_configuration(&doc)?;
        for deprecation in schema::present_deprecations(&doc) {
            if let Some(message) = deprecation.spec.deprecation {
                crate::warn!("{} in {}: {}", deprecation.path, path.display(), message);
            }
        }
        let config = toml_edit::de::from_document(doc).map_err(|error| error.to_string())?;
        Ok((config, bytes))
    }

    pub(crate) fn load_path_with_bytes(path: &Path) -> RailResult<(Self, Vec<u8>)> {
        Self::parse_path(path).map_err(|message| {
            RailError::Config(ConfigError::ParseError {
                path: path.to_path_buf(),
                message,
            })
        })
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
        if let Some(found) = candidates.iter().find(|p| p.exists()) {
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
                if let Some(found) = canonical_candidates.iter().find(|p| p.exists()) {
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
    /// "not found" and "parse error". This is used by WorkspaceContext to
    /// properly report parse errors instead of silently falling back to defaults.
    pub fn try_load(path: &Path) -> ConfigLoadResult {
        let config_path = match Self::find_config_path(path) {
            Some(p) => p,
            None => return ConfigLoadResult::NotFound,
        };

        match Self::parse_path(&config_path) {
            Ok((config, _)) => ConfigLoadResult::Loaded(Box::new(config)),
            Err(message) => ConfigLoadResult::ParseError {
                path: config_path,
                message,
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
