//! Persistent store for compiler diagnostics cache entries.

use crate::compiler::model::{
  COLLECTOR_VERSION, COMPILER_DIAG_CACHE_VERSION, CompilerDiagCacheFile, CompilerDiagEntry, CompilerDiagKey,
};
use crate::error::RailResult;
use std::path::{Path, PathBuf};

/// Persistent compiler diagnostics store.
pub struct CompilerDiagnosticsStore {
  path: PathBuf,
  cache: CompilerDiagCacheFile,
  dirty: bool,
}

impl CompilerDiagnosticsStore {
  /// Load diagnostics store from `target/cargo-rail/cache/compiler-diags-v1.json`.
  pub fn load(workspace_root: &Path) -> Self {
    let path = workspace_root
      .join("target")
      .join("cargo-rail")
      .join("cache")
      .join("compiler-diags-v1.json");

    let cache = std::fs::read_to_string(&path)
      .ok()
      .and_then(|raw| serde_json::from_str::<CompilerDiagCacheFile>(&raw).ok())
      .filter(|file| file.version == COMPILER_DIAG_CACHE_VERSION)
      .unwrap_or_default();

    Self {
      path,
      cache,
      dirty: false,
    }
  }

  /// Return cached entry for the exact key.
  pub fn get(&self, key: &CompilerDiagKey) -> Option<&CompilerDiagEntry> {
    self
      .cache
      .entries
      .get(&key.stable_id())
      .filter(|entry| entry.collector_version == COLLECTOR_VERSION)
  }

  /// Upsert one entry.
  pub fn put(&mut self, entry: CompilerDiagEntry) {
    let id = entry.key.stable_id();
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

    let json = serde_json::to_string(&self.cache)?;
    std::fs::write(&self.path, json)?;
    self.dirty = false;
    Ok(())
  }
}
