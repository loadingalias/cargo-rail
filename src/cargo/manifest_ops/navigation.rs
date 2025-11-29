//! TOML section navigation and target-specific dependencies

use crate::error::{RailError, RailResult};
use toml_edit::{DocumentMut, Item, Table};

// ============================================================================
// TOML Section Navigation
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
// Target-Specific Dependencies
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

#[cfg(test)]
mod tests {
  use super::*;
  use toml_edit::{DocumentMut, Item, Value};

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
}
