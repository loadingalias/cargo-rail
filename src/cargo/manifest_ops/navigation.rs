//! TOML section navigation and target-specific dependencies

use crate::error::{RailError, RailResult};
use toml_edit::{DocumentMut, Item, Table};

// TOML Section Navigation

/// Ensure a section exists, creating if necessary
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
        "cannot create section: expected table but found non-table",
      ));
    }
  }

  Ok(())
}

/// Get or create a table at dotted path
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
        "cannot navigate to {}: parent is not a table",
        part
      )));
    }
  }

  current
    .as_table_mut()
    .ok_or_else(|| RailError::message(format!("Final item at path '{}' is not a table", path)))
}

/// Insert dependency into a section
pub fn insert_dependency(section: &mut Table, name: &str, entry: Item) -> RailResult<()> {
  section.insert(name, entry);
  Ok(())
}

// Target-Specific Dependencies

/// Add target-specific dependency
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
  fn test_insert_dependency() {
    let content = "[dependencies]\n";
    let mut doc: DocumentMut = content.parse().unwrap();

    let deps = get_or_create_table(&mut doc, "dependencies").unwrap();
    let entry = Item::Value(Value::from("1.0"));
    insert_dependency(deps, "serde", entry).unwrap();

    assert!(deps.contains_key("serde"));
  }

  #[test]
  fn test_insert_target_dependency() {
    let content = "";
    let mut doc: DocumentMut = content.parse().unwrap();

    let entry = Item::Value(Value::from("1.0"));
    insert_target_dependency(&mut doc, "'cfg(unix)'", "dependencies", "libc", entry).unwrap();

    // Verify it was inserted
    let target_deps = get_or_create_table(&mut doc, "target.'cfg(unix)'.dependencies").unwrap();
    assert!(target_deps.contains_key("libc"));
  }
}
