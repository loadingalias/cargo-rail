//! Feature minimization cache
//!
//! Caches the results of feature minimization to avoid re-testing unchanged dependencies.
//! This makes subsequent `cargo rail unify --minimal` runs nearly instant.
//!
//! # Cache Strategy
//!
//! Cache key: `(dep_name, version_req, features_hash)`
//! Cache value: `minimal_features`
//!
//! Invalidation triggers:
//! - Dependency version changed
//! - Available features changed (crate update)
//! - Workspace structure changed (new members)
//! - Manual invalidation (--force-minimize)

use crate::cargo::unify::UnifiedDep;
use crate::error::RailResult;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Cache entry for a single dependency's minimization result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
  /// Dependency name
  pub dep_name: String,

  /// Version requirement (for invalidation detection)
  pub version_req: String,

  /// Hash of available features (for invalidation detection)
  pub features_hash: u64,

  /// Minimal feature set (cached result)
  pub minimal_features: Vec<String>,

  /// Original feature count (for statistics)
  pub original_feature_count: usize,

  /// Timestamp when cached
  pub cached_at: String,

  /// Hash of minimization config (for invalidation when config changes)
  #[serde(default)]
  pub config_hash: u64,
}

/// Feature minimization cache
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FeatureCache {
  /// Cache entries keyed by dependency name
  entries: HashMap<String, CacheEntry>,

  /// Cache metadata
  pub metadata: CacheMetadata,
}

/// Cache metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheMetadata {
  /// When the cache was created
  pub created_at: String,

  /// Last updated timestamp
  pub updated_at: String,

  /// Number of entries
  pub entry_count: usize,
}

impl Default for CacheMetadata {
  fn default() -> Self {
    let now = chrono::Utc::now().to_rfc3339();
    Self {
      created_at: now.clone(),
      updated_at: now,
      entry_count: 0,
    }
  }
}

impl FeatureCache {
  /// Load cache from disk
  pub fn load(workspace_root: &Path) -> RailResult<Self> {
    let cache_path = Self::cache_path(workspace_root);

    if !cache_path.exists() {
      // No cache file - return empty cache
      return Ok(Self::default());
    }

    let content = std::fs::read_to_string(&cache_path)?;
    let cache: FeatureCache = serde_json::from_str(&content)?;

    Ok(cache)
  }

  /// Save cache to disk
  pub fn save(&self, workspace_root: &Path) -> RailResult<()> {
    let cache_path = Self::cache_path(workspace_root);

    // Ensure parent directory exists
    if let Some(parent) = cache_path.parent() {
      std::fs::create_dir_all(parent)?;
    }

    let content = serde_json::to_string_pretty(self)?;
    std::fs::write(&cache_path, content)?;

    Ok(())
  }

  /// Get cache path
  fn cache_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join("target/.cargo-rail/feature-cache.json")
  }

  /// Check if dependency is cached and still valid
  pub fn get(&self, dep: &UnifiedDep) -> Option<&CacheEntry> {
    let entry = self.entries.get(&dep.name)?;

    // Validate cache entry is still valid
    if entry.version_req != dep.version_req.to_string() {
      // Version changed - cache invalid
      return None;
    }

    let current_hash = Self::hash_features(&dep.features);
    if entry.features_hash != current_hash {
      // Features changed - cache invalid
      return None;
    }

    Some(entry)
  }

  /// Check if cache entry is valid for given config
  pub fn get_with_config(&self, dep: &UnifiedDep, config_hash: u64) -> Option<&CacheEntry> {
    let entry = self.get(dep)?;

    // Also check config hash
    if entry.config_hash != config_hash {
      // Config changed - cache invalid
      return None;
    }

    Some(entry)
  }

  /// Insert or update cache entry
  pub fn insert(&mut self, dep: &UnifiedDep, minimal_features: Vec<String>, config_hash: u64) {
    let entry = CacheEntry {
      dep_name: dep.name.clone(),
      version_req: dep.version_req.to_string(),
      features_hash: Self::hash_features(&dep.features),
      minimal_features,
      original_feature_count: dep.features.len(),
      cached_at: chrono::Utc::now().to_rfc3339(),
      config_hash,
    };

    self.entries.insert(dep.name.clone(), entry);
    self.metadata.entry_count = self.entries.len();
    self.metadata.updated_at = chrono::Utc::now().to_rfc3339();
  }

  /// Compute hash of feature list for invalidation detection
  fn hash_features(features: &[String]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();

    // Sort for deterministic hashing
    let mut sorted = features.to_vec();
    sorted.sort();

    sorted.hash(&mut hasher);
    hasher.finish()
  }

  /// Get cache statistics
  pub fn stats(&self) -> CacheStats {
    CacheStats {
      total_entries: self.entries.len(),
    }
  }
}

/// Cache statistics for reporting
pub struct CacheStats {
  pub total_entries: usize,
}

#[cfg(test)]
mod tests {
  use super::*;
  use semver::VersionReq;
  use std::collections::HashSet;

  #[test]
  fn test_hash_features_deterministic() {
    let features1 = vec!["std".to_string(), "derive".to_string(), "async".to_string()];

    let features2 = vec!["async".to_string(), "std".to_string(), "derive".to_string()];

    // Same features, different order -> same hash
    assert_eq!(
      FeatureCache::hash_features(&features1),
      FeatureCache::hash_features(&features2)
    );
  }

  #[test]
  fn test_hash_features_different() {
    let features1 = vec!["std".to_string(), "derive".to_string()];
    let features2 = vec!["std".to_string(), "async".to_string()];

    // Different features -> different hash
    assert_ne!(
      FeatureCache::hash_features(&features1),
      FeatureCache::hash_features(&features2)
    );
  }

  #[test]
  fn test_cache_invalidation_version_change() {
    let mut cache = FeatureCache::default();

    let dep = crate::cargo::unify::UnifiedDep {
      name: "serde".to_string(),
      version_req: VersionReq::parse("^1.0").unwrap(),
      features: vec!["derive".to_string()],
      feature_provenance: HashMap::new(),
      default_features: true,
      used_by: vec!["test".to_string()],
      dep_kinds: HashSet::new(),
      fragmentation_count: 1,
      path: None,
      target: None,
      comments: vec![],
      is_proc_macro: false,
    };

    cache.insert(&dep, vec!["derive".to_string()], 0);

    // Same version -> cache hit
    assert!(cache.get(&dep).is_some());

    // Different version -> cache miss
    let mut dep_v2 = dep.clone();
    dep_v2.version_req = VersionReq::parse("^2.0").unwrap();
    assert!(cache.get(&dep_v2).is_none());
  }

  #[test]
  fn test_cache_invalidation_features_change() {
    let mut cache = FeatureCache::default();

    let dep = crate::cargo::unify::UnifiedDep {
      name: "tokio".to_string(),
      version_req: VersionReq::parse("^1.0").unwrap(),
      features: vec!["runtime".to_string(), "macros".to_string()],
      feature_provenance: HashMap::new(),
      default_features: true,
      used_by: vec!["test".to_string()],
      dep_kinds: HashSet::new(),
      fragmentation_count: 1,
      path: None,
      target: None,
      comments: vec![],
      is_proc_macro: false,
    };

    cache.insert(&dep, vec!["runtime".to_string()], 0);

    // Same features -> cache hit
    assert!(cache.get(&dep).is_some());

    // Different features -> cache miss
    let mut dep_new_features = dep.clone();
    dep_new_features.features = vec!["runtime".to_string(), "macros".to_string(), "fs".to_string()];
    assert!(cache.get(&dep_new_features).is_none());
  }
}
