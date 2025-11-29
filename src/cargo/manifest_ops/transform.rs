//! Batch transformation operations and validation/inspection functions

use crate::cargo::manifest_analyzer::DepKind;
use crate::error::{RailError, RailResult};
use toml_edit::{DocumentMut, Item, Table, Value};

use super::navigation::{get_dependencies, get_table};
use super::workspace_ref::is_package_workspace_inherited;

// ============================================================================
// Batch Transformation Operations
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
// Validation & Inspection
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
      "cannot set version: item is not a valid dependency format",
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
    Err(RailError::message("cannot set optional: item is not a table"))
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
  use toml_edit::{DocumentMut, InlineTable, Item, Table, Value};

  use super::super::fields::{extract_path, has_path};

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
}
