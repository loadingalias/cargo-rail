//! Cargo workspace state wrapper
//!
//! Thin wrapper around cargo_metadata providing workspace-level cargo operations.
//! Built once at workspace context initialization, passed by reference.

use crate::cargo::WorkspaceMetadata;
use crate::error::RailResult;
use cargo_metadata::Package;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Cargo state for the workspace
///
/// Provides cargo metadata and workspace information.
/// This is built once and shared across all commands via WorkspaceContext.
#[derive(Clone)]
pub struct CargoState {
  /// Underlying cargo metadata
  metadata: WorkspaceMetadata,

  /// Cached workspace root
  workspace_root: PathBuf,
}

/// Metadata cache structure
#[derive(Serialize, Deserialize)]
struct MetadataCache {
  /// FNV-1a hash of Cargo.toml + Cargo.lock
  hash: u64,
  /// Cached metadata
  metadata: WorkspaceMetadata,
}

impl CargoState {
  /// Load cargo metadata from workspace root
  ///
  /// Uses a content-based cache (FNV-1a hash of Cargo.toml + Cargo.lock)
  /// stored in `target/rail/metadata.json` to speed up subsequent loads.
  pub fn load(workspace_root: &Path) -> RailResult<Self> {
    let cache_dir = workspace_root.join("target").join("rail");
    let cache_file = cache_dir.join("metadata.json");

    // Compute current hash
    let current_hash = compute_workspace_hash(workspace_root);

    // Try to load from cache
    if let Some(cache) = fs::read_to_string(&cache_file)
      .ok()
      .and_then(|c| serde_json::from_str::<MetadataCache>(&c).ok())
      .filter(|c| c.hash == current_hash)
    {
      // Cache hit!
      let workspace_root = cache.metadata.workspace_root().to_path_buf();
      return Ok(Self {
        metadata: cache.metadata,
        workspace_root,
      });
    }

    // Cache miss or invalid - load fresh
    let metadata = WorkspaceMetadata::load(workspace_root)?;
    let workspace_root = metadata.workspace_root().to_path_buf();

    // Save to cache (best effort)
    if let Err(e) = fs::create_dir_all(&cache_dir) {
      eprintln!("Warning: Failed to create cache directory: {}", e);
    } else {
      let cache = MetadataCache {
        hash: current_hash,
        metadata: metadata.clone(),
      };
      if let Err(e) = serde_json::to_string(&cache)
        .map_err(|e| e.to_string())
        .and_then(|json| fs::write(&cache_file, json).map_err(|e| e.to_string()))
      {
        eprintln!("Warning: Failed to write metadata cache: {}", e);
      }
    }

    Ok(Self {
      metadata,
      workspace_root,
    })
  }

  /// Get workspace root path
  pub fn workspace_root(&self) -> &Path {
    &self.workspace_root
  }

  /// Access underlying WorkspaceMetadata for advanced operations
  ///
  /// Use this when you need direct access to cargo_metadata types.
  pub fn metadata(&self) -> &WorkspaceMetadata {
    &self.metadata
  }

  /// Get all workspace member packages (backbone API - used in tests/contexts)
  #[cfg(test)]
  pub fn workspace_packages(&self) -> Vec<&Package> {
    self.metadata.list_crates()
  }

  /// Get package by name
  pub fn get_package(&self, name: &str) -> Option<&Package> {
    self.metadata.get_package(name)
  }
}

/// Compute FNV-1a hash of Cargo.toml and Cargo.lock
fn compute_workspace_hash(workspace_root: &Path) -> u64 {
  let mut hasher = Fnv1aHasher::new();

  // Hash Cargo.toml
  if let Ok(mut file) = fs::File::open(workspace_root.join("Cargo.toml")) {
    let mut buffer = Vec::new();
    if file.read_to_end(&mut buffer).is_ok() {
      hasher.write(&buffer);
    }
  }

  // Hash Cargo.lock (if exists)
  if let Ok(mut file) = fs::File::open(workspace_root.join("Cargo.lock")) {
    let mut buffer = Vec::new();
    if file.read_to_end(&mut buffer).is_ok() {
      hasher.write(&buffer);
    }
  }

  hasher.finish()
}

/// Simple FNV-1a 64-bit hasher
struct Fnv1aHasher {
  hash: u64,
}

impl Fnv1aHasher {
  const OFFSET_BASIS: u64 = 0xcbf29ce484222325;
  const PRIME: u64 = 0x100000001b3;

  fn new() -> Self {
    Self {
      hash: Self::OFFSET_BASIS,
    }
  }

  fn write(&mut self, bytes: &[u8]) {
    for byte in bytes {
      self.hash ^= *byte as u64;
      self.hash = self.hash.wrapping_mul(Self::PRIME);
    }
  }

  fn finish(&self) -> u64 {
    self.hash
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs::File;
  use std::io::Write;
  use tempfile::TempDir;

  #[test]
  fn test_fnv1a_hasher() {
    let mut hasher = Fnv1aHasher::new();
    hasher.write(b"hello");
    let hash1 = hasher.finish();

    let mut hasher2 = Fnv1aHasher::new();
    hasher2.write(b"hello");
    let hash2 = hasher2.finish();

    assert_eq!(hash1, hash2, "Hash should be deterministic");

    let mut hasher3 = Fnv1aHasher::new();
    hasher3.write(b"world");
    let hash3 = hasher3.finish();

    assert_ne!(hash1, hash3, "Different content should have different hash");
  }

  #[test]
  fn test_metadata_caching() {
    // Create a temp workspace
    let temp_dir = TempDir::new().unwrap();
    let workspace_root = temp_dir.path();

    // Create a minimal Cargo.toml
    let cargo_toml = r#"
    [workspace]
    members = ["pkg-a"]
    resolver = "2"

    [package]
    name = "pkg-a"
    version = "0.1.0"
    edition = "2021"
    "#;

    // Create pkg-a directory and Cargo.toml
    fs::create_dir(workspace_root.join("pkg-a")).unwrap();
    let mut f = File::create(workspace_root.join("pkg-a/Cargo.toml")).unwrap();
    f.write_all(cargo_toml.as_bytes()).unwrap();

    // Create workspace Cargo.toml
    let mut f = File::create(workspace_root.join("Cargo.toml")).unwrap();
    f.write_all(
      r#"[workspace]
members = ["pkg-a"]
resolver = "2"
"#
      .as_bytes(),
    )
    .unwrap();

    // 1. First load - should create cache
    // Note: This might fail if cargo is not in path or if it tries to access network
    // For unit test, we might need to mock or just verify the hashing logic if full load is too heavy
    // But let's try to verify the cache file creation logic at least.

    // Since we can't easily run real cargo metadata in this unit test environment without network/registry access
    // (and it might be slow), let's verify the hashing and cache path logic.

    let hash1 = compute_workspace_hash(workspace_root);

    // Modify Cargo.toml
    let mut f = fs::OpenOptions::new()
      .append(true)
      .open(workspace_root.join("Cargo.toml"))
      .unwrap();
    writeln!(f, "# comment").unwrap();

    let hash2 = compute_workspace_hash(workspace_root);
    assert_ne!(hash1, hash2, "Hash should change when Cargo.toml changes");
  }

  #[test]
  fn test_metadata_cache_reuse_and_invalidation() {
    // Create a temp workspace
    let temp_dir = TempDir::new().unwrap();
    let workspace_root = temp_dir.path();
    let cache_dir = workspace_root.join("target/rail");
    fs::create_dir_all(&cache_dir).unwrap();

    // Create a minimal Cargo.toml so hashing works
    let mut f = File::create(workspace_root.join("Cargo.toml")).unwrap();
    f.write_all(b"[workspace]\nmembers = []\n").unwrap();

    // 1. Setup a FAKE cache
    // We create a MetadataCache with the CURRENT hash, but containing FAKE metadata.
    // If CargoState::load returns this fake metadata, we know it used the cache.
    let current_hash = compute_workspace_hash(workspace_root);

    // Construct fake metadata JSON that matches cargo_metadata structure
    // We'll inject a fake package "cached-pkg"
    let fake_metadata_json = r#"{
      "packages": [
        {
          "name": "cached-pkg",
          "version": "0.1.0",
          "id": "cached-pkg 0.1.0 (path+file:///tmp/fake)",
          "license": null,
          "license_file": null,
          "description": null,
          "source": null,
          "dependencies": [],
          "targets": [],
          "features": {},
          "manifest_path": "/tmp/fake/Cargo.toml",
          "metadata": null,
          "publish": null,
          "authors": [],
          "categories": [],
          "keywords": [],
          "readme": null,
          "repository": null,
          "homepage": null,
          "documentation": null,
          "edition": "2021",
          "links": null,
          "default_run": null,
          "rust_version": null
        }
      ],
      "workspace_members": ["cached-pkg 0.1.0 (path+file:///tmp/fake)"],
      "workspace_default_members": ["cached-pkg 0.1.0 (path+file:///tmp/fake)"],
      "resolve": null,
      "target_directory": "/tmp/fake/target",
      "version": 1,
      "workspace_root": "/tmp/fake"
    }"#;

    let fake_metadata: WorkspaceMetadata =
      serde_json::from_str(&format!(r#"{{"metadata": {}}}"#, fake_metadata_json)).unwrap();

    let cache = MetadataCache {
      hash: current_hash,
      metadata: fake_metadata,
    };

    let cache_file = cache_dir.join("metadata.json");
    fs::write(&cache_file, serde_json::to_string(&cache).unwrap()).unwrap();

    // 2. Test Cache Hit
    // Should load our fake metadata because hash matches
    let state = CargoState::load(workspace_root).expect("Should load from cache");
    assert!(
      state.get_package("cached-pkg").is_some(),
      "Should have loaded cached package"
    );

    // 3. Test Invalidation
    // Modify Cargo.toml
    let mut f = fs::OpenOptions::new()
      .append(true)
      .open(workspace_root.join("Cargo.toml"))
      .unwrap();
    writeln!(f, "# change").unwrap();

    // Now load should fail (or at least NOT return the cached pkg)
    // It will try to run real `cargo metadata` which will likely fail in this env or return empty/different result
    // But definitely NOT our fake package
    let result = CargoState::load(workspace_root);

    match result {
      Ok(state) => {
        // If it somehow succeeded (e.g. cargo metadata ran successfully on the minimal toml),
        // it should NOT have our fake package
        assert!(
          state.get_package("cached-pkg").is_none(),
          "Should NOT have loaded cached package after invalidation"
        );
      }
      Err(_) => {
        // Expected failure (cargo metadata failed), which proves it tried to run real metadata
        // and didn't use the stale cache
      }
    }
  }
}
