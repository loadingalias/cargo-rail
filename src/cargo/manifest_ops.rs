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

use crate::cargo::manifest_analyzer::DepKind;
use crate::cargo::unify_analyzer::UnifiedDep;
use crate::error::{RailError, RailResult, ResultExt};
use std::path::Path;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value};

// ============================================================================
// SECTION 0: File I/O Operations
// ============================================================================

/// Read and parse a TOML file into a DocumentMut
///
/// Provides consistent error handling for reading and parsing Cargo.toml files.
///
/// # Arguments
///
/// * `path` - Path to the TOML file
///
/// # Returns
///
/// The parsed TOML document
///
/// # Errors
///
/// Returns error if file cannot be read or TOML is invalid
pub fn read_toml_file(path: &Path) -> RailResult<DocumentMut> {
  let content = std::fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;

  content
    .parse()
    .with_context(|| format!("Failed to parse {}", path.display()))
}

/// Write a TOML document to a file
///
/// Provides consistent error handling for writing Cargo.toml files.
///
/// # Arguments
///
/// * `path` - Path to write to
/// * `doc` - The TOML document to write
///
/// # Errors
///
/// Returns error if file cannot be written
pub fn write_toml_file(path: &Path, doc: &DocumentMut) -> RailResult<()> {
  std::fs::write(path, doc.to_string()).with_context(|| format!("Failed to write {}", path.display()))
}

// ============================================================================
// SECTION 1: Dependency Entry Building
// ============================================================================

/// Build a dependency entry for [workspace.dependencies]
///
/// Returns:
/// - `Item::Value(Value::String)` for simple case (version only)
/// - `Item::Value(Value::InlineTable)` for complex case (features, path, etc.)
///
/// # Examples
///
/// ```ignore
/// let dep = UnifiedDep {
///     name: "serde".to_string(),
///     version_req: "1.0".parse().unwrap(),
///     features: vec![],
///     default_features: true,
///     used_by: vec!["crate1".to_string()],
///     target: None,
///     path: None,
/// };
/// let entry = build_dep_entry(&dep);
/// // Result: Item::Value(Value::String("1.0"))
/// ```
pub fn build_dep_entry(dep: &UnifiedDep) -> Item {
  // Simple case: just version, no features, defaults enabled, no path
  if dep.features.is_empty() && dep.default_features && dep.path.is_none() {
    let mut value = Value::from(dep.version_req.to_string());
    value.decor_mut().set_suffix(" #unified");
    return Item::Value(value);
  }

  // Complex case: use inline table
  let mut table = InlineTable::new();

  // Add path if present (for workspace member deps)
  if let Some(ref path) = dep.path {
    table.insert("path", Value::from(path.display().to_string()));
    // Also include version for publishable workspace members
    // This allows `cargo publish` to work while using paths for local dev
    if dep.version_req.to_string() != "*" {
      table.insert("version", Value::from(dep.version_req.to_string()));
    }
  } else {
    table.insert("version", Value::from(dep.version_req.to_string()));
  }

  // Add default-features flag if false
  if !dep.default_features {
    table.insert("default-features", Value::from(false));
  }

  // Add features if any
  if !dep.features.is_empty() {
    table.insert("features", build_feature_array(&dep.features));
  }

  // Add #unified comment marker
  let mut value = Value::InlineTable(table);
  value.decor_mut().set_suffix(" #unified");

  Item::Value(value)
}

/// Build a workspace-inherited dependency entry
///
/// Used in member manifests to reference [workspace.dependencies].
///
/// # Arguments
///
/// * `local_features` - Additional features to enable locally (beyond workspace features)
/// * `is_optional` - Whether the dependency is optional
///
/// # Returns
///
/// `{ workspace = true }` with optional fields and `#unified` comment marker
pub fn build_workspace_dep_entry(local_features: Option<Vec<String>>, is_optional: bool) -> Item {
  let mut table = InlineTable::new();
  table.insert("workspace", Value::from(true));

  // Add local features if any
  if let Some(features) = local_features
    && !features.is_empty()
  {
    table.insert("features", build_feature_array(&features));
  }

  // Add optional if needed
  if is_optional {
    table.insert("optional", Value::from(true));
  }

  // Add #unified comment marker to track what was modified by unify
  let mut value = Value::InlineTable(table);
  value.decor_mut().set_suffix(" #unified");

  Item::Value(value)
}

/// Build a transitive dependency entry for pinning
///
/// Used for workspace-hack replacement (dev-dependencies with workspace = true and features).
///
/// # Arguments
///
/// * `features` - Features to enable for the transitive dependency
pub fn build_transitive_entry(features: &[String]) -> Item {
  let mut table = InlineTable::new();
  table.insert("workspace", Value::from(true));

  if !features.is_empty() {
    table.insert("features", build_feature_array(features));
  }

  // Add #unified comment marker
  let mut value = Value::InlineTable(table);
  value.decor_mut().set_suffix(" #unified");

  Item::Value(value)
}

/// Build a versioned dependency entry for [workspace.dependencies]
///
/// Used when adding transitive dependencies to workspace.dependencies.
/// Creates entries like: `dep = { version = "1.0", features = ["foo"] }`
///
/// # Arguments
///
/// * `version` - The semver version to use
/// * `features` - Features to enable
pub fn build_versioned_dep_entry(version: &semver::Version, features: &[String]) -> Item {
  // Simple case: just version, no features
  if features.is_empty() {
    let mut value = Value::from(format!("^{}", version));
    value.decor_mut().set_suffix(" #unified");
    return Item::Value(value);
  }

  // Complex case: version + features
  let mut table = InlineTable::new();
  table.insert("version", Value::from(format!("^{}", version)));
  table.insert("features", build_feature_array(features));

  // Add #unified comment marker
  let mut value = Value::InlineTable(table);
  value.decor_mut().set_suffix(" #unified");

  Item::Value(value)
}

// ============================================================================
// SECTION 2: Workspace Reference Detection
// ============================================================================

/// Check if dependency entry uses workspace = true
///
/// Works for both inline tables and regular tables.
///
/// # Returns
///
/// `true` if the dependency has `workspace = true`, `false` otherwise
pub fn is_workspace_dep(item: &Item) -> bool {
  if let Some(table) = item.as_inline_table() {
    table.get("workspace").and_then(|v| v.as_bool()).unwrap_or(false)
  } else if let Some(table) = item.as_table() {
    table.get("workspace").and_then(|v| v.as_bool()).unwrap_or(false)
  } else {
    false
  }
}

/// Check if package field is workspace-inherited
///
/// Same logic as is_workspace_dep, just for clarity in package context.
pub fn is_package_workspace_inherited(item: &Item) -> bool {
  is_workspace_dep(item)
}

/// Extract and remove workspace = true marker from dependency
///
/// # Returns
///
/// `true` if workspace marker was present and removed, `false` otherwise
pub fn extract_workspace_marker(item: &mut Item) -> bool {
  if let Some(table) = item.as_inline_table_mut() {
    table.remove("workspace").is_some()
  } else if let Some(table) = item.as_table_mut() {
    table.remove("workspace").is_some()
  } else {
    false
  }
}

// ============================================================================
// SECTION 3: TOML Section Navigation
// ============================================================================

/// Ensure a section exists, creating if necessary
///
/// # Arguments
///
/// * `doc` - The TOML document
/// * `path` - Dotted path: "workspace" or "workspace.dependencies" or "dependencies"
///
/// # Errors
///
/// Returns error if a non-table exists at the path or if parent is not a table
pub fn ensure_section(doc: &mut DocumentMut, path: &str) -> RailResult<()> {
  let parts: Vec<&str> = path.split('.').collect();
  if parts.is_empty() {
    return Err(RailError::message("Empty path provided"));
  }

  let mut current = doc.as_item_mut();

  for part in parts {
    if let Some(table) = current.as_table_mut() {
      if !table.contains_key(part) {
        table.insert(part, Item::Table(Table::new()));
      }
      current = table
        .get_mut(part)
        .ok_or_else(|| RailError::message(format!("Failed to access {}", part)))?;
    } else {
      return Err(RailError::message(
        "Cannot create section: expected table but found non-table",
      ));
    }
  }

  Ok(())
}

/// Get or create a table at dotted path
///
/// Creates intermediate tables as needed.
///
/// # Arguments
///
/// * `doc` - The TOML document
/// * `path` - Dotted path: "workspace.dependencies"
///
/// # Returns
///
/// Mutable reference to the table at the path
///
/// # Errors
///
/// Returns error if path is invalid or final item is not a table
pub fn get_or_create_table<'a>(doc: &'a mut DocumentMut, path: &str) -> RailResult<&'a mut Table> {
  let parts: Vec<&str> = path.split('.').collect();
  if parts.is_empty() {
    return Err(RailError::message("Empty path provided"));
  }

  let mut current = doc.as_item_mut();

  for part in &parts {
    if let Some(table) = current.as_table_mut() {
      if !table.contains_key(part) {
        table.insert(part, Item::Table(Table::new()));
      }
      current = table
        .get_mut(part)
        .ok_or_else(|| RailError::message(format!("Failed to access {}", part)))?;
    } else {
      return Err(RailError::message(format!(
        "Cannot navigate to {}: parent is not a table",
        part
      )));
    }
  }

  current
    .as_table_mut()
    .ok_or_else(|| RailError::message(format!("Final item at path '{}' is not a table", path)))
}

/// Get a table reference (non-mutable)
pub fn get_table<'a>(doc: &'a DocumentMut, path: &str) -> Option<&'a Table> {
  let parts: Vec<&str> = path.split('.').collect();
  let mut current = doc.as_item();

  for part in parts {
    if let Some(table) = current.as_table() {
      current = table.get(part)?;
    } else {
      return None;
    }
  }

  current.as_table()
}

/// Insert dependency into a section
///
/// # Arguments
///
/// * `section` - The dependencies section (mutable)
/// * `name` - Dependency name
/// * `entry` - Dependency entry (from build_dep_entry or similar)
pub fn insert_dependency(section: &mut Table, name: &str, entry: Item) -> RailResult<()> {
  section.insert(name, entry);
  Ok(())
}

/// Remove dependency from a section
///
/// # Returns
///
/// `true` if dependency was present and removed, `false` if not present
pub fn remove_dependency(section: &mut Table, name: &str) -> bool {
  section.remove(name).is_some()
}

/// Get all dependencies from a section
///
/// # Returns
///
/// Vector of (name, item) tuples
pub fn get_dependencies(section: &Table) -> Vec<(String, &Item)> {
  section.iter().map(|(k, v)| (k.to_string(), v)).collect()
}

// ============================================================================
// SECTION 4: Target-Specific Dependencies
// ============================================================================

/// Add target-specific dependency
///
/// Ensures [target.'cfg(...)'.SECTION] exists and inserts dependency
///
/// # Arguments
///
/// * `doc` - The TOML document
/// * `target` - Target configuration (e.g., "cfg(unix)")
/// * `section` - Section name (e.g., "dependencies", "dev-dependencies")
/// * `name` - Dependency name
/// * `entry` - Dependency entry
pub fn insert_target_dependency(
  doc: &mut DocumentMut,
  target: &str,
  section: &str,
  name: &str,
  entry: Item,
) -> RailResult<()> {
  let target_section = get_target_section_mut(doc, target, section)?;
  target_section.insert(name, entry);
  Ok(())
}

/// Remove a dependency from target-specific section
///
/// # Arguments
///
/// * `doc` - TOML document to modify
/// * `target` - Target triple or cfg expression (e.g., "cfg(windows)")
/// * `section` - Section name ("dependencies", "dev-dependencies", "build-dependencies")
/// * `name` - Dependency name to remove
pub fn remove_target_dependency(doc: &mut DocumentMut, target: &str, section: &str, name: &str) -> RailResult<()> {
  let path = format!("target.{}.{}", target, section);
  if let Some(target_section) = get_table_mut(doc, &path) {
    target_section.remove(name);
  }
  Ok(())
}

/// Get target-specific dependencies section (mutable)
fn get_target_section_mut<'a>(doc: &'a mut DocumentMut, target: &str, section: &str) -> RailResult<&'a mut Table> {
  let path = format!("target.{}.{}", target, section);
  get_or_create_table(doc, &path)
}

/// Get a mutable reference to a table by path (returns None if not found)
fn get_table_mut<'a>(doc: &'a mut DocumentMut, path: &str) -> Option<&'a mut Table> {
  let parts: Vec<&str> = path.split('.').collect();
  let mut current: &mut Item = doc.as_item_mut();

  for part in parts {
    current = current.get_mut(part)?;
  }

  current.as_table_mut()
}

// ============================================================================
// SECTION 5: Feature Array Operations
// ============================================================================

/// Build feature array from strings
///
/// # Arguments
///
/// * `features` - List of feature names
///
/// # Returns
///
/// `Value::Array` containing the features
pub fn build_feature_array(features: &[String]) -> Value {
  let mut array = Array::new();
  for feature in features {
    array.push(feature.as_str());
  }
  Value::from(array)
}

/// Extract features from dependency entry
///
/// # Returns
///
/// `Some(features)` if present, `None` otherwise
pub fn extract_features(item: &Item) -> Option<Vec<String>> {
  if let Some(table) = item.as_inline_table() {
    extract_features_from_inline_table(table)
  } else if let Some(table) = item.as_table() {
    extract_features_from_table(table)
  } else {
    None
  }
}

fn extract_features_from_inline_table(table: &InlineTable) -> Option<Vec<String>> {
  table
    .get("features")
    .and_then(|v| v.as_array())
    .map(|arr| arr.iter().filter_map(|v| v.as_str()).map(|s| s.to_string()).collect())
}

fn extract_features_from_table(table: &Table) -> Option<Vec<String>> {
  table
    .get("features")
    .and_then(|item| item.as_value())
    .and_then(|v| v.as_array())
    .map(|arr| arr.iter().filter_map(|v| v.as_str()).map(|s| s.to_string()).collect())
}

/// Update or add features in dependency entry
pub fn set_features(item: &mut Item, features: Vec<String>) -> RailResult<()> {
  if let Some(table) = item.as_inline_table_mut() {
    table.insert("features", build_feature_array(&features));
    Ok(())
  } else if let Some(table) = item.as_table_mut() {
    table.insert("features", Item::Value(build_feature_array(&features)));
    Ok(())
  } else {
    Err(RailError::message("Cannot set features: item is not a table"))
  }
}

/// Remove features field from dependency entry
///
/// # Returns
///
/// `true` if features were present and removed, `false` otherwise
pub fn remove_features(item: &mut Item) -> bool {
  if let Some(table) = item.as_inline_table_mut() {
    table.remove("features").is_some()
  } else if let Some(table) = item.as_table_mut() {
    table.remove("features").is_some()
  } else {
    false
  }
}

// ============================================================================
// SECTION 6: Path Dependency Operations
// ============================================================================

/// Check if dependency has path field
pub fn has_path(item: &Item) -> bool {
  if let Some(table) = item.as_inline_table() {
    table.contains_key("path")
  } else if let Some(table) = item.as_table() {
    table.contains_key("path")
  } else {
    false
  }
}

/// Extract path value from dependency
pub fn extract_path(item: &Item) -> Option<String> {
  if let Some(table) = item.as_inline_table() {
    table.get("path").and_then(|v| v.as_str()).map(String::from)
  } else if let Some(table) = item.as_table() {
    table
      .get("path")
      .and_then(|item| item.as_value())
      .and_then(|v| v.as_str())
      .map(String::from)
  } else {
    None
  }
}

/// Remove path field from dependency
///
/// # Returns
///
/// `true` if path was present and removed, `false` otherwise
pub fn remove_path(item: &mut Item) -> bool {
  if let Some(table) = item.as_inline_table_mut() {
    table.remove("path").is_some()
  } else if let Some(table) = item.as_table_mut() {
    table.remove("path").is_some()
  } else {
    false
  }
}

/// Set path field on dependency
pub fn set_path(item: &mut Item, path: &str) -> RailResult<()> {
  if let Some(table) = item.as_inline_table_mut() {
    table.insert("path", Value::from(path));
    Ok(())
  } else if let Some(table) = item.as_table_mut() {
    table.insert("path", Item::Value(Value::from(path)));
    Ok(())
  } else {
    Err(RailError::message("Cannot set path: item is not a table"))
  }
}

// ============================================================================
// SECTION 7: Default Features Operations
// ============================================================================

/// Check default-features setting
///
/// Returns `true` if not specified (default) or explicitly set to true
pub fn has_default_features(item: &Item) -> bool {
  if let Some(table) = item.as_inline_table() {
    table.get("default-features").and_then(|v| v.as_bool()).unwrap_or(true)
  } else if let Some(table) = item.as_table() {
    table
      .get("default-features")
      .and_then(|item| item.as_value())
      .and_then(|v| v.as_bool())
      .unwrap_or(true)
  } else {
    true
  }
}

/// Set default-features flag
pub fn set_default_features(item: &mut Item, enabled: bool) -> RailResult<()> {
  if let Some(table) = item.as_inline_table_mut() {
    table.insert("default-features", Value::from(enabled));
    Ok(())
  } else if let Some(table) = item.as_table_mut() {
    table.insert("default-features", Item::Value(Value::from(enabled)));
    Ok(())
  } else {
    Err(RailError::message("Cannot set default-features: item is not a table"))
  }
}

/// Remove default-features field
pub fn remove_default_features(item: &mut Item) -> bool {
  if let Some(table) = item.as_inline_table_mut() {
    table.remove("default-features").is_some()
  } else if let Some(table) = item.as_table_mut() {
    table.remove("default-features").is_some()
  } else {
    false
  }
}

// ============================================================================
// SECTION 8: Batch Transformation Operations
// ============================================================================

/// Transform all dependencies in a section
///
/// Calls transform_fn for each dependency in the section, allowing custom mutations.
///
/// # Arguments
///
/// * `doc` - The TOML document
/// * `section` - Section name (e.g., "dependencies")
/// * `transform_fn` - Closure to transform each dependency
pub fn transform_dependencies_in_section<F>(doc: &mut DocumentMut, section: &str, mut transform_fn: F) -> RailResult<()>
where
  F: FnMut(&str, &mut Item) -> RailResult<()>,
{
  if let Some(deps) = doc.get_mut(section).and_then(|d| d.as_table_mut()) {
    let dep_names: Vec<String> = deps.iter().map(|(k, _)| k.to_string()).collect();

    for dep_name in dep_names {
      if let Some(dep_item) = deps.get_mut(&dep_name) {
        transform_fn(&dep_name, dep_item)?;
      }
    }
  }

  Ok(())
}

/// Resolve workspace references in package fields
///
/// Replaces `{ workspace = true }` with actual values from workspace.package
pub fn resolve_package_workspace_inheritance(doc: &mut DocumentMut, workspace_package: &Table) -> RailResult<()> {
  let fields = [
    "version",
    "authors",
    "edition",
    "license",
    "repository",
    "description",
    "homepage",
    "documentation",
    "readme",
    "keywords",
    "categories",
  ];

  if let Some(package) = doc.get_mut("package").and_then(|p| p.as_table_mut()) {
    for field in fields {
      if let Some(item) = package.get(field)
        && is_package_workspace_inherited(item)
      {
        // Get value from workspace.package
        if let Some(workspace_value) = workspace_package.get(field) {
          package.insert(field, workspace_value.clone());
        } else {
          package.remove(field);
        }
      }
    }

    // Remove workspace reference from package itself
    package.remove("workspace");
  }

  Ok(())
}

// ============================================================================
// SECTION 9: Validation & Inspection
// ============================================================================

/// Collect all dependency sections in a manifest
pub fn collect_all_dep_sections(doc: &DocumentMut) -> Vec<(String, Vec<(String, &Item)>)> {
  let mut sections = Vec::new();

  for section_name in ["dependencies", "dev-dependencies", "build-dependencies"] {
    if let Some(table) = get_table(doc, section_name) {
      let deps = get_dependencies(table);
      if !deps.is_empty() {
        sections.push((section_name.to_string(), deps));
      }
    }
  }

  sections
}

/// Check if dependency entry is "simple" (just version string)
pub fn is_simple_string_dep(item: &Item) -> bool {
  item.as_value().and_then(|v| v.as_str()).is_some()
}

/// Check if dependency entry is an inline table
pub fn is_inline_table_dep(item: &Item) -> bool {
  item.as_inline_table().is_some()
}

/// Check if dependency entry is a full table (not inline)
pub fn is_table_dep(item: &Item) -> bool {
  item.as_table().is_some()
}

/// Get version from a dependency entry (any format)
///
/// # Returns
///
/// `Some(version)` if version field exists, `None` otherwise
pub fn extract_version(item: &Item) -> Option<String> {
  // Simple string format: dep = "1.0"
  if let Some(s) = item.as_str() {
    return Some(s.to_string());
  }

  // Table format: dep = { version = "1.0", ... }
  if let Some(table) = item.as_inline_table() {
    return table.get("version").and_then(|v| v.as_str()).map(String::from);
  }

  if let Some(table) = item.as_table() {
    return table
      .get("version")
      .and_then(|item| item.as_value())
      .and_then(|v| v.as_str())
      .map(String::from);
  }

  None
}

/// Set version on a dependency entry
///
/// Converts simple string deps to inline tables if needed
pub fn set_version(item: &mut Item, version: &str) -> RailResult<()> {
  // If it's a simple string, replace the entire item
  if item.as_str().is_some() {
    *item = Item::Value(Value::from(version));
    return Ok(());
  }

  // If it's a table, update the version field
  if let Some(table) = item.as_inline_table_mut() {
    table.insert("version", Value::from(version));
    Ok(())
  } else if let Some(table) = item.as_table_mut() {
    table.insert("version", Item::Value(Value::from(version)));
    Ok(())
  } else {
    Err(RailError::message(
      "Cannot set version: item is not a valid dependency format",
    ))
  }
}

/// Check if dependency entry has optional flag set
pub fn is_optional(item: &Item) -> bool {
  if let Some(table) = item.as_inline_table() {
    table.get("optional").and_then(|v| v.as_bool()).unwrap_or(false)
  } else if let Some(table) = item.as_table() {
    table
      .get("optional")
      .and_then(|item| item.as_value())
      .and_then(|v| v.as_bool())
      .unwrap_or(false)
  } else {
    false
  }
}

/// Set optional flag on dependency
pub fn set_optional(item: &mut Item, optional: bool) -> RailResult<()> {
  if let Some(table) = item.as_inline_table_mut() {
    table.insert("optional", Value::from(optional));
    Ok(())
  } else if let Some(table) = item.as_table_mut() {
    table.insert("optional", Item::Value(Value::from(optional)));
    Ok(())
  } else {
    Err(RailError::message("Cannot set optional: item is not a table"))
  }
}

/// Convert DepKind to section name string
pub fn dep_kind_to_section(kind: DepKind) -> &'static str {
  match kind {
    DepKind::Normal => "dependencies",
    DepKind::Dev => "dev-dependencies",
    DepKind::Build => "build-dependencies",
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::PathBuf;

  fn create_test_dep(name: &str, version: &str) -> UnifiedDep {
    UnifiedDep {
      name: name.to_string(),
      version_req: version.parse().unwrap(),
      features: vec![],
      default_features: true,
      used_by: vec!["test-crate".to_string()],
      target: None,
      path: None,
    }
  }

  // ========================================================================
  // Section 1: Dependency Entry Building Tests
  // ========================================================================

  #[test]
  fn test_build_dep_entry_simple() {
    let dep = create_test_dep("serde", "1.0");
    let entry = build_dep_entry(&dep);
    assert!(is_simple_string_dep(&entry));
    // Version requirement parsing adds the caret operator
    assert_eq!(extract_version(&entry).unwrap(), "^1.0");
  }

  #[test]
  fn test_build_dep_entry_with_features() {
    let mut dep = create_test_dep("serde", "1.0");
    dep.features = vec!["derive".to_string()];

    let entry = build_dep_entry(&dep);
    assert!(is_inline_table_dep(&entry));
    let features = extract_features(&entry).unwrap();
    assert_eq!(features, vec!["derive"]);
  }

  #[test]
  fn test_build_dep_entry_with_path() {
    let mut dep = create_test_dep("local-crate", "0.1.0");
    dep.path = Some(PathBuf::from("../local-crate"));

    let entry = build_dep_entry(&dep);
    assert!(is_inline_table_dep(&entry));
    assert!(has_path(&entry));
    assert_eq!(extract_path(&entry).unwrap(), "../local-crate");
  }

  #[test]
  fn test_build_dep_entry_no_default_features() {
    let mut dep = create_test_dep("tokio", "1.0");
    dep.default_features = false;

    let entry = build_dep_entry(&dep);
    assert!(is_inline_table_dep(&entry));
    assert!(!has_default_features(&entry));
  }

  #[test]
  fn test_build_workspace_dep_entry_simple() {
    let entry = build_workspace_dep_entry(None, false);
    assert!(is_workspace_dep(&entry));
    assert!(is_inline_table_dep(&entry));
  }

  #[test]
  fn test_build_workspace_dep_entry_with_features() {
    let entry = build_workspace_dep_entry(Some(vec!["extra".to_string()]), false);
    assert!(is_workspace_dep(&entry));
    let features = extract_features(&entry).unwrap();
    assert_eq!(features, vec!["extra"]);
  }

  #[test]
  fn test_build_workspace_dep_entry_optional() {
    let entry = build_workspace_dep_entry(None, true);
    assert!(is_workspace_dep(&entry));
    assert!(is_optional(&entry));
  }

  #[test]
  fn test_build_transitive_entry() {
    let features = vec!["feature1".to_string(), "feature2".to_string()];
    let entry = build_transitive_entry(&features);
    assert!(is_workspace_dep(&entry));
    let extracted = extract_features(&entry).unwrap();
    assert_eq!(extracted, features);
  }

  // ========================================================================
  // Section 2: Workspace Reference Detection Tests
  // ========================================================================

  #[test]
  fn test_is_workspace_dep_inline_table() {
    let mut table = InlineTable::new();
    table.insert("workspace", Value::from(true));
    let item = Item::Value(Value::InlineTable(table));
    assert!(is_workspace_dep(&item));
  }

  #[test]
  fn test_is_workspace_dep_false() {
    let mut table = InlineTable::new();
    table.insert("version", Value::from("1.0"));
    let item = Item::Value(Value::InlineTable(table));
    assert!(!is_workspace_dep(&item));
  }

  #[test]
  fn test_extract_workspace_marker() {
    let mut table = InlineTable::new();
    table.insert("workspace", Value::from(true));
    table.insert("features", build_feature_array(&["a".to_string()]));
    let mut item = Item::Value(Value::InlineTable(table));

    assert!(extract_workspace_marker(&mut item));
    assert!(!is_workspace_dep(&item));
    // Features should still be there
    assert!(extract_features(&item).is_some());
  }

  // ========================================================================
  // Section 3: TOML Section Navigation Tests
  // ========================================================================

  #[test]
  fn test_ensure_section_creates_new() {
    let content = "[package]\nname = \"test\"\n";
    let mut doc: DocumentMut = content.parse().unwrap();

    ensure_section(&mut doc, "workspace").unwrap();
    assert!(doc.contains_key("workspace"));
  }

  #[test]
  fn test_ensure_section_nested() {
    let content = "";
    let mut doc: DocumentMut = content.parse().unwrap();

    ensure_section(&mut doc, "workspace.dependencies").unwrap();
    assert!(doc.contains_key("workspace"));
    assert!(doc["workspace"].as_table().unwrap().contains_key("dependencies"));
  }

  #[test]
  fn test_get_or_create_table() {
    let content = "";
    let mut doc: DocumentMut = content.parse().unwrap();

    let table = get_or_create_table(&mut doc, "workspace.dependencies").unwrap();
    assert!(table.is_empty());

    // Should work again without error
    let table2 = get_or_create_table(&mut doc, "workspace.dependencies").unwrap();
    assert!(table2.is_empty());
  }

  #[test]
  fn test_get_table() {
    let content = "[workspace]\n[workspace.dependencies]\nserde = \"1.0\"\n";
    let doc: DocumentMut = content.parse().unwrap();

    let table = get_table(&doc, "workspace.dependencies").unwrap();
    assert!(table.contains_key("serde"));

    let none = get_table(&doc, "nonexistent.path");
    assert!(none.is_none());
  }

  #[test]
  fn test_insert_remove_dependency() {
    let content = "[dependencies]\n";
    let mut doc: DocumentMut = content.parse().unwrap();

    let deps = get_or_create_table(&mut doc, "dependencies").unwrap();
    let entry = Item::Value(Value::from("1.0"));
    insert_dependency(deps, "serde", entry).unwrap();

    assert!(deps.contains_key("serde"));

    let removed = remove_dependency(deps, "serde");
    assert!(removed);
    assert!(!deps.contains_key("serde"));

    let removed_again = remove_dependency(deps, "serde");
    assert!(!removed_again);
  }

  #[test]
  fn test_get_dependencies() {
    let content = "[dependencies]\nserde = \"1.0\"\ntokio = \"1.0\"\n";
    let doc: DocumentMut = content.parse().unwrap();

    let table = get_table(&doc, "dependencies").unwrap();
    let deps = get_dependencies(table);

    assert_eq!(deps.len(), 2);
    let names: Vec<String> = deps.iter().map(|(name, _)| name.clone()).collect();
    assert!(names.contains(&"serde".to_string()));
    assert!(names.contains(&"tokio".to_string()));
  }

  // ========================================================================
  // Section 4: Target-Specific Dependencies Tests
  // ========================================================================

  #[test]
  fn test_insert_target_dependency() {
    let content = "";
    let mut doc: DocumentMut = content.parse().unwrap();

    let entry = Item::Value(Value::from("1.0"));
    insert_target_dependency(&mut doc, "'cfg(unix)'", "dependencies", "libc", entry).unwrap();

    let path = "target.'cfg(unix)'.dependencies";
    let table = get_table(&doc, path).unwrap();
    assert!(table.contains_key("libc"));
  }

  // ========================================================================
  // Section 5: Feature Array Operations Tests
  // ========================================================================

  #[test]
  fn test_build_feature_array() {
    let features = vec!["derive".to_string(), "std".to_string()];
    let value = build_feature_array(&features);

    assert!(value.as_array().is_some());
    let arr = value.as_array().unwrap();
    assert_eq!(arr.len(), 2);
  }

  #[test]
  fn test_extract_features_inline_table() {
    let mut table = InlineTable::new();
    table.insert("version", Value::from("1.0"));
    table.insert("features", build_feature_array(&["a".to_string(), "b".to_string()]));
    let item = Item::Value(Value::InlineTable(table));

    let features = extract_features(&item).unwrap();
    assert_eq!(features, vec!["a", "b"]);
  }

  #[test]
  fn test_set_features() {
    let mut table = InlineTable::new();
    table.insert("version", Value::from("1.0"));
    let mut item = Item::Value(Value::InlineTable(table));

    set_features(&mut item, vec!["new".to_string()]).unwrap();

    let features = extract_features(&item).unwrap();
    assert_eq!(features, vec!["new"]);
  }

  #[test]
  fn test_remove_features() {
    let mut table = InlineTable::new();
    table.insert("version", Value::from("1.0"));
    table.insert("features", build_feature_array(&["old".to_string()]));
    let mut item = Item::Value(Value::InlineTable(table));

    assert!(extract_features(&item).is_some());
    let removed = remove_features(&mut item);
    assert!(removed);
    assert!(extract_features(&item).is_none());
  }

  // ========================================================================
  // Section 6: Path Dependency Operations Tests
  // ========================================================================

  #[test]
  fn test_has_path() {
    let mut table = InlineTable::new();
    table.insert("path", Value::from("../other"));
    let item = Item::Value(Value::InlineTable(table));

    assert!(has_path(&item));
  }

  #[test]
  fn test_extract_path() {
    let mut table = InlineTable::new();
    table.insert("path", Value::from("../local"));
    let item = Item::Value(Value::InlineTable(table));

    assert_eq!(extract_path(&item).unwrap(), "../local");
  }

  #[test]
  fn test_remove_path() {
    let mut table = InlineTable::new();
    table.insert("path", Value::from("../other"));
    let mut item = Item::Value(Value::InlineTable(table));

    assert!(remove_path(&mut item));
    assert!(!has_path(&item));
  }

  #[test]
  fn test_set_path() {
    let mut table = InlineTable::new();
    table.insert("version", Value::from("1.0"));
    let mut item = Item::Value(Value::InlineTable(table));

    set_path(&mut item, "../new-path").unwrap();
    assert_eq!(extract_path(&item).unwrap(), "../new-path");
  }

  // ========================================================================
  // Section 7: Default Features Operations Tests
  // ========================================================================

  #[test]
  fn test_has_default_features_default_true() {
    let item = Item::Value(Value::from("1.0"));
    assert!(has_default_features(&item));
  }

  #[test]
  fn test_has_default_features_explicit_false() {
    let mut table = InlineTable::new();
    table.insert("version", Value::from("1.0"));
    table.insert("default-features", Value::from(false));
    let item = Item::Value(Value::InlineTable(table));

    assert!(!has_default_features(&item));
  }

  #[test]
  fn test_set_default_features() {
    let mut table = InlineTable::new();
    table.insert("version", Value::from("1.0"));
    let mut item = Item::Value(Value::InlineTable(table));

    set_default_features(&mut item, false).unwrap();
    assert!(!has_default_features(&item));
  }

  #[test]
  fn test_remove_default_features() {
    let mut table = InlineTable::new();
    table.insert("version", Value::from("1.0"));
    table.insert("default-features", Value::from(false));
    let mut item = Item::Value(Value::InlineTable(table));

    let removed = remove_default_features(&mut item);
    assert!(removed);
    // Should now be true (default)
    assert!(has_default_features(&item));
  }

  // ========================================================================
  // Section 8: Batch Transformation Tests
  // ========================================================================

  #[test]
  fn test_transform_dependencies_in_section() {
    let content = "[dependencies]\nserde = \"1.0\"\ntokio = \"1.0\"\n";
    let mut doc: DocumentMut = content.parse().unwrap();

    let mut count = 0;
    transform_dependencies_in_section(&mut doc, "dependencies", |_name, _item| {
      count += 1;
      Ok(())
    })
    .unwrap();

    assert_eq!(count, 2);
  }

  #[test]
  fn test_transform_dependencies_can_modify() {
    let content = "[dependencies]\nserde = \"1.0\"\n";
    let mut doc: DocumentMut = content.parse().unwrap();

    transform_dependencies_in_section(&mut doc, "dependencies", |_name, item| {
      set_version(item, "2.0")?;
      Ok(())
    })
    .unwrap();

    let table = get_table(&doc, "dependencies").unwrap();
    let item = table.get("serde").unwrap();
    assert_eq!(extract_version(item).unwrap(), "2.0");
  }

  #[test]
  fn test_resolve_package_workspace_inheritance() {
    let content = "[package]\nname = \"test\"\nversion = { workspace = true }\n";
    let mut doc: DocumentMut = content.parse().unwrap();

    let mut workspace_package = Table::new();
    workspace_package.insert("version", Item::Value(Value::from("1.2.3")));

    resolve_package_workspace_inheritance(&mut doc, &workspace_package).unwrap();

    let package = doc.get("package").unwrap().as_table().unwrap();
    let version = package.get("version").unwrap();
    assert_eq!(version.as_value().unwrap().as_str().unwrap(), "1.2.3");
  }

  // ========================================================================
  // Section 9: Validation & Inspection Tests
  // ========================================================================

  #[test]
  fn test_collect_all_dep_sections() {
    let content = "[dependencies]\nserde = \"1.0\"\n[dev-dependencies]\ntokio = \"1.0\"\n";
    let doc: DocumentMut = content.parse().unwrap();

    let sections = collect_all_dep_sections(&doc);
    assert_eq!(sections.len(), 2);

    let names: Vec<String> = sections.iter().map(|(name, _)| name.clone()).collect();
    assert!(names.contains(&"dependencies".to_string()));
    assert!(names.contains(&"dev-dependencies".to_string()));
  }

  #[test]
  fn test_is_simple_string_dep() {
    let item = Item::Value(Value::from("1.0"));
    assert!(is_simple_string_dep(&item));

    let mut table = InlineTable::new();
    table.insert("version", Value::from("1.0"));
    let item2 = Item::Value(Value::InlineTable(table));
    assert!(!is_simple_string_dep(&item2));
  }

  #[test]
  fn test_extract_version_all_formats() {
    // Simple string
    let item1 = Item::Value(Value::from("1.0"));
    assert_eq!(extract_version(&item1).unwrap(), "1.0");

    // Inline table
    let mut table = InlineTable::new();
    table.insert("version", Value::from("2.0"));
    let item2 = Item::Value(Value::InlineTable(table));
    assert_eq!(extract_version(&item2).unwrap(), "2.0");
  }

  #[test]
  fn test_set_version_all_formats() {
    // Simple string
    let mut item1 = Item::Value(Value::from("1.0"));
    set_version(&mut item1, "2.0").unwrap();
    assert_eq!(extract_version(&item1).unwrap(), "2.0");

    // Inline table
    let mut table = InlineTable::new();
    table.insert("version", Value::from("1.0"));
    let mut item2 = Item::Value(Value::InlineTable(table));
    set_version(&mut item2, "3.0").unwrap();
    assert_eq!(extract_version(&item2).unwrap(), "3.0");
  }

  #[test]
  fn test_is_optional() {
    let mut table = InlineTable::new();
    table.insert("version", Value::from("1.0"));
    table.insert("optional", Value::from(true));
    let item = Item::Value(Value::InlineTable(table));

    assert!(is_optional(&item));
  }

  #[test]
  fn test_set_optional() {
    let mut table = InlineTable::new();
    table.insert("version", Value::from("1.0"));
    let mut item = Item::Value(Value::InlineTable(table));

    set_optional(&mut item, true).unwrap();
    assert!(is_optional(&item));
  }

  #[test]
  fn test_dep_kind_to_section() {
    assert_eq!(dep_kind_to_section(DepKind::Normal), "dependencies");
    assert_eq!(dep_kind_to_section(DepKind::Dev), "dev-dependencies");
    assert_eq!(dep_kind_to_section(DepKind::Build), "build-dependencies");
  }
}
