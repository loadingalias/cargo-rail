//! Field operations for dependency entries (features, path, default-features)

use crate::error::{RailError, RailResult};
use toml_edit::{Array, InlineTable, Item, Table, Value};

// ============================================================================
// Feature Array Operations
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
// Path Dependency Operations
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
// Default Features Operations
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

#[cfg(test)]
mod tests {
  use super::*;
  use toml_edit::{InlineTable, Item, Value};

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
}
