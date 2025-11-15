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
//! ## Current Modules
//! - **changelog**: Conventional commit parsing and changelog generation (replaces git-cliff)
//!
//! ## Planned Modules (Phase 3)
//! - **unused_deps**: Detect unused dependencies (replaces cargo-udeps)
//! - **duplicates**: Find duplicate dependency versions (replaces cargo-deny)
//! - **security**: Security advisory checks (replaces cargo-audit)

pub mod changelog;
