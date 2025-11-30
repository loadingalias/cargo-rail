//! Dependency entry building functions

use crate::cargo::unify_types::UnifiedDep;
use toml_edit::{InlineTable, Item, Value};

use super::fields::build_feature_array;

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
/// Creates entries like: `dep = { version = "1.0", default-features = false, features = ["foo"] }`
///
/// IMPORTANT: When pinning transitives with explicit features, we MUST set
/// `default-features = false` to prevent cargo from enabling default features
/// that might pull in new dependencies not present in the current Cargo.lock.
///
/// # Arguments
///
/// * `version` - The semver version to use
/// * `features` - Features to enable (intersection across all targets)
pub fn build_versioned_dep_entry(version: &semver::Version, features: &[String]) -> Item {
  // Simple case: just version, no features - let cargo use defaults
  // This is safe because we're not changing the feature set
  if features.is_empty() {
    let mut value = Value::from(format!("^{}", version));
    value.decor_mut().set_suffix(" #unified");
    return Item::Value(value);
  }

  // Complex case: version + explicit features
  // MUST disable default-features to avoid pulling in new deps
  let mut table = InlineTable::new();
  table.insert("version", Value::from(format!("^{}", version)));
  table.insert("default-features", Value::from(false));
  table.insert("features", build_feature_array(features));

  // Add #unified comment marker
  let mut value = Value::InlineTable(table);
  value.decor_mut().set_suffix(" #unified");

  Item::Value(value)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::cargo::unify_types::UnifiedDep;
  use std::path::PathBuf;

  use super::super::fields::{extract_features, extract_path, has_default_features, has_path};
  use super::super::transform::{extract_version, is_inline_table_dep, is_optional, is_simple_string_dep};
  use super::super::workspace_ref::is_workspace_dep;

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
}
