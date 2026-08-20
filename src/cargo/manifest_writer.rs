//! Clean manifest writing operations
//!
//! Handles writing [workspace.dependencies] and updating member manifests

use crate::cargo::manifest_analyzer::DepKind;
use crate::cargo::manifest_ops;
use crate::cargo::unify_types::{TransitivePin, UnifiedDep};
use crate::error::{RailError, RailResult, ResultExt};
use crate::toml::format::TomlFormatter;
use std::path::Path;

/// Writes changes to Cargo.toml files
pub struct ManifestWriter {
    formatter: TomlFormatter,
}

impl Default for ManifestWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl ManifestWriter {
    /// Creates a new manifest writer with default settings
    pub fn new() -> Self {
        Self {
            formatter: TomlFormatter::new(),
        }
    }

    /// Write unified dependencies to workspace Cargo.toml
    ///
    /// IMPORTANT: This MERGES new deps with existing workspace.dependencies.
    /// It does NOT replace the entire section.
    pub fn write_workspace_deps(&self, workspace_toml_path: &Path, deps: &[UnifiedDep]) -> RailResult<()> {
        // Read workspace Cargo.toml
        let mut doc = manifest_ops::read_toml_file(workspace_toml_path)?;

        // Ensure [workspace] section exists
        manifest_ops::ensure_section(&mut doc, "workspace").context("Failed to create [workspace] section")?;

        // Write all dependencies to [workspace.dependencies]
        // Note: Target constraints stay in member manifests (e.g., [target.'cfg(unix)'.dependencies])
        // with `workspace = true`. We never write [target] sections to workspace Cargo.toml.
        let deps_table = manifest_ops::get_or_create_table(&mut doc, "workspace.dependencies")
            .context("Failed to create [workspace.dependencies]")?;

        for dep in deps {
            let entry = manifest_ops::build_dep_entry(dep);
            manifest_ops::insert_dependency(deps_table, &dep.name, entry).context("Failed to insert dependency")?;
        }

        // Format and write
        self.formatter.format_manifest(&mut doc)?;
        manifest_ops::write_toml_file(workspace_toml_path, &doc)?;

        Ok(())
    }

    /// Update a member's Cargo.toml to use workspace inheritance
    ///
    /// Rewrites the dependency entry to `workspace = true`, preserving optional
    /// and local feature overlays when provided.
    pub fn update_member<S: AsRef<str>>(
        &self,
        member_toml_path: &Path,
        dep_name: &str,
        dep_kind: DepKind,
        target: Option<&str>,
        local_features: Option<&[S]>,
        is_optional: bool,
    ) -> RailResult<()> {
        // Read member Cargo.toml
        let mut doc = manifest_ops::read_toml_file(member_toml_path)?;

        // Get section name from kind
        let kind_section = self.dep_kind_to_section(dep_kind);

        // Build workspace-inherited entry
        let entry = manifest_ops::build_workspace_dep_entry(local_features, is_optional);

        // Handle target-specific vs regular sections
        if let Some(target_cfg) = target {
            // Target-specific: write to [target.'cfg(...)'.dependencies]
            manifest_ops::insert_target_dependency(&mut doc, target_cfg, kind_section, dep_name, entry)
                .context("Failed to insert target-specific workspace dependency")?;
        } else {
            // Regular section: write to [dependencies], [dev-dependencies], or [build-dependencies]
            let deps = manifest_ops::get_or_create_table(&mut doc, kind_section)
                .context("Failed to get dependencies section")?;
            manifest_ops::insert_dependency(deps, dep_name, entry).context("Failed to insert workspace dependency")?;
        }

        // Format and write
        self.formatter.format_manifest(&mut doc)?;
        manifest_ops::write_toml_file(member_toml_path, &doc)?;

        Ok(())
    }

    /// Add fragmented transitive dependencies to the selected pin host.
    ///
    /// This adds entries with `workspace = true` to the host's dev-dependencies.
    /// IMPORTANT: The caller must ensure these deps are already in [workspace.dependencies]
    /// before calling this function. Use `write_transitive_workspace_deps` first.
    pub fn add_transitive_pins(&self, host_toml_path: &Path, transitives: &[TransitivePin]) -> RailResult<()> {
        // Read host Cargo.toml (usually workspace root)
        let mut doc = manifest_ops::read_toml_file(host_toml_path)?;

        // Ensure [dev-dependencies] exists
        let dev_deps = manifest_ops::get_or_create_table(&mut doc, "dev-dependencies")
            .context("Failed to create [dev-dependencies]")?;

        // Add each transitive as a dev dependency with workspace = true
        for pin in transitives {
            let entry = manifest_ops::build_transitive_entry(&pin.features);
            manifest_ops::insert_dependency(dev_deps, &pin.name, entry)
                .context("Failed to insert transitive dependency")?;
        }

        // Format and write
        self.formatter.format_manifest(&mut doc)?;
        manifest_ops::write_toml_file(host_toml_path, &doc)?;

        Ok(())
    }

    /// Write transitive dependencies to [workspace.dependencies]
    ///
    /// This must be called BEFORE `add_transitive_pins` so that the deps exist
    /// in workspace.dependencies when referenced with `workspace = true`.
    pub fn write_transitive_workspace_deps(
        &self,
        workspace_toml_path: &Path,
        transitives: &[TransitivePin],
    ) -> RailResult<()> {
        // Read workspace Cargo.toml
        let mut doc = manifest_ops::read_toml_file(workspace_toml_path)?;

        // Ensure [workspace.dependencies] exists
        manifest_ops::ensure_section(&mut doc, "workspace").context("Failed to create [workspace] section")?;
        let deps_table = manifest_ops::get_or_create_table(&mut doc, "workspace.dependencies")
            .context("Failed to create [workspace.dependencies]")?;

        // Add each transitive dependency with version and features
        for pin in transitives {
            let entry = manifest_ops::build_versioned_dep_entry(&pin.version, &pin.features);
            manifest_ops::insert_dependency(deps_table, &pin.name, entry)
                .context("Failed to insert transitive to workspace.dependencies")?;
        }

        // Format and write
        self.formatter.format_manifest(&mut doc)?;
        manifest_ops::write_toml_file(workspace_toml_path, &doc)?;

        Ok(())
    }

    /// Convert DepKind to Cargo.toml section name
    fn dep_kind_to_section(&self, dep_kind: DepKind) -> &'static str {
        manifest_ops::dep_kind_to_section(dep_kind)
    }

    /// Write MSRV (rust-version) to workspace manifest
    ///
    /// Writes to [workspace.package].rust-version so that members can inherit it
    /// via `rust-version = { workspace = true }`
    pub fn write_workspace_msrv(&self, workspace_toml_path: &Path, msrv: &semver::Version) -> RailResult<()> {
        // Read workspace Cargo.toml
        let mut doc = manifest_ops::read_toml_file(workspace_toml_path)?;

        // Ensure [workspace] section exists
        manifest_ops::ensure_section(&mut doc, "workspace").context("Failed to create [workspace] section")?;

        // Get or create [workspace.package] section
        let ws_package = manifest_ops::get_or_create_table(&mut doc, "workspace.package")
            .context("Failed to create [workspace.package]")?;

        // Format MSRV as "major.minor.patch" (explicit and unambiguous)
        let msrv_str = format!("{}.{}.{}", msrv.major, msrv.minor, msrv.patch);

        // Insert or update rust-version
        ws_package.insert("rust-version", toml_edit::value(&msrv_str));

        // Format and write
        self.formatter.format_manifest(&mut doc)?;
        manifest_ops::write_toml_file(workspace_toml_path, &doc)?;

        Ok(())
    }

    /// Ensure a member manifest inherits `rust-version` from `[workspace.package]`.
    ///
    /// Sets `[package].rust-version = { workspace = true }`.
    pub fn enforce_member_msrv_inheritance(&self, member_toml_path: &Path) -> RailResult<()> {
        let mut doc = manifest_ops::read_toml_file(member_toml_path)?;

        let Some(pkg) = doc.get_mut("package").and_then(|p| p.as_table_like_mut()) else {
            return Ok(());
        };

        let mut tbl = toml_edit::InlineTable::new();
        tbl.insert("workspace", true.into());
        pkg.insert("rust-version", toml_edit::value(toml_edit::Value::InlineTable(tbl)));

        self.formatter.format_manifest(&mut doc)?;
        manifest_ops::write_toml_file(member_toml_path, &doc)?;
        Ok(())
    }

    /// Replace one equivalent member package field with workspace inheritance.
    pub fn inherit_member_package_field(&self, member_toml_path: &Path, field: &str) -> RailResult<()> {
        if !crate::cargo::manifest_analyzer::package_field_is_automatically_inheritable(field) {
            return Err(RailError::message(format!(
                "package field '{field}' is not authorized for automatic workspace inheritance"
            )));
        }
        let mut doc = manifest_ops::read_toml_file(member_toml_path)?;
        let package = doc
            .get_mut("package")
            .and_then(|item| item.as_table_like_mut())
            .ok_or_else(|| RailError::message(format!("'{}' has no [package] table", member_toml_path.display())))?;
        let decor = package
            .get(field)
            .and_then(toml_edit::Item::as_value)
            .map(|value| value.decor().clone())
            .ok_or_else(|| {
                RailError::message(format!(
                    "package field '{field}' disappeared from '{}' before mutation",
                    member_toml_path.display()
                ))
            })?;

        let mut table = toml_edit::InlineTable::new();
        table.insert("workspace", true.into());
        let mut value = toml_edit::Value::InlineTable(table);
        *value.decor_mut() = decor;
        package.insert(field, toml_edit::value(value));

        self.formatter.format_manifest(&mut doc)?;
        manifest_ops::write_toml_file(member_toml_path, &doc)
    }

    /// Remove an unused dependency from a member's Cargo.toml
    ///
    /// Removes the dependency from the resolved dependency section (optionally
    /// target-scoped) and writes the manifest back.
    pub fn remove_dep(
        &self,
        member_toml_path: &Path,
        dep_name: &str,
        dep_kind: DepKind,
        target: Option<&str>,
    ) -> RailResult<()> {
        // Read member Cargo.toml
        let mut doc = manifest_ops::read_toml_file(member_toml_path)?;

        // Get section name from kind
        let kind_section = self.dep_kind_to_section(dep_kind);

        // Handle target-specific vs regular sections
        if let Some(target_cfg) = target {
            // Target-specific: remove from [target.'cfg(...)'.dependencies]
            manifest_ops::remove_target_dependency(&mut doc, target_cfg, kind_section, dep_name)
                .context("Failed to remove target-specific dependency")?;
        } else {
            // Regular section: remove from [dependencies], [dev-dependencies], or [build-dependencies]
            if let Some(deps) = doc.get_mut(kind_section).and_then(|d| d.as_table_like_mut()) {
                deps.remove(dep_name);
            }
        }

        // Format and write
        self.formatter.format_manifest(&mut doc)?;
        manifest_ops::write_toml_file(member_toml_path, &doc)?;

        Ok(())
    }

    /// Remove a dead feature from a member's Cargo.toml
    ///
    /// No-ops if the manifest has no `[features]` table or the feature is absent.
    pub fn remove_feature(&self, member_toml_path: &Path, feature_name: &str) -> RailResult<()> {
        // Read member Cargo.toml
        let mut doc = manifest_ops::read_toml_file(member_toml_path)?;

        // Remove from [features] section
        if let Some(features) = doc.get_mut("features").and_then(|f| f.as_table_like_mut()) {
            features.remove(feature_name);
        }

        // Format and write
        self.formatter.format_manifest(&mut doc)?;
        manifest_ops::write_toml_file(member_toml_path, &doc)?;

        Ok(())
    }

    /// Add features to an existing dependency in a member's Cargo.toml
    ///
    /// This is used to fix undeclared feature dependencies - when a crate relies on
    /// Cargo's feature unification to "borrow" features from other workspace members.
    ///
    /// Adds missing features idempotently and preserves existing dependency form
    /// (string or table) where possible.
    pub fn add_features<S: AsRef<str>>(
        &self,
        member_toml_path: &Path,
        dep_name: &str,
        dep_kind: DepKind,
        target: Option<&str>,
        features_to_add: &[S],
    ) -> RailResult<()> {
        // Read member Cargo.toml
        let mut doc = manifest_ops::read_toml_file(member_toml_path)?;

        // Get section name from kind
        let kind_section = self.dep_kind_to_section(dep_kind);

        // Handle target-specific vs regular sections
        let dep_item = if let Some(target_cfg) = target {
            // Target-specific: look in [target.'cfg(...)'.dependencies]
            let path = format!("target.{}.{}", target_cfg, kind_section);
            let table = manifest_ops::get_or_create_table(&mut doc, &path)?;
            table.get_mut(dep_name)
        } else {
            // Regular section
            doc.get_mut(kind_section)
                .and_then(|t| t.as_table_mut())
                .and_then(|t| t.get_mut(dep_name))
        };

        let Some(dep_item) = dep_item else {
            // Dependency not found in manifest - skip silently
            // This can legitimately happen with:
            // - Renamed dependencies (package = "other-name")
            // - Target-specific deps where the target section doesn't exist
            // - Dependencies that were already converted to workspace = true
            return Ok(());
        };

        // Get existing features (if any)
        let mut existing_features: Vec<String> = manifest_ops::extract_features(dep_item).unwrap_or_default();

        // Add new features (dedup)
        for feature in features_to_add {
            let feat_str = feature.as_ref();
            if !existing_features.iter().any(|f| f == feat_str) {
                existing_features.push(feat_str.to_string());
            }
        }

        // Sort for consistency
        existing_features.sort();

        // Update the dependency entry
        // If it's a simple string like `serde = "1.0"`, we need to convert it to a table
        if let Some(version) = dep_item.as_str() {
            // Convert simple string (e.g., `dep = "1.0"`) to inline table with features
            let mut inline_table = toml_edit::InlineTable::new();
            inline_table.insert("version", toml_edit::Value::from(version));
            inline_table.insert("features", manifest_ops::build_feature_array(&existing_features));
            *dep_item = toml_edit::Item::Value(toml_edit::Value::InlineTable(inline_table));
        } else {
            // Already a table, just update features
            manifest_ops::set_features(dep_item, &existing_features)?;
        }

        // Format and write
        self.formatter.format_manifest(&mut doc)?;
        manifest_ops::write_toml_file(member_toml_path, &doc)?;

        Ok(())
    }
}
