#![allow(dead_code)]

use crate::cargo::metadata::WorkspaceMetadata;
use crate::core::transform::{Transform, TransformContext};
use anyhow::{Context, Result};
use std::collections::HashMap;
use toml_edit::{DocumentMut, Item, Value};

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
  fn flatten_workspace_inheritance(&self, doc: &mut DocumentMut) -> Result<()> {
    // Load workspace Cargo.toml to get inherited values
    let workspace_toml_path = self.workspace_metadata.workspace_root().join("Cargo.toml");
    let workspace_content = std::fs::read_to_string(&workspace_toml_path)
      .context("Failed to read workspace Cargo.toml")?;
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
  fn transform_dependencies_to_versions(&self, doc: &mut DocumentMut) -> Result<()> {
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
                anyhow::bail!(
                  "Cannot split: dependency '{}' has path to non-workspace crate. \
                     Convert to version dependency first.",
                  dep_name
                );
              }
            }
          }
        }
      }
    }

    Ok(())
  }
}

impl Transform for CargoTransform {
  fn transform_to_split(&self, content: &str, _context: &TransformContext) -> Result<String> {
    let mut doc: DocumentMut = content.parse().context("Failed to parse Cargo.toml")?;

    // 1. Flatten workspace = true to actual values
    self.flatten_workspace_inheritance(&mut doc)?;

    // 2. Transform path dependencies to version dependencies
    self.transform_dependencies_to_versions(&mut doc)?;

    // 3. Remove workspace section if it exists (not needed in split repo)
    doc.remove("workspace");

    Ok(doc.to_string())
  }

  fn transform_to_mono(&self, content: &str, _context: &TransformContext) -> Result<String> {
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
}
