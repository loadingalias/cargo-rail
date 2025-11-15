//! Quality analysis and enforcement (Pillar 5)
//!
//! Unified quality suite to replace cargo-deny, cargo-audit, cargo-udeps, and git-cliff.
//!
//! ## Philosophy: Supply-Chain Safe
//! - Deterministic analysis with minimal external dependencies
//! - Single dependency graph build, multiple analyses
//! - Zero-panic parsers using winnow (not regex)
//! - No network calls, no telemetry
//!
//! ## Architecture
//!
//! The quality engine uses an internal trait-based plugin system:
//! - `QualityAnalysis` trait: All analyses implement this
//! - `QualityEngine`: Orchestrates running analyses over shared context
//! - `QualityContext`: Single build of graph + metadata, passed to all analyses
//!
//! All analyses are compiled into the binary (no external plugins).
//!
//! ## Current Modules
//! - **changelog**: Conventional commit parsing and changelog generation (replaces git-cliff)
//! - **duplicates**: Find duplicate dependency versions (replaces cargo-deny)
//! - **unused_deps**: Detect unused dependencies (replaces cargo-udeps) - placeholder
//!
//! ## Planned Modules (Future)
//! - **security**: Security advisory checks (replaces cargo-audit)

pub mod changelog;
pub mod duplicates;
pub mod engine;
pub mod unused_deps;

// Re-export public API
pub use engine::{QualityContext, QualityEngine, QualityReport, Severity};

use std::sync::Arc;

/// Create a quality engine with all built-in analyses registered
pub fn create_default_engine() -> QualityEngine {
  let mut engine = QualityEngine::new();

  // Register all analyses
  engine.register(Arc::new(duplicates::DuplicateVersionsAnalysis));
  engine.register(Arc::new(unused_deps::UnusedDepsAnalysis));

  engine
}
