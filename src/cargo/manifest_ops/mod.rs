//! Low-level TOML operations for Cargo manifests
//!
//! This module provides reusable, test-friendly operations for manipulating
//! Cargo.toml documents at a low level. Both ManifestWriter and CargoTransform
//! use these functions to avoid duplicating complex TOML manipulation logic.
//!
//! ## Organization
//!
//! - **File I/O** - read/write TOML files with consistent error handling
//! - **Dependency Entry Building** - create entries for dependencies
//! - **Workspace Reference Handling** - detect/manipulate workspace = true
//! - **TOML Navigation** - get/create sections and tables
//! - **Feature/Path/Defaults** - manipulate dependency fields
//! - **Bulk Operations** - transform multiple dependencies at once

mod entry_builder;
mod fields;
mod io;
mod navigation;
mod transform;
mod workspace_ref;

// Re-export all public functions to maintain the same API

// File I/O
pub use io::{read_toml_file, write_toml_file};

// Dependency Entry Building
pub use entry_builder::{
  build_dep_entry, build_transitive_entry, build_versioned_dep_entry, build_workspace_dep_entry,
};

// Workspace Reference Detection
pub use workspace_ref::{extract_workspace_marker, is_package_workspace_inherited, is_workspace_dep};

// TOML Section Navigation & Target-Specific Dependencies
pub use navigation::{
  ensure_section, get_dependencies, get_or_create_table, get_table, insert_dependency, insert_target_dependency,
  remove_dependency, remove_target_dependency,
};

// Feature Array Operations
pub use fields::{build_feature_array, extract_features, remove_features, set_features};

// Path Dependency Operations
pub use fields::{extract_path, has_path, remove_path, set_path};

// Default Features Operations
pub use fields::{has_default_features, remove_default_features, set_default_features};

// Batch Transformation Operations
pub use transform::{resolve_package_workspace_inheritance, transform_dependencies_in_section};

// Validation & Inspection
pub use transform::{
  collect_all_dep_sections, dep_kind_to_section, extract_version, is_inline_table_dep, is_optional,
  is_simple_string_dep, is_table_dep, set_optional, set_version,
};
