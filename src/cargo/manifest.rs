use crate::cargo::WorkspaceMetadata;
use crate::error::{RailError, RailResult, ResultExt};
use crate::toml::format::TomlFormatter;
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
  /// Manifest formatter
  formatter: TomlFormatter,
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
      formatter: TomlFormatter::new(),
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

    // 4. Format the manifest
    self.formatter.format_manifest(&mut doc)?;

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

    // Format the manifest
    self.formatter.format_manifest(&mut doc)?;

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
    _add_comments: bool,
  ) -> RailResult<()> {
    use crate::toml::builder::WorkspaceDepsBuilder;
    use crate::toml::format::TomlValue;
    use toml_edit::table;

    // Read workspace Cargo.toml
    let content = std::fs::read_to_string(workspace_toml_path).context("Failed to read workspace Cargo.toml")?;
    let mut doc: DocumentMut = content.parse().context("Failed to parse workspace Cargo.toml")?;

    // Ensure [workspace] section exists
    if !doc.contains_key("workspace") {
      doc["workspace"] = table();
    }

    // Group by target
    // None -> regular dependencies
    // Some(target) -> platform specific
    let mut regular_deps = Vec::new();
    let mut target_deps: HashMap<String, Vec<&crate::cargo::unify::UnifiedDep>> = HashMap::new();

    for dep in unified_deps {
      if let Some(ref target) = dep.target {
        target_deps.entry(target.clone()).or_default().push(dep);
      } else {
        regular_deps.push(dep);
      }
    }

    // 1. Write regular dependencies using Builder
    if !regular_deps.is_empty() {
      let mut builder = WorkspaceDepsBuilder::new();

      for dep in regular_deps {
        // Determine if we should add a comment
        let comment = if _add_comments && !dep.comments.is_empty() {
          Some(dep.comments.join("; "))
        } else {
          None
        };

        if dep.features.is_empty() && dep.default_features && dep.path.is_none() {
          // Simple string version: dep = "1.0"
          if let Some(c) = comment {
            builder.add_with_comment(&dep.name, &format!("\"{}\"", dep.version_req), &c);
          } else {
            builder.add(&dep.name, &format!("\"{}\"", dep.version_req));
          }
        } else {
          // Complex table version
          let mut pairs = Vec::new();

          if let Some(ref path) = dep.path {
            pairs.push(("path".to_string(), TomlValue::String(path.to_string())));
          } else {
            pairs.push(("version".to_string(), TomlValue::String(dep.version_req.to_string())));
          }

          if !dep.default_features {
            pairs.push(("default-features".to_string(), TomlValue::Bool(false)));
          }

          if !dep.features.is_empty() {
            pairs.push(("features".to_string(), TomlValue::Array(dep.features.clone())));
          }

          if let Some(c) = comment {
            builder.add_table_with_comment(&dep.name, &pairs, &c);
          } else {
            builder.add_table(&dep.name, &pairs);
          }
        }
      }

      // The builder generates a full [workspace.dependencies] string.
      // We need to parse it and merge it into the document.
      // This is a bit inefficient (generate string -> parse -> merge), but it ensures consistency.
      // Alternatively, we could make the builder operate on DocumentMut, but that breaks the pattern.
      // For now, let's parse the builder output.
      let deps_toml = builder.build()?;
      let deps_doc: DocumentMut = deps_toml
        .parse()
        .map_err(|e| RailError::message(format!("Failed to parse generated dependencies: {}", e)))?;

      if let Some(generated_deps) = deps_doc.get("workspace").and_then(|w| w.get("dependencies")) {
        // Replace or merge [workspace.dependencies]
        // We want to replace the entries we generated, but maybe preserve others?
        // The unification plan should be comprehensive, so replacing is likely correct for managed deps.
        // However, existing implementation seemed to overwrite.
        // Let's overwrite [workspace.dependencies] with our generated table.

        // Ensure workspace is a table
        let workspace = doc["workspace"]
          .as_table_mut()
          .ok_or_else(|| RailError::message("[workspace] is not a table"))?;
        workspace.insert("dependencies", generated_deps.clone());
      }
    }

    // 2. Write target-specific dependencies
    // The Builder currently only supports [workspace.dependencies].
    // For target dependencies, we might need to extend the builder or handle them manually here using the same logic.
    // Given the complexity, let's handle them manually but using the cleaner logic pattern.
    for (target, deps) in target_deps {
      // ... (Target specific logic remains similar but cleaner if possible)
      // For now, I will keep the existing logic for targets or adapt it.
      // The existing logic was very verbose.
      // Let's implement a mini-builder logic here or just use toml_edit directly but cleanly.

      let workspace = doc["workspace"]
        .as_table_mut()
        .ok_or_else(|| RailError::message("[workspace] is not a table"))?;

      // Ensure [workspace.target] table exists
      if !workspace.contains_key("target") {
        workspace["target"] = table();
      }
      let target_table = workspace["target"].as_table_mut().unwrap();

      if !target_table.contains_key(&target) {
        target_table[&target] = table();
      }
      let cfg_section = target_table[&target].as_table_mut().unwrap();

      if !cfg_section.contains_key("dependencies") {
        cfg_section["dependencies"] = table();
      }
      let deps_section = cfg_section["dependencies"].as_table_mut().unwrap();

      for dep in deps {
        // Use inline table for consistency
        use toml_edit::{InlineTable, Value};
        let mut dep_table = InlineTable::new();

        if let Some(ref path) = dep.path {
          dep_table.insert("path", Value::from(path.to_string()));
        } else {
          dep_table.insert("version", Value::from(dep.version_req.to_string()));
        }

        if !dep.default_features {
          dep_table.insert("default-features", Value::from(false));
        }

        if !dep.features.is_empty() {
          let mut features_array = toml_edit::Array::new();
          for feature in &dep.features {
            features_array.push(feature.as_str());
          }
          dep_table.insert("features", Value::from(features_array));
        }

        deps_section.insert(&dep.name, Item::Value(Value::InlineTable(dep_table)));
      }
    }

    // Format the manifest
    self.formatter.format_manifest(&mut doc)?;

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
    use crate::toml::builder::MemberManifestBuilder;
    use crate::toml::editor::TomlEditor;

    // Use TomlEditor for safe editing
    let mut editor = TomlEditor::open(member_toml_path)?;

    // Determine which section to modify
    let section_name = match dep_kind {
      DependencyKind::Normal => "dependencies",
      DependencyKind::Development => "dev-dependencies",
      DependencyKind::Build => "build-dependencies",
      DependencyKind::Unknown => {
        return Err(RailError::message("Unknown dependency kind"));
      }
    };

    // Check if dependency exists
    // We need to navigate manually first to check existence and get current values
    // because TomlEditor doesn't expose full read access to everything easily yet
    // but we can use editor.doc()
    let doc = editor.doc();
    let deps = doc
      .get(section_name)
      .and_then(|d| d.as_table_like())
      .ok_or_else(|| RailError::message(format!("[{}] section not found or not a table", section_name)))?;

    if !deps.contains_key(dep_name) {
      return Err(RailError::message(format!(
        "Dependency '{}' not found in [{}]",
        dep_name, section_name
      )));
    }

    let current_dep = deps.get(dep_name).unwrap();

    // Check for optional
    let is_optional = if let Some(dep_table) = current_dep.as_inline_table() {
      dep_table.get("optional").and_then(|v| v.as_bool()).unwrap_or(false)
    } else if let Some(dep_table) = current_dep.as_table() {
      dep_table.get("optional").and_then(|v| v.as_bool()).unwrap_or(false)
    } else {
      false
    };

    // We don't preserve features here because the unification plan
    // already computed the union. If we wanted to support per-member feature
    // additions, we'd handle that differently.

    // Build new dependency value using Builder
    let builder = MemberManifestBuilder::new();
    // We pass None for features to just get { workspace = true }
    // If we wanted to add features, we'd pass Some(vec![...])
    let new_dep_str = builder.workspace_dep(dep_name, None);

    // Parse the string back to a Value (a bit round-about but uses the builder)
    // Or we can manually construct the inline table if we want to add 'optional'
    // The builder currently returns a String.
    // Let's use the string and parse it.
    let mut new_dep_value = new_dep_str
      .parse::<Value>()
      .map_err(|e| RailError::message(format!("Failed to parse built dependency: {}", e)))?;

    // Add optional if needed
    if is_optional && let Some(inline) = new_dep_value.as_inline_table_mut() {
      inline.insert("optional", Value::from(true));
    }

    // Update using Editor
    // Path is "section.dep_name"
    let path = format!("{}.{}", section_name, dep_name);
    editor.set(&path, new_dep_value)?;

    // Add trailing comment (TomlEditor doesn't support comments on set yet easily)
    // So we might need to access doc_mut directly for comments
    if let Some(deps) = editor
      .doc_mut()
      .get_mut(section_name)
      .and_then(|d| d.as_table_like_mut())
      && let Some(item) = deps.get_mut(dep_name)
      && let Some(value) = item.as_value_mut()
    {
      value.decor_mut().set_suffix(" # unified by cargo-rail\n");
    }

    // Format (TomlEditor validates on write, but we can also format explicitly if needed)
    // The editor preserves format mostly.
    // But we might want to run the formatter.
    self.formatter.format_manifest(editor.doc_mut())?;

    // Write
    editor.write()?;

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

    let anyhow_dep = &doc["workspace"]["dependencies"]["anyhow"];

    if let Some(version) = anyhow_dep.as_str() {
      assert_eq!(version, "^1.0");
    } else if let Some(table) = anyhow_dep.as_inline_table() {
      assert_eq!(table.get("version").and_then(|v| v.as_str()), Some("^1.0"));
      assert!(!table.contains_key("features"), "Should not have features field");
    } else {
      panic!("Dependency is neither string nor inline table");
    }
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
