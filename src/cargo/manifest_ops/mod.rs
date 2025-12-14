//! Low-level TOML operations for Cargo manifests
//!
//! This module provides reusable, test-friendly operations for manipulating
//! Cargo.toml documents at a low level.

mod entry_builder;
mod fields;
mod io;
mod navigation;
mod transform;
mod workspace_ref;

// File I/O
pub use io::{read_toml_file, write_toml_file};

// Dependency Entry Building
pub use entry_builder::{
  build_dep_entry, build_transitive_entry, build_versioned_dep_entry, build_workspace_dep_entry,
};

// Workspace Reference Detection
pub use workspace_ref::{extract_workspace_marker, is_workspace_dep};

// TOML Section Navigation & Target-Specific Dependencies
pub use navigation::{
  ensure_section, get_or_create_table, insert_dependency, insert_target_dependency, remove_target_dependency,
};

// Feature Operations
pub use fields::{build_feature_array, extract_features, remove_path, set_features};

// Batch Transformation Operations
pub use transform::{
  dep_kind_to_section, resolve_package_workspace_inheritance, set_version, transform_dependencies_in_section,
};
