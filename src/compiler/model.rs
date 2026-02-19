//! Compiler diagnostics cache model for target-aware source-unused detection.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Cache format version for compiler diagnostics.
pub const COMPILER_DIAG_CACHE_VERSION: u32 = 1;

/// Collector version used to invalidate stale semantic behavior.
pub const COLLECTOR_VERSION: u32 = 1;

/// Stable key for a member+target diagnostics entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CompilerDiagKey {
  /// Workspace package name.
  pub member: String,
  /// Target triple or `default` when no explicit target was used.
  pub target_triple: String,
  /// rustc semantic version string.
  pub rustc_version: String,
  /// rustc host triple.
  pub host_triple: String,
  /// Workspace lockfile fingerprint.
  pub lock_fingerprint: String,
  /// Member manifest fingerprint.
  pub manifest_fingerprint: String,
  /// Member source-tree fingerprint.
  pub source_fingerprint: String,
  /// Build-affecting environment fingerprint (RUSTFLAGS, CARGO_ENCODED_RUSTFLAGS).
  pub compiler_env_fingerprint: String,
  /// Workspace cargo config fingerprint (`.cargo/config.toml` and `.cargo/config`).
  pub cargo_config_fingerprint: String,
}

impl CompilerDiagKey {
  /// Deterministic string identifier for map storage.
  #[must_use]
  pub fn stable_id(&self) -> String {
    format!(
      "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
      self.member,
      self.target_triple,
      self.rustc_version,
      self.host_triple,
      self.lock_fingerprint,
      self.manifest_fingerprint,
      self.source_fingerprint,
      self.compiler_env_fingerprint,
      self.cargo_config_fingerprint
    )
  }
}

/// Completeness of compiler diagnostics for a member+target run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagnosticsCompleteness {
  /// Cargo check finished successfully and diagnostics are complete.
  Complete,
  /// Cargo check failed; diagnostics are partial and must not drive auto-removal.
  Incomplete,
}

/// Cached diagnostics payload for one member+target pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompilerDiagEntry {
  /// Key used for cache lookup.
  pub key: CompilerDiagKey,
  /// Dependencies warned by rustc as package-unused for this target.
  pub unused_deps: BTreeSet<String>,
  /// Crate targets that were compiled for this package.
  pub compiled_crate_targets: BTreeSet<String>,
  /// Timestamp when collected.
  pub generated_at_unix_ms: u64,
  /// Collector semantic version.
  pub collector_version: u32,
  /// Whether diagnostics were complete for this run.
  pub completeness: DiagnosticsCompleteness,
}

/// On-disk cache envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompilerDiagCacheFile {
  /// Cache format version.
  pub version: u32,
  /// Entries keyed by [`CompilerDiagKey::stable_id`].
  pub entries: std::collections::BTreeMap<String, CompilerDiagEntry>,
}

impl Default for CompilerDiagCacheFile {
  fn default() -> Self {
    Self {
      version: COMPILER_DIAG_CACHE_VERSION,
      entries: std::collections::BTreeMap::new(),
    }
  }
}
