//! Quality analysis and enforcement module
//!
//! Provides unified quality checks to replace cargo-deny, cargo-audit, cargo-udeps, and git-cliff.
//! Philosophy: Supply-chain safe, deterministic, minimal dependencies.

pub mod changelog;
