//! Workspace reference detection and manipulation

use toml_edit::Item;

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

#[cfg(test)]
mod tests {
  use super::*;
  use toml_edit::{InlineTable, Item, Value};

  use super::super::fields::{build_feature_array, extract_features};

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
}
