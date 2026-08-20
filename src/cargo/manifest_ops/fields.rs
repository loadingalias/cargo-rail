//! Field operations for dependency entries (features, path, default-features)

use crate::error::{RailError, RailResult};
use toml_edit::{Array, InlineTable, Item, Table, Value};

// Feature Array Operations

/// Build feature array from strings
pub fn build_feature_array<S: AsRef<str>>(features: &[S]) -> Value {
    let mut array = Array::new();
    for feature in features {
        array.push(feature.as_ref());
    }
    Value::from(array)
}

/// Extract features from dependency entry
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
pub fn set_features<S: AsRef<str>>(item: &mut Item, features: &[S]) -> RailResult<()> {
    if let Some(table) = item.as_inline_table_mut() {
        table.insert("features", build_feature_array(features));
        Ok(())
    } else if let Some(table) = item.as_table_mut() {
        table.insert("features", Item::Value(build_feature_array(features)));
        Ok(())
    } else {
        Err(RailError::message("cannot set features: item is not a table"))
    }
}

// Path Dependency Operations

/// Remove path field from dependency
pub fn remove_path(item: &mut Item) -> bool {
    if let Some(table) = item.as_inline_table_mut() {
        table.remove("path").is_some()
    } else if let Some(table) = item.as_table_mut() {
        table.remove("path").is_some()
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

        set_features(&mut item, &["new".to_string()]).unwrap();

        let features = extract_features(&item).unwrap();
        assert_eq!(features, vec!["new"]);
    }

    #[test]
    fn test_remove_path() {
        let mut table = InlineTable::new();
        table.insert("path", Value::from("../other"));
        let mut item = Item::Value(Value::InlineTable(table));

        assert!(remove_path(&mut item));
        // Path should be gone
        assert!(item.as_inline_table().unwrap().get("path").is_none());
    }
}
