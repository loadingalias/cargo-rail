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
/// Simple version-only deps return `Item::Value(Value::String("^1.0"))`.
/// Deps with features/path/default-features=false return inline tables.
pub fn build_dep_entry(dep: &UnifiedDep) -> Item {
  // Simple case: just version, no features, defaults enabled, no path or rename.
  if dep.features.is_empty() && dep.default_features && dep.path.is_none() && dep.package.is_none() {
    let mut value = Value::from(dep.version_req.to_string());
    value.decor_mut().set_suffix(" #unified");
    return Item::Value(value);
  }

  // Complex case: use inline table
  let mut table = InlineTable::new();

  if let Some(package) = &dep.package {
    table.insert("package", Value::from(package.as_ref()));
  }

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
/// Produces `{ workspace = true }` plus optional feature/optional fields and
/// the `#unified` comment marker.
pub fn build_workspace_dep_entry<S: AsRef<str>>(local_features: Option<&[S]>, is_optional: bool) -> Item {
  let mut table = InlineTable::new();
  table.insert("workspace", Value::from(true));

  // Add local features if any
  if let Some(features) = local_features
    && !features.is_empty()
  {
    table.insert("features", build_feature_array(features));
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
/// Used for host-owned transitive pins (dev-dependencies with workspace inheritance and features).
pub fn build_transitive_entry<S: AsRef<str>>(features: &[S]) -> Item {
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
pub fn build_versioned_dep_entry<S: AsRef<str>>(version: &semver::Version, features: &[S]) -> Item {
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
  use std::sync::Arc;

  use super::super::fields::extract_features;
  use super::super::workspace_ref::is_workspace_dep;

  fn create_test_dep(name: &str, version: &str) -> UnifiedDep {
    UnifiedDep {
      name: Arc::from(name),
      package: None,
      version_req: version.parse().unwrap(),
      features: vec![],
      default_features: true,
      used_by: vec![Arc::from("test-crate")],
      target: None,
      path: None,
    }
  }

  #[test]
  fn test_build_dep_entry_simple() {
    let dep = create_test_dep("serde", "1.0");
    let entry = build_dep_entry(&dep);
    // Simple case returns a string value
    assert!(entry.as_str().is_some());
    assert_eq!(entry.as_str().unwrap(), "^1.0");
  }

  #[test]
  fn test_build_dep_entry_with_features() {
    let mut dep = create_test_dep("serde", "1.0");
    dep.features = vec![Arc::from("derive")];

    let entry = build_dep_entry(&dep);
    assert!(entry.as_inline_table().is_some());
    let features = extract_features(&entry).unwrap();
    assert_eq!(features, vec!["derive"]);
  }

  #[test]
  fn test_build_dep_entry_preserves_renamed_package_identity() {
    let mut dep = create_test_dep("renamed_serde", "1.0");
    dep.package = Some(Arc::from("serde"));

    let entry = build_dep_entry(&dep);
    let table = entry.as_inline_table().expect("renamed dependency uses a table");
    assert_eq!(table.get("package").and_then(Value::as_str), Some("serde"));
    assert_eq!(table.get("version").and_then(Value::as_str), Some("^1.0"));
  }

  #[test]
  fn test_build_dep_entry_with_path() {
    let mut dep = create_test_dep("local-crate", "0.1.0");
    dep.path = Some(PathBuf::from("../local-crate"));

    let entry = build_dep_entry(&dep);
    let table = entry.as_inline_table().unwrap();
    assert!(table.contains_key("path"));
    assert_eq!(table.get("path").unwrap().as_str().unwrap(), "../local-crate");
  }

  #[test]
  fn test_build_dep_entry_no_default_features() {
    let mut dep = create_test_dep("tokio", "1.0");
    dep.default_features = false;

    let entry = build_dep_entry(&dep);
    let table = entry.as_inline_table().unwrap();
    assert!(!table.get("default-features").unwrap().as_bool().unwrap());
  }

  #[test]
  fn test_build_workspace_dep_entry_simple() {
    let entry = build_workspace_dep_entry::<String>(None, false);
    assert!(is_workspace_dep(&entry));
    assert!(entry.as_inline_table().is_some());
  }

  #[test]
  fn test_build_workspace_dep_entry_with_features() {
    let features = ["extra".to_string()];
    let entry = build_workspace_dep_entry(Some(&features[..]), false);
    assert!(is_workspace_dep(&entry));
    let features = extract_features(&entry).unwrap();
    assert_eq!(features, vec!["extra"]);
  }

  #[test]
  fn test_build_workspace_dep_entry_optional() {
    let entry = build_workspace_dep_entry::<String>(None, true);
    assert!(is_workspace_dep(&entry));
    let table = entry.as_inline_table().unwrap();
    assert!(table.get("optional").unwrap().as_bool().unwrap());
  }

  #[test]
  fn test_build_transitive_entry() {
    let features = ["feature1".to_string(), "feature2".to_string()];
    let entry = build_transitive_entry(&features);
    assert!(is_workspace_dep(&entry));
    let extracted = extract_features(&entry).unwrap();
    assert_eq!(extracted, vec!["feature1", "feature2"]);
  }
}
