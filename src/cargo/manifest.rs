use crate::cargo::WorkspaceMetadata;
use crate::error::{RailError, RailResult, ResultExt};
use cargo_metadata::DependencyKind;
use std::collections::HashMap;
use std::path::PathBuf;
use toml_edit::{DocumentMut, Item, Value};

/// Context for transformation operations
pub struct TransformContext {
  /// Crate name being transformed - public API field for future use
  #[allow(dead_code)]
  pub crate_name: String,
  /// Workspace root path - public API field for future use
  #[allow(dead_code)]
  pub workspace_root: PathBuf,
}

/// Cargo-specific transformations for Cargo.toml
/// Handles: path ↔ version, workspace flattening
pub struct CargoTransform {
  workspace_metadata: WorkspaceMetadata,
  /// Map of crate name -> version from workspace
  workspace_versions: HashMap<String, String>,
  /// Map of crate name -> relative path from workspace root
  workspace_paths: HashMap<String, String>,
}

impl CargoTransform {
  /// Create a new CargoTransform from workspace metadata
  pub fn new(workspace_metadata: WorkspaceMetadata) -> Self {
    let mut workspace_versions = HashMap::new();
    let mut workspace_paths = HashMap::new();

    // Build lookup maps from workspace packages
    for pkg in workspace_metadata.list_crates() {
      workspace_versions.insert(pkg.name.to_string(), pkg.version.to_string());

      // Calculate relative path from workspace root
      if let Some(manifest_dir) = pkg.manifest_path.parent()
        && let Ok(rel_path) = manifest_dir.strip_prefix(workspace_metadata.workspace_root())
      {
        workspace_paths.insert(pkg.name.to_string(), rel_path.to_string());
      }
    }

    Self {
      workspace_metadata,
      workspace_versions,
      workspace_paths,
    }
  }

  /// Flatten workspace = true fields with actual values
  fn flatten_workspace_inheritance(&self, doc: &mut DocumentMut) -> RailResult<()> {
    // Load workspace Cargo.toml to get inherited values
    let workspace_toml_path = self.workspace_metadata.workspace_root().join("Cargo.toml");
    let workspace_content =
      std::fs::read_to_string(&workspace_toml_path).context("Failed to read workspace Cargo.toml")?;
    let workspace_doc = workspace_content
      .parse::<DocumentMut>()
      .context("Failed to parse workspace Cargo.toml")?;

    // Get workspace.package section
    let workspace_pkg = workspace_doc
      .get("workspace")
      .and_then(|w| w.as_table())
      .and_then(|t| t.get("package"))
      .and_then(|p| p.as_table());

    // Get workspace.dependencies section
    let workspace_deps = workspace_doc
      .get("workspace")
      .and_then(|w| w.as_table())
      .and_then(|t| t.get("dependencies"))
      .and_then(|d| d.as_table());

    // Flatten [package] fields
    if let Some(package_table) = doc.get_mut("package").and_then(|p| p.as_table_like_mut()) {
      let inheritable_fields = [
        "version",
        "authors",
        "edition",
        "rust-version",
        "license",
        "repository",
        "homepage",
        "documentation",
        "description",
        "keywords",
        "categories",
      ];

      for field in inheritable_fields {
        // Check if field has workspace = true (handles both inline table and regular table)
        let has_workspace_inheritance = if let Some(value) = package_table.get(field) {
          // Check if it's an inline table with workspace = true
          if let Some(inline_table) = value.as_inline_table() {
            inline_table.get("workspace").and_then(|w| w.as_bool()) == Some(true)
          }
          // Check if it's a regular table with workspace = true
          else if let Some(table) = value.as_table() {
            table.get("workspace").and_then(|w| w.as_bool()) == Some(true)
          }
          // Check if it's a table-like with workspace = true
          else if let Some(table_like) = value.as_table_like() {
            table_like.get("workspace").and_then(|w| w.as_bool()) == Some(true)
          } else {
            false
          }
        } else {
          false
        };

        if has_workspace_inheritance {
          // Replace with actual value from workspace
          if let Some(workspace_pkg) = workspace_pkg
            && let Some(workspace_value) = workspace_pkg.get(field)
          {
            // Insert the actual value
            package_table.insert(field, workspace_value.clone());
          }
        }
      }
    }

    // Flatten [dependencies], [dev-dependencies], [build-dependencies]
    let dep_sections = ["dependencies", "dev-dependencies", "build-dependencies"];
    for section in dep_sections {
      if let Some(deps_table) = doc.get_mut(section).and_then(|d| d.as_table_like_mut()) {
        let dep_names: Vec<String> = deps_table.iter().map(|(k, _)| k.to_string()).collect();

        for dep_name in dep_names {
          // Check if dependency has workspace = true
          let has_workspace_inheritance = if let Some(value) = deps_table.get(&dep_name) {
            // Check if it's an inline table with workspace = true
            if let Some(inline_table) = value.as_inline_table() {
              inline_table.get("workspace").and_then(|w| w.as_bool()) == Some(true)
            }
            // Check if it's a regular table with workspace = true
            else if let Some(table) = value.as_table() {
              table.get("workspace").and_then(|w| w.as_bool()) == Some(true)
            }
            // Check if it's a table-like with workspace = true
            else if let Some(table_like) = value.as_table_like() {
              table_like.get("workspace").and_then(|w| w.as_bool()) == Some(true)
            } else {
              false
            }
          } else {
            false
          };

          if has_workspace_inheritance {
            // Replace with actual value from workspace.dependencies
            if let Some(workspace_deps) = workspace_deps
              && let Some(workspace_dep) = workspace_deps.get(&dep_name)
            {
              // Insert the actual value
              deps_table.insert(&dep_name, workspace_dep.clone());
            }
          }
        }
      }
    }

    Ok(())
  }

  /// Transform path dependencies to version dependencies
  fn transform_dependencies_to_versions(&self, doc: &mut DocumentMut) -> RailResult<()> {
    let dep_sections = ["dependencies", "dev-dependencies", "build-dependencies"];

    for section in dep_sections {
      if let Some(deps) = doc.get_mut(section).and_then(|d| d.as_table_like_mut()) {
        let dep_names: Vec<String> = deps.iter().map(|(k, _)| k.to_string()).collect();

        for dep_name in dep_names {
          if let Some(dep) = deps.get_mut(&dep_name) {
            // Check if it's a table with path field
            if let Some(dep_table) = dep.as_table_like_mut()
              && dep_table.contains_key("path")
            {
              // Check if it's a workspace path dependency
              if let Some(version) = self.workspace_versions.get(&dep_name) {
                // Remove path, add version
                dep_table.remove("path");
                dep_table.insert("version", Item::Value(Value::from(version.clone())));
              } else {
                // Path dependency to non-workspace crate - ERROR
                return Err(RailError::with_help(
                  format!(
                    "Cannot split: dependency '{}' has path to non-workspace crate",
                    dep_name
                  ),
                  "Convert to version dependency first",
                ));
              }
            }
          }
        }
      }
    }

    Ok(())
  }
}

impl CargoTransform {
  /// Transform a Cargo.toml for split repository
  ///
  /// Converts workspace inheritance and path dependencies to standalone format
  pub fn transform_to_split(&self, content: &str, _context: &TransformContext) -> RailResult<String> {
    let mut doc: DocumentMut = content.parse().context("Failed to parse Cargo.toml")?;

    // 1. Flatten workspace = true to actual values
    self.flatten_workspace_inheritance(&mut doc)?;

    // 2. Transform path dependencies to version dependencies
    self.transform_dependencies_to_versions(&mut doc)?;

    // 3. Remove workspace section if it exists (not needed in split repo)
    doc.remove("workspace");

    Ok(doc.to_string())
  }

  /// Transform a Cargo.toml back to monorepo format
  ///
  /// Converts version dependencies to path dependencies for workspace members
  pub fn transform_to_mono(&self, content: &str, _context: &TransformContext) -> RailResult<String> {
    let mut doc: DocumentMut = content.parse().context("Failed to parse Cargo.toml")?;

    // Transform version dependencies back to path dependencies
    let dep_sections = ["dependencies", "dev-dependencies", "build-dependencies"];

    for section in dep_sections {
      if let Some(deps) = doc.get_mut(section).and_then(|d| d.as_table_like_mut()) {
        let dep_names: Vec<String> = deps.iter().map(|(k, _)| k.to_string()).collect();

        for dep_name in dep_names {
          if let Some(dep) = deps.get_mut(&dep_name)
            && let Some(dep_table) = dep.as_table_like_mut()
          {
            // Check if this is a workspace crate
            if let Some(path) = self.workspace_paths.get(&dep_name)
              && dep_table.contains_key("version")
            {
              // Replace version with path
              dep_table.remove("version");
              let relative_path = format!("../{}", path);
              dep_table.insert("path", Item::Value(Value::from(relative_path)));
            }
          }
        }
      }
    }

    // Restore workspace = true for common fields (simplified for MVP)
    if let Some(package) = doc.get_mut("package").and_then(|p| p.as_table_like_mut())
      && package.contains_key("version")
    {
      // Create workspace = true for version
      package.insert("version", Item::Value(Value::from("workspace = true")));
    }

    Ok(doc.to_string())
  }

  /// Write unified dependencies to workspace Cargo.toml [workspace.dependencies] section
  ///
  /// Takes a UnificationPlan and writes all unified dependencies to the workspace
  /// Cargo.toml file's [workspace.dependencies] section.
  pub fn write_workspace_dependencies(
    &self,
    workspace_toml_path: &std::path::Path,
    unified_deps: &[crate::cargo::unify::UnifiedDep],
    add_comments: bool,
  ) -> RailResult<()> {
    use toml_edit::{Array, InlineTable, table};

    // Read workspace Cargo.toml
    let content = std::fs::read_to_string(workspace_toml_path).context("Failed to read workspace Cargo.toml")?;
    let mut doc: DocumentMut = content.parse().context("Failed to parse workspace Cargo.toml")?;

    // Ensure [workspace] section exists
    if !doc.contains_key("workspace") {
      doc["workspace"] = table();
    }

    // Write each unified dependency
    // Group by target (None for regular deps, Some(target) for platform-specific)
    for unified in unified_deps {
      // Determine which table to write to based on target
      let deps_table = if let Some(ref target) = unified.target {
        // Platform-specific dependency: write to [target.'<target>'.dependencies]
        let target_key = format!("target.'{}'", target);

        // Ensure target section exists in workspace
        let workspace = doc["workspace"]
          .as_table_mut()
          .ok_or_else(|| RailError::message("[workspace] is not a table"))?;

        if !workspace.contains_key(&target_key) {
          workspace[&target_key] = table();
        }

        let target_section = workspace[&target_key]
          .as_table_mut()
          .ok_or_else(|| RailError::message(format!("[workspace.{}] is not a table", target_key)))?;

        if !target_section.contains_key("dependencies") {
          target_section["dependencies"] = table();
        }

        target_section["dependencies"]
          .as_table_mut()
          .ok_or_else(|| RailError::message(format!("[workspace.{}.dependencies] is not a table", target_key)))?
      } else {
        // Regular dependency: write to [workspace.dependencies]
        let workspace = doc["workspace"]
          .as_table_mut()
          .ok_or_else(|| RailError::message("[workspace] is not a table"))?;

        if !workspace.contains_key("dependencies") {
          workspace["dependencies"] = table();
        }

        workspace["dependencies"]
          .as_table_mut()
          .ok_or_else(|| RailError::message("[workspace.dependencies] is not a table"))?
      };
      // Use inline table for simple deps and deps with few features
      // Use regular table format only for deps with many features (>10) to avoid long lines
      let use_inline_table = unified.features.len() <= 10;

      if use_inline_table {
        // Simple dependency or dependency with few features - use inline table format
        let mut dep_table = InlineTable::new();

        // INVISIBLE FEATURE: Support workspace member path dependencies
        // If this dependency has a path (workspace member), use path instead of version
        if let Some(ref path) = unified.path {
          // This is a workspace member - use path
          dep_table.insert("path", Value::from(path.to_string()));
        } else {
          // External dependency - use version
          dep_table.insert("version", Value::from(unified.version_req.to_string()));
        }

        // Add default-features if false (true is the default, so we only specify false)
        if !unified.default_features {
          dep_table.insert("default-features", Value::from(false));
        }

        // Add features if any (inline arrays for <10 features)
        if !unified.features.is_empty() {
          let mut features_array = Array::new();
          for feature in &unified.features {
            features_array.push(feature.as_str());
          }
          dep_table.insert("features", Value::from(features_array));
        }

        // Insert the dependency
        deps_table.insert(&unified.name, toml_edit::Item::Value(dep_table.into()));

        // Add comments if enabled and any exist
        if add_comments
          && !unified.comments.is_empty()
          && let Some(item) = deps_table.get_mut(&unified.name)
        {
          let comment_str = unified.comments.join(", ");
          item
            .as_value_mut()
            .unwrap()
            .decor_mut()
            .set_suffix(format!(" # {}", comment_str));
        }
      } else {
        // Complex dependency with features - use regular table format
        let mut dep_table = table();

        // INVISIBLE FEATURE: Support workspace member path dependencies
        if let Some(ref path) = unified.path {
          dep_table["path"] = toml_edit::value(path.to_string());
        } else {
          dep_table["version"] = toml_edit::value(unified.version_req.to_string());
        }

        // Add default-features if false
        if !unified.default_features {
          dep_table["default-features"] = toml_edit::value(false);
        }

        // Add features as multiline array
        let mut features_array = Array::new();
        for feature in &unified.features {
          features_array.push(feature.as_str());
        }
        dep_table["features"] = toml_edit::value(Value::Array(features_array));

        // Insert the dependency
        deps_table.insert(&unified.name, dep_table);

        // Add comments if enabled and any exist
        // Note: Comments for regular tables are more complex - for now we skip them
        // The comment is only applied to inline tables above
        if add_comments && !unified.comments.is_empty() {
          // Regular table entries don't support trailing comments in the same way
          // We could add them as a separate comment line above the entry, but that's complex
          // For now, we just skip comments for regular table format (with features)
        }
      }
    }

    // Write back to file
    std::fs::write(workspace_toml_path, doc.to_string()).context("Failed to write workspace Cargo.toml")?;

    Ok(())
  }

  /// Convert a member's dependency to use workspace inheritance
  ///
  /// Transforms a dependency entry in a member's Cargo.toml to use
  /// `{ workspace = true }` syntax, optionally preserving additional fields
  /// like `optional` or `features` that add to workspace definition.
  pub fn convert_to_workspace_inheritance(
    &self,
    member_toml_path: &std::path::Path,
    dep_name: &str,
    dep_kind: &DependencyKind,
  ) -> RailResult<()> {
    use toml_edit::InlineTable;

    // Read member Cargo.toml
    let content = std::fs::read_to_string(member_toml_path).context("Failed to read member Cargo.toml")?;
    let mut doc: DocumentMut = content.parse().context("Failed to parse member Cargo.toml")?;

    // Determine which section to modify
    let section_name = match dep_kind {
      DependencyKind::Normal => "dependencies",
      DependencyKind::Development => "dev-dependencies",
      DependencyKind::Build => "build-dependencies",
      DependencyKind::Unknown => {
        return Err(RailError::message("Unknown dependency kind"));
      }
    };

    // Get the dependencies section
    let deps = doc
      .get_mut(section_name)
      .and_then(|d| d.as_table_mut())
      .ok_or_else(|| RailError::message(format!("[{}] section not found or not a table", section_name)))?;

    // Check if dependency exists
    if !deps.contains_key(dep_name) {
      return Err(RailError::message(format!(
        "Dependency '{}' not found in [{}]",
        dep_name, section_name
      )));
    }

    // Get current dependency value
    let current_dep = deps.get(dep_name).unwrap();

    // Build new dependency value with workspace = true
    let mut new_dep = InlineTable::new();
    new_dep.insert("workspace", Value::from(true));

    // Preserve certain fields that can be specified alongside workspace = true:
    // - optional: can be different per member
    // - features: can ADD to workspace features
    if let Some(dep_table) = current_dep.as_inline_table() {
      if let Some(optional) = dep_table.get("optional")
        && let Some(optional_bool) = optional.as_bool()
        && optional_bool
      {
        new_dep.insert("optional", Value::from(true));
      }

      // Note: We don't preserve features here because the unification plan
      // already computed the union. If we wanted to support per-member feature
      // additions, we'd handle that differently.
    } else if let Some(dep_table) = current_dep.as_table()
      && let Some(optional) = dep_table.get("optional")
      && let Some(optional_bool) = optional.as_bool()
      && optional_bool
    {
      new_dep.insert("optional", Value::from(true));
    }

    // Replace dependency with workspace inheritance
    deps.insert(dep_name, toml_edit::Item::Value(new_dep.into()));

    // Add trailing comment to mark this as unified
    if let Some(item) = deps.get_mut(dep_name)
      && let Some(value) = item.as_value_mut()
    {
      value.decor_mut().set_suffix(" # unified by cargo-rail\n");
    }

    // Write back to file
    std::fs::write(member_toml_path, doc.to_string()).context("Failed to write member Cargo.toml")?;

    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::PathBuf;

  fn create_test_metadata() -> WorkspaceMetadata {
    // Create a minimal test metadata by running on current workspace
    let current_dir = std::env::current_dir().unwrap();
    WorkspaceMetadata::load(&current_dir).unwrap()
  }

  #[test]
  fn test_transform_path_to_version() {
    let metadata = create_test_metadata();
    let transformer = CargoTransform::new(metadata);

    let input = r#"
[package]
name = "test-crate"
version = "0.1.0"

[dependencies]
cargo-rail = { path = "../cargo-rail" }
anyhow = "1.0"
"#;

    let result = transformer.transform_to_split(
      input,
      &TransformContext {
        crate_name: "test-crate".to_string(),
        workspace_root: PathBuf::from("/test"),
      },
    );

    // Should transform path to version for workspace crates
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains(r#"version = "0.1.0""#) || output.contains(r#"version = '0.1.0'"#));
    assert!(!output.contains("path ="));
  }

  #[test]
  fn test_remove_workspace_section() {
    let metadata = create_test_metadata();
    let transformer = CargoTransform::new(metadata);

    let input = r#"
[package]
name = "test-crate"
version = "0.1.0"

[workspace]
members = ["crates/*"]

[dependencies]
anyhow = "1.0"
"#;

    let result = transformer.transform_to_split(
      input,
      &TransformContext {
        crate_name: "test-crate".to_string(),
        workspace_root: PathBuf::from("/test"),
      },
    );

    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(!output.contains("[workspace]"));
    assert!(!output.contains("members ="));
  }

  #[test]
  fn test_error_on_non_workspace_path_dep() {
    let metadata = create_test_metadata();
    let transformer = CargoTransform::new(metadata);

    let input = r#"
[package]
name = "test-crate"
version = "0.1.0"

[dependencies]
unknown-crate = { path = "../unknown" }
"#;

    let result = transformer.transform_to_split(
      input,
      &TransformContext {
        crate_name: "test-crate".to_string(),
        workspace_root: PathBuf::from("/test"),
      },
    );

    // Should error on non-workspace path dependencies
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("non-workspace crate"));
  }

  #[test]
  fn test_transform_preserves_other_fields() {
    let metadata = create_test_metadata();
    let transformer = CargoTransform::new(metadata);

    let input = r#"
[package]
name = "test-crate"
version = "0.1.0"
edition = "2021"
license = "MIT"

[dependencies]
anyhow = "1.0"

[dev-dependencies]
tokio = { version = "1.0", features = ["full"] }
"#;

    let result = transformer.transform_to_split(
      input,
      &TransformContext {
        crate_name: "test-crate".to_string(),
        workspace_root: PathBuf::from("/test"),
      },
    );

    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains(r#"name = "test-crate""#));
    assert!(output.contains("edition"));
    assert!(output.contains("license"));
    assert!(output.contains("[dev-dependencies]"));
  }

  #[test]
  fn test_roundtrip_simple_manifest() {
    let metadata = create_test_metadata();
    let transformer = CargoTransform::new(metadata);

    let input = r#"
[package]
name = "test-crate"
version = "0.1.0"

[dependencies]
anyhow = "1.0"
"#;

    let to_split = transformer.transform_to_split(
      input,
      &TransformContext {
        crate_name: "test-crate".to_string(),
        workspace_root: PathBuf::from("/test"),
      },
    );

    assert!(to_split.is_ok());
    let split_output = to_split.unwrap();

    // Should parse as valid TOML
    let doc: Result<DocumentMut, _> = split_output.parse();
    assert!(doc.is_ok());
  }

  // ==========================================================================
  // Workspace Dependency Unification Tests
  // ==========================================================================

  #[test]
  fn test_write_workspace_dependencies_creates_section() {
    use crate::cargo::unify::UnifiedDep;
    use semver::VersionReq;
    use std::collections::HashSet;
    use tempfile::NamedTempFile;

    let metadata = create_test_metadata();
    let transformer = CargoTransform::new(metadata);

    // Create a temporary workspace Cargo.toml
    let mut temp_file = NamedTempFile::new().unwrap();
    let workspace_toml = r#"
[workspace]
members = ["crate-a", "crate-b"]
"#;
    std::io::Write::write_all(&mut temp_file, workspace_toml.as_bytes()).unwrap();

    // Create unified dependencies
    let unified_deps = vec![UnifiedDep {
      name: "serde".to_string(),
      version_req: VersionReq::parse("1.0").unwrap(),
      features: vec!["derive".to_string()],
      feature_provenance: HashMap::new(),
      default_features: true,
      used_by: vec!["crate-a".to_string(), "crate-b".to_string()],
      dep_kinds: HashSet::new(),
      fragmentation_count: 2,
      path: None,
      target: None,
      comments: Vec::new(),
      is_proc_macro: false,
    }];

    // Write workspace dependencies
    let result = transformer.write_workspace_dependencies(temp_file.path(), &unified_deps, true);
    assert!(result.is_ok(), "Should successfully write workspace dependencies");

    // Read back and verify
    let content = std::fs::read_to_string(temp_file.path()).unwrap();
    let doc: DocumentMut = content.parse().unwrap();

    // Verify [workspace.dependencies] exists
    let workspace_deps = doc["workspace"]["dependencies"].as_table().unwrap();
    assert!(workspace_deps.contains_key("serde"), "Should have serde dependency");

    // Verify serde entry structure
    let serde_dep = workspace_deps.get("serde").unwrap();
    // Dependencies with few features (≤10) use inline table format
    assert!(serde_dep.is_inline_table(), "Should be inline table (has ≤10 features)");

    let serde_table = serde_dep.as_inline_table().unwrap();
    assert_eq!(
      serde_table.get("version").and_then(|v| v.as_str()),
      Some("^1.0"),
      "Should have correct version (with caret notation)"
    );

    // Features should be present
    assert!(serde_table.contains_key("features"), "Should have features");
  }

  #[test]
  fn test_write_workspace_dependencies_no_features() {
    use crate::cargo::unify::UnifiedDep;
    use semver::VersionReq;
    use std::collections::HashSet;
    use tempfile::NamedTempFile;

    let metadata = create_test_metadata();
    let transformer = CargoTransform::new(metadata);

    let mut temp_file = NamedTempFile::new().unwrap();
    let workspace_toml = r#"
[workspace]
members = ["crate-a"]
"#;
    std::io::Write::write_all(&mut temp_file, workspace_toml.as_bytes()).unwrap();

    let unified_deps = vec![UnifiedDep {
      name: "anyhow".to_string(),
      version_req: VersionReq::parse("1.0").unwrap(),
      features: vec![], // No features
      feature_provenance: HashMap::new(),
      default_features: true,
      used_by: vec!["crate-a".to_string()],
      dep_kinds: HashSet::new(),
      fragmentation_count: 1,
      path: None,
      target: None,
      comments: Vec::new(),
      is_proc_macro: false,
    }];

    transformer
      .write_workspace_dependencies(temp_file.path(), &unified_deps, true)
      .unwrap();

    let content = std::fs::read_to_string(temp_file.path()).unwrap();
    let doc: DocumentMut = content.parse().unwrap();

    let anyhow_dep = doc["workspace"]["dependencies"]["anyhow"].as_inline_table().unwrap();
    assert_eq!(anyhow_dep.get("version").and_then(|v| v.as_str()), Some("^1.0"));
    assert!(!anyhow_dep.contains_key("features"), "Should not have features field");
  }

  #[test]
  fn test_write_workspace_dependencies_default_features_false() {
    use crate::cargo::unify::UnifiedDep;
    use semver::VersionReq;
    use std::collections::HashSet;
    use tempfile::NamedTempFile;

    let metadata = create_test_metadata();
    let transformer = CargoTransform::new(metadata);

    let mut temp_file = NamedTempFile::new().unwrap();
    let workspace_toml = "[workspace]\nmembers = []";
    std::io::Write::write_all(&mut temp_file, workspace_toml.as_bytes()).unwrap();

    let unified_deps = vec![UnifiedDep {
      name: "tokio".to_string(),
      version_req: VersionReq::parse("1.0").unwrap(),
      features: vec!["fs".to_string(), "net".to_string()],
      feature_provenance: HashMap::new(),
      default_features: false, // Explicitly disabled
      used_by: vec!["crate-a".to_string()],
      dep_kinds: HashSet::new(),
      fragmentation_count: 1,
      path: None,
      target: None,
      comments: Vec::new(),
      is_proc_macro: false,
    }];

    transformer
      .write_workspace_dependencies(temp_file.path(), &unified_deps, true)
      .unwrap();

    let content = std::fs::read_to_string(temp_file.path()).unwrap();
    let doc: DocumentMut = content.parse().unwrap();

    // Dependencies with ≤10 features use inline table format
    let tokio_dep = doc["workspace"]["dependencies"]["tokio"].as_inline_table().unwrap();
    assert_eq!(
      tokio_dep.get("default-features").and_then(|v| v.as_bool()),
      Some(false),
      "Should have default-features = false"
    );
  }

  #[test]
  fn test_convert_to_workspace_inheritance_simple() {
    use tempfile::NamedTempFile;

    let metadata = create_test_metadata();
    let transformer = CargoTransform::new(metadata);

    let mut temp_file = NamedTempFile::new().unwrap();
    let member_toml = r#"
[package]
name = "test-crate"
version = "0.1.0"

[dependencies]
serde = { version = "1.0", features = ["derive"] }
"#;
    std::io::Write::write_all(&mut temp_file, member_toml.as_bytes()).unwrap();

    let result = transformer.convert_to_workspace_inheritance(temp_file.path(), "serde", &DependencyKind::Normal);
    assert!(result.is_ok(), "Should successfully convert to workspace inheritance");

    let content = std::fs::read_to_string(temp_file.path()).unwrap();
    let doc: DocumentMut = content.parse().unwrap();

    let serde_dep = doc["dependencies"]["serde"].as_inline_table().unwrap();
    assert_eq!(
      serde_dep.get("workspace").and_then(|v| v.as_bool()),
      Some(true),
      "Should have workspace = true"
    );
    assert!(!serde_dep.contains_key("version"), "Should not have version");
    assert!(!serde_dep.contains_key("features"), "Should not preserve features");
  }

  #[test]
  fn test_convert_to_workspace_inheritance_optional() {
    use tempfile::NamedTempFile;

    let metadata = create_test_metadata();
    let transformer = CargoTransform::new(metadata);

    let mut temp_file = NamedTempFile::new().unwrap();
    let member_toml = r#"
[package]
name = "test-crate"
version = "0.1.0"

[dependencies]
tokio = { version = "1.0", optional = true, features = ["fs"] }
"#;
    std::io::Write::write_all(&mut temp_file, member_toml.as_bytes()).unwrap();

    transformer
      .convert_to_workspace_inheritance(temp_file.path(), "tokio", &DependencyKind::Normal)
      .unwrap();

    let content = std::fs::read_to_string(temp_file.path()).unwrap();
    let doc: DocumentMut = content.parse().unwrap();

    let tokio_dep = doc["dependencies"]["tokio"].as_inline_table().unwrap();
    assert_eq!(tokio_dep.get("workspace").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
      tokio_dep.get("optional").and_then(|v| v.as_bool()),
      Some(true),
      "Should preserve optional = true"
    );
  }

  #[test]
  fn test_convert_to_workspace_inheritance_dev_dependencies() {
    use tempfile::NamedTempFile;

    let metadata = create_test_metadata();
    let transformer = CargoTransform::new(metadata);

    let mut temp_file = NamedTempFile::new().unwrap();
    let member_toml = r#"
[package]
name = "test-crate"
version = "0.1.0"

[dev-dependencies]
tempfile = "3.0"
"#;
    std::io::Write::write_all(&mut temp_file, member_toml.as_bytes()).unwrap();

    let result =
      transformer.convert_to_workspace_inheritance(temp_file.path(), "tempfile", &DependencyKind::Development);
    assert!(result.is_ok(), "Should handle dev-dependencies");

    let content = std::fs::read_to_string(temp_file.path()).unwrap();
    let doc: DocumentMut = content.parse().unwrap();

    let tempfile_dep = doc["dev-dependencies"]["tempfile"].as_inline_table().unwrap();
    assert_eq!(tempfile_dep.get("workspace").and_then(|v| v.as_bool()), Some(true));
  }

  #[test]
  fn test_convert_to_workspace_inheritance_build_dependencies() {
    use tempfile::NamedTempFile;

    let metadata = create_test_metadata();
    let transformer = CargoTransform::new(metadata);

    let mut temp_file = NamedTempFile::new().unwrap();
    let member_toml = r#"
[package]
name = "test-crate"
version = "0.1.0"

[build-dependencies]
cc = "1.0"
"#;
    std::io::Write::write_all(&mut temp_file, member_toml.as_bytes()).unwrap();

    let result = transformer.convert_to_workspace_inheritance(temp_file.path(), "cc", &DependencyKind::Build);
    assert!(result.is_ok(), "Should handle build-dependencies");

    let content = std::fs::read_to_string(temp_file.path()).unwrap();
    let doc: DocumentMut = content.parse().unwrap();

    let cc_dep = doc["build-dependencies"]["cc"].as_inline_table().unwrap();
    assert_eq!(cc_dep.get("workspace").and_then(|v| v.as_bool()), Some(true));
  }

  #[test]
  fn test_convert_to_workspace_inheritance_missing_dependency() {
    use tempfile::NamedTempFile;

    let metadata = create_test_metadata();
    let transformer = CargoTransform::new(metadata);

    let mut temp_file = NamedTempFile::new().unwrap();
    let member_toml = r#"
[package]
name = "test-crate"
version = "0.1.0"

[dependencies]
serde = "1.0"
"#;
    std::io::Write::write_all(&mut temp_file, member_toml.as_bytes()).unwrap();

    // Try to convert non-existent dependency
    let result = transformer.convert_to_workspace_inheritance(temp_file.path(), "nonexistent", &DependencyKind::Normal);
    assert!(result.is_err(), "Should error on missing dependency");
  }
}
