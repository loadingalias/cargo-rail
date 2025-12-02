//! Advanced change detection and classification
//!
//! This module provides intelligent file change classification with a layered architecture:
//!
//! - **Layer 1: `classify`** - Pure file classification by path (fast, no I/O)
//!   - `ChangeKind`: Source, Test, Example, BuildScript, Config, Documentation, Other
//!
//! - **Layer 2: `workspace::change_analyzer`** - Graph + cargo aware impact analysis
//!   - `ImpactReport`: Direct crates, transitive dependents, rebuild requirements
//!   - Proc-macro detection via cached workspace state
//!
//! - **Layer 3: `presentation`** - User-configurable presentation choices
//!   - `ChangeClassification`: docs-only, rebuild-all, custom categories
//!   - Driven by `ChangeDetectionConfig` from rail.toml
//!
//! # Usage
//!
//! ```ignore
//! // Layer 1: Quick file classification
//! let kind = classify_file(path);
//!
//! // Layer 2: Full impact analysis (requires workspace context)
//! let impact = ChangeImpact::new(&ctx).analyze_changes("main", None)?;
//!
//! // Layer 3: Presentation classification
//! let classifier = ChangeClassifier::new(&config.change_detection);
//! let classification = classifier.classify(&changed_files);
//! ```

pub mod classify;
pub mod presentation;

// Layer 1: File classification types
pub use classify::{ChangeKind, ConfigKind, TestKind};

// Layer 3: Presentation classification
pub use presentation::{ChangeClassification, ChangeClassifier};
