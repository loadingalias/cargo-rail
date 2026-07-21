//! Persistent store for compiler diagnostics cache entries.

use crate::compiler::model::{
  COLLECTOR_VERSION, COMPILER_DIAG_CACHE_VERSION, CompilerDiagCacheFile, CompilerDiagEntry, CompilerDiagKey,
};
use crate::error::RailResult;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_CACHE_ENTRIES: usize = 4096;
const MAX_CACHE_AGE_MS: u64 = 30 * 24 * 60 * 60 * 1000;

/// Persistent compiler diagnostics store.
pub struct CompilerDiagnosticsStore {
  path: PathBuf,
  cache: CompilerDiagCacheFile,
  dirty: bool,
  prior_by_configuration: HashMap<String, (u64, CompilerDiagKey, u32)>,
  cached_packages: HashSet<String>,
  discarded_reason: Option<&'static str>,
}

impl CompilerDiagnosticsStore {
  /// Load diagnostics store from `target/cargo-rail/cache/compiler-diags-v1.json`.
  pub fn load(workspace_root: &Path) -> Self {
    let path = crate::workspace::cargo_rail_state_root(workspace_root)
      .join("cache")
      .join("compiler-diags-v1.json");

    let raw = std::fs::read_to_string(&path).ok();
    let parsed = raw.as_deref().map(serde_json::from_str::<CompilerDiagCacheFile>);
    let discarded_reason = match parsed.as_ref() {
      Some(Err(_)) => Some("cache_unreadable"),
      Some(Ok(file)) if file.version != COMPILER_DIAG_CACHE_VERSION => Some("schema_changed"),
      _ => None,
    };
    let cache = parsed
      .and_then(Result::ok)
      .filter(|file| file.version == COMPILER_DIAG_CACHE_VERSION)
      .unwrap_or_default();
    let mut store = Self {
      path,
      cache,
      dirty: false,
      prior_by_configuration: HashMap::new(),
      cached_packages: HashSet::new(),
      discarded_reason,
    };
    store.rebuild_index();
    store
  }

  /// Return cached entry for the exact key.
  pub fn get(&self, key: &CompilerDiagKey) -> Option<&CompilerDiagEntry> {
    self
      .cache
      .entries
      .get(&key.stable_id())
      .filter(|entry| entry.collector_version == COLLECTOR_VERSION)
  }

  /// Explain why an exact semantic key was not reusable.
  pub fn miss_reason(&self, key: &CompilerDiagKey) -> &'static str {
    if let Some(reason) = self.discarded_reason {
      return reason;
    }
    let Some((_, prior, collector_version)) = self.prior_by_configuration.get(&configuration_id(key)) else {
      return if self.cached_packages.contains(&key.package_id.to_string()) {
        "configuration_not_cached"
      } else {
        "cold_cache"
      };
    };
    if *collector_version != COLLECTOR_VERSION {
      "collector_changed"
    } else if prior.toolchain_fingerprint != key.toolchain_fingerprint
      || prior.rustc_version != key.rustc_version
      || prior.cargo_version != key.cargo_version
      || prior.host_triple != key.host_triple
    {
      "toolchain_changed"
    } else if prior.target_fingerprint != key.target_fingerprint {
      "target_identity_changed"
    } else if prior.lock_fingerprint != key.lock_fingerprint {
      "lockfile_changed"
    } else if prior.manifest_fingerprint != key.manifest_fingerprint {
      "manifest_changed"
    } else if prior.source_fingerprint != key.source_fingerprint {
      "source_changed"
    } else if prior.compiler_env_fingerprint != key.compiler_env_fingerprint {
      "compiler_environment_changed"
    } else if prior.cargo_config_fingerprint != key.cargo_config_fingerprint {
      "cargo_config_changed"
    } else {
      "semantic_key_changed"
    }
  }

  /// Upsert one entry.
  pub fn put(&mut self, entry: CompilerDiagEntry) {
    let id = entry.key.stable_id();
    self.cached_packages.insert(entry.key.package_id.to_string());
    self.prior_by_configuration.insert(
      configuration_id(&entry.key),
      (entry.generated_at_unix_ms, entry.key.clone(), entry.collector_version),
    );
    self.cache.entries.insert(id, entry);
    self.dirty = true;
  }

  /// Persist dirty cache state to disk.
  pub fn flush(&mut self) -> RailResult<()> {
    if !self.dirty {
      return Ok(());
    }

    if let Some(parent) = self.path.parent() {
      std::fs::create_dir_all(parent)?;
    }

    self.prune();
    let bytes = serde_json::to_vec(&self.cache)?;
    crate::utils::write_file_atomic(&self.path, &bytes)?;
    self.dirty = false;
    Ok(())
  }

  fn prune(&mut self) {
    let now = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .map(|duration| duration.as_millis() as u64)
      .unwrap_or(0);
    self
      .cache
      .entries
      .retain(|_, entry| now.saturating_sub(entry.generated_at_unix_ms) <= MAX_CACHE_AGE_MS);
    if self.cache.entries.len() <= MAX_CACHE_ENTRIES {
      return;
    }
    let mut by_age: Vec<_> = self
      .cache
      .entries
      .iter()
      .map(|(key, entry)| (entry.generated_at_unix_ms, key.clone()))
      .collect();
    by_age.sort_unstable();
    for (_, key) in by_age.into_iter().take(self.cache.entries.len() - MAX_CACHE_ENTRIES) {
      self.cache.entries.remove(&key);
    }
  }

  fn rebuild_index(&mut self) {
    for entry in self.cache.entries.values() {
      self.cached_packages.insert(entry.key.package_id.to_string());
      let id = configuration_id(&entry.key);
      let replace = self
        .prior_by_configuration
        .get(&id)
        .is_none_or(|(generated, _, _)| *generated < entry.generated_at_unix_ms);
      if replace {
        self.prior_by_configuration.insert(
          id,
          (entry.generated_at_unix_ms, entry.key.clone(), entry.collector_version),
        );
      }
    }
  }
}

fn configuration_id(key: &CompilerDiagKey) -> String {
  format!(
    "{}\u{1f}{}\u{1f}{}",
    key.package_id,
    key.target.as_str(),
    key.features.label()
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::compiler::model::{DiagnosticsCompleteness, FeatureSelection, PlatformTarget, TargetEvidence};
  use cargo_metadata::PackageId;
  use std::collections::BTreeSet;

  fn key() -> CompilerDiagKey {
    CompilerDiagKey {
      package_id: PackageId {
        repr: "path+file:///workspace#member@0.1.0".to_string(),
      },
      target: PlatformTarget::from("default"),
      features: FeatureSelection::Default,
      rustc_version: "rustc test".to_string(),
      cargo_version: "cargo test".to_string(),
      host_triple: "test-host".to_string(),
      toolchain_fingerprint: "sha256:toolchain".to_string(),
      target_fingerprint: "sha256:target".to_string(),
      lock_fingerprint: "sha256:lock".to_string(),
      manifest_fingerprint: "sha256:manifest".to_string(),
      source_fingerprint: "sha256:source".to_string(),
      compiler_env_fingerprint: "sha256:environment".to_string(),
      cargo_config_fingerprint: "sha256:configuration".to_string(),
    }
  }

  #[test]
  fn prior_collector_semantics_never_authorize_reuse() {
    let root = tempfile::tempdir().expect("temporary workspace should be created");
    let path = root.path().join("target/cargo-rail/cache/compiler-diags-v1.json");
    std::fs::create_dir_all(path.parent().expect("cache path should have a parent"))
      .expect("cache directory should be created");
    let key = key();
    let mut cache = CompilerDiagCacheFile::default();
    cache.entries.insert(
      key.stable_id(),
      CompilerDiagEntry {
        key: key.clone(),
        evidence: TargetEvidence {
          platform: PlatformTarget::from("default"),
          features: FeatureSelection::Default,
          compiled_units: BTreeSet::new(),
          unused_crates: BTreeSet::new(),
          unit_evidence: Vec::new(),
          completeness: DiagnosticsCompleteness::Complete,
        },
        generated_at_unix_ms: 1,
        collector_version: COLLECTOR_VERSION - 1,
        observations: Vec::new(),
      },
    );
    std::fs::write(&path, serde_json::to_vec(&cache).expect("cache should serialize"))
      .expect("cache should be written");

    let store = CompilerDiagnosticsStore::load(root.path());
    assert!(
      store.get(&key).is_none(),
      "prior collector evidence must not be returned"
    );
    assert_eq!(store.miss_reason(&key), "collector_changed");
  }
}
