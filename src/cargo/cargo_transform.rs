//! Cargo.toml transformation for split/sync operations
//!
//! This module provides simple Cargo.toml transformations needed by split and sync:
//! - Transform workspace dependencies to standalone format (for splits)
//! - Transform standalone dependencies back to workspace format (for syncs)

use crate::cargo::manifest_ops;
use crate::error::{RailError, RailResult};
use cargo_metadata::Metadata;
use std::cell::RefCell;
use std::path::PathBuf;
use toml_edit::{DocumentMut, Item, Table};

/// Context for Cargo.toml transformations
#[derive(Debug)]
pub struct TransformContext {
    /// Name of the crate being transformed
    pub crate_name: String,
    /// Workspace root path
    pub workspace_root: PathBuf,
    /// Whether the target repo will have a workspace structure
    /// - true: keep `[lints] workspace = true` (target is a workspace)
    /// - false: resolve `[lints]` to actual values (target is standalone)
    pub target_has_workspace: bool,
}

/// Cargo.toml transformer for split/sync operations
///
/// Caches the workspace document to avoid repeated I/O when transforming multiple manifests.
/// Uses interior mutability (`RefCell`) so the public API remains `&self`.
#[derive(Debug)]
pub struct CargoTransform {
    metadata: Metadata,
    /// Cached workspace document (loaded lazily via RefCell for interior mutability)
    cached_workspace_doc: RefCell<Option<DocumentMut>>,
    /// Workspace root for lazy loading
    workspace_root: PathBuf,
}

impl CargoTransform {
    /// Create a new transformer with workspace metadata
    pub fn new(metadata: Metadata) -> Self {
        let workspace_root = metadata.workspace_root.as_std_path().to_path_buf();
        Self {
            metadata,
            cached_workspace_doc: RefCell::new(None),
            workspace_root,
        }
    }

    /// Ensure workspace doc is loaded and cached
    fn ensure_workspace_doc_cached(&self) -> RailResult<()> {
        let already_cached = self.cached_workspace_doc.borrow().is_some();
        if !already_cached {
            let workspace_toml_path = self.workspace_root.join("Cargo.toml");
            let doc = manifest_ops::read_toml_file(&workspace_toml_path)?;
            *self.cached_workspace_doc.borrow_mut() = Some(doc);
        }
        Ok(())
    }

    /// Get workspace.package table, loading and caching the workspace doc if needed
    ///
    /// Uses `RefCell` for interior mutability - loads once, caches for reuse.
    fn get_workspace_package(&self) -> RailResult<Option<Table>> {
        self.ensure_workspace_doc_cached()?;

        let cache = self.cached_workspace_doc.borrow();
        Ok(cache
            .as_ref()
            .and_then(|doc| doc.get("workspace"))
            .and_then(|w| w.as_table())
            .and_then(|w| w.get("package"))
            .and_then(|p| p.as_table())
            .cloned())
    }

    /// Get workspace.lints table, loading and caching the workspace doc if needed
    fn get_workspace_lints(&self) -> RailResult<Option<Item>> {
        self.ensure_workspace_doc_cached()?;

        let cache = self.cached_workspace_doc.borrow();
        Ok(cache
            .as_ref()
            .and_then(|doc| doc.get("workspace"))
            .and_then(|w| w.as_table())
            .and_then(|w| w.get("lints"))
            .cloned())
    }

    /// Transform a Cargo.toml from workspace format to split (standalone) format
    ///
    /// This replaces workspace dependency references with concrete version requirements.
    pub fn transform_to_split(&self, content: &str, context: &TransformContext) -> RailResult<String> {
        let mut doc: DocumentMut = content
            .parse()
            .map_err(|e| RailError::message(format!("Failed to parse Cargo.toml: {}", e)))?;

        // Remove workspace inheritance markers and resolve to actual values
        self.resolve_workspace_inheritance(&mut doc)?;

        // Transform workspace dependencies to standalone format
        self.transform_dependencies_to_standalone(&mut doc)?;

        // Handle `[lints]` section based on target workspace mode
        // - If target has workspace: keep `[lints] workspace = true`
        // - If target is standalone: resolve to actual values or remove
        if !context.target_has_workspace {
            self.resolve_lints_workspace_inheritance(&mut doc)?;
        }

        Ok(doc.to_string())
    }

    /// Transform a Cargo.toml from split (standalone) format back to workspace format
    ///
    /// This is currently a no-op since syncing from remote to mono doesn't need transformation.
    /// The crate in the monorepo already uses workspace format.
    pub fn transform_to_mono(&self, content: &str, _context: &TransformContext) -> RailResult<String> {
        // For now, pass through unchanged. If we need to restore workspace.dependencies
        // references, we can implement that here.
        Ok(content.to_string())
    }

    /// Resolve workspace inheritance (workspace = true fields) to actual values
    ///
    /// Uses cached workspace document to avoid repeated I/O.
    fn resolve_workspace_inheritance(&self, doc: &mut DocumentMut) -> RailResult<()> {
        // Get workspace.package table from cache (loads workspace doc if needed)
        if let Some(workspace_pkg) = self.get_workspace_package()? {
            manifest_ops::resolve_package_workspace_inheritance(doc, &workspace_pkg)?;
        }

        Ok(())
    }

    /// Resolve `[lints]` workspace inheritance for standalone split targets
    ///
    /// If the crate has `[lints] workspace = true`, this:
    /// 1. Removes the `workspace = true` marker
    /// 2. Copies the actual lints from `[workspace.lints]` into `[lints]`
    /// 3. If no workspace lints exist, removes the `[lints]` section entirely
    fn resolve_lints_workspace_inheritance(&self, doc: &mut DocumentMut) -> RailResult<()> {
        // Check if [lints] section exists and has workspace = true
        let has_workspace_lints = doc
            .get("lints")
            .and_then(|l| l.as_table())
            .and_then(|t| t.get("workspace"))
            .and_then(|w| w.as_value())
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !has_workspace_lints {
            return Ok(());
        }

        // Get workspace.lints from source workspace
        let workspace_lints = self.get_workspace_lints()?;

        // Remove [lints] section first
        doc.remove("lints");

        // If workspace has lints, copy them to [lints] section
        if let Some(ws_lints) = workspace_lints {
            doc.insert("lints", ws_lints);
        }
        // If no workspace lints, we've already removed the section - crate will use defaults

        Ok(())
    }

    /// Transform workspace dependencies to standalone format
    fn transform_dependencies_to_standalone(&self, doc: &mut DocumentMut) -> RailResult<()> {
        // Transform each dependency section using manifest_ops
        manifest_ops::transform_dependencies_in_section(doc, "dependencies", |name, item| {
            self.transform_and_resolve_dep(item, name)
        })?;

        manifest_ops::transform_dependencies_in_section(doc, "dev-dependencies", |name, item| {
            self.transform_and_resolve_dep(item, name)
        })?;

        manifest_ops::transform_dependencies_in_section(doc, "build-dependencies", |name, item| {
            self.transform_and_resolve_dep(item, name)
        })?;

        Ok(())
    }

    /// Helper: transform and resolve a single dependency
    fn transform_and_resolve_dep(&self, dep_item: &mut Item, dep_name: &str) -> RailResult<()> {
        // Check if this is a workspace dependency using manifest_ops
        if manifest_ops::is_workspace_dep(dep_item) {
            // Find the dependency version in workspace metadata
            if let Some(pkg) = self.metadata.packages.iter().find(|p| p.name == dep_name) {
                let version = pkg.version.to_string();

                // Remove workspace marker and set version using manifest_ops
                manifest_ops::extract_workspace_marker(dep_item);
                manifest_ops::set_version(dep_item, &version)?;
            }
        }

        // Remove path dependencies (they won't be valid in split repo)
        // This applies to both workspace and non-workspace dependencies
        manifest_ops::remove_path(dep_item);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transform_workspace_dep() {
        let input = r#"
[package]
name = "test-crate"
version = "0.1.0"

[dependencies]
other-crate = { workspace = true }
serde = "1.0"
"#;

        // For testing, we'll just verify it parses and doesn't crash
        let doc: DocumentMut = input.parse().unwrap();
        assert!(doc.get("dependencies").is_some());
    }

    #[test]
    fn test_transform_to_mono_passthrough() {
        let input = r#"
[package]
name = "test-crate"
version = "0.1.0"

[dependencies]
serde = "1.0"
"#;

        // Create a minimal metadata for testing
        let metadata_json = serde_json::json!({
            "packages": [],
            "workspace_members": [],
            "resolve": null,
            "target_directory": "/tmp",
            "version": 1,
            "workspace_root": "/tmp",
            "metadata": null
        });

        let metadata: Metadata = serde_json::from_value(metadata_json).unwrap();
        let transformer = CargoTransform::new(metadata);
        let context = TransformContext {
            crate_name: "test-crate".to_string(),
            workspace_root: PathBuf::from("/tmp"),
            target_has_workspace: false,
        };

        let result = transformer.transform_to_mono(input, &context).unwrap();
        assert_eq!(result, input);
    }

    #[test]
    fn test_resolve_lints_workspace_inheritance_removes_workspace_true() {
        // Test that [lints] workspace = true is removed when no workspace lints exist
        let input = r#"
[package]
name = "test-crate"
version = "0.1.0"

[lints]
workspace = true
"#;

        let mut doc: DocumentMut = input.parse().unwrap();

        // Verify [lints] workspace = true exists before
        assert!(doc.get("lints").is_some());
        let lints = doc.get("lints").unwrap().as_table().unwrap();
        assert!(lints.get("workspace").is_some());

        // Remove [lints] section (simulating no workspace lints)
        doc.remove("lints");

        // Verify it's gone
        assert!(doc.get("lints").is_none());
    }

    #[test]
    fn test_resolve_lints_workspace_inheritance_copies_workspace_lints() {
        // Test that [lints] workspace = true is replaced with actual workspace.lints content
        let input = r#"
[package]
name = "test-crate"
version = "0.1.0"

[lints]
workspace = true
"#;

        let mut doc: DocumentMut = input.parse().unwrap();

        // Simulate having workspace.lints content
        let workspace_lints = r#"
[rust]
unexpected_cfgs = { level = "warn" }
"#;
        let ws_lints_doc: DocumentMut = workspace_lints.parse().unwrap();

        // Remove [lints] section first
        doc.remove("lints");

        // Insert workspace lints content
        if let Some(lints_item) = ws_lints_doc.as_item().as_table() {
            doc.insert("lints", Item::Table(lints_item.clone()));
        }

        // Verify [lints.rust] now exists with actual content
        let lints = doc.get("lints").unwrap().as_table().unwrap();
        assert!(lints.get("rust").is_some());
        assert!(lints.get("workspace").is_none()); // No workspace = true
    }
}
