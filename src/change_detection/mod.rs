//! Canonical change detection.
//!
//! The planner taxonomy in `classify` is the source of truth for every
//! higher-level layer:
//!
//! - `classify` provides the canonical path profile plus planner
//!   `kind` / `sub_kind` projection and configured pattern helpers.
//! - [`workspace::change_analyzer`](crate::workspace::change_analyzer)
//!   buckets those canonical profiles into graph-aware impact reports.
//! - `presentation` applies configured infrastructure and custom-pattern
//!   overlays without re-implementing path heuristics.
//!
//! # Usage
//!
//! ```text
//! // Canonical path profile / planner taxonomy
//! let profile = classify_path(path);
//!
//! // Layer 2: Full impact analysis (requires workspace context)
//! let impact = ChangeImpact::new(&ctx).analyze_changes("main", None)?;
//!
//! // Layer 3: Presentation helpers
//! let classifier = ChangeClassifier::new(&config.change_detection);
//! let classification = classifier.classify(&changed_files);
//! ```

pub mod classify;
pub mod presentation;

// Canonical path classification types
pub use classify::{ChangeKind, ConfigKind, FileProfile, TestKind, classify_file, classify_path};

// Layer 3: Presentation classification
pub use presentation::{ChangeClassification, ChangeClassifier};
