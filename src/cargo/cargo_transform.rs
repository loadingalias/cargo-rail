//! Captured Cargo manifest policy for split and sync transforms.

use std::collections::BTreeMap;

use cargo_metadata::Metadata;
use semver::VersionReq;
use toml_edit::{DocumentMut, Item, Table, Value};

use crate::cargo::manifest_ops;
use crate::error::{RailError, RailResult, ResultExt as _};
use crate::workspace::WorkspaceContext;

#[derive(Debug, Clone)]
struct StandaloneDependency {
    package: Option<String>,
    version: String,
}

/// Immutable transformation policy derived from one authoritative workspace snapshot.
#[derive(Debug)]
pub(crate) struct ManifestTransformPolicy {
    workspace_package: Option<Table>,
    workspace_lints: Option<Item>,
    dependencies: BTreeMap<String, StandaloneDependency>,
}

impl ManifestTransformPolicy {
    pub(crate) fn capture(ctx: &WorkspaceContext) -> RailResult<Self> {
        let manifest = ctx.snapshot()?.workspace_manifest()?;
        let path = manifest.path().as_path();
        let content = std::str::from_utf8(manifest.bytes())
            .with_context(|| format!("workspace manifest '{}' is not valid UTF-8", path.display()))?;
        let document = content
            .parse::<DocumentMut>()
            .with_context(|| format!("failed to parse captured workspace manifest '{}'", path.display()))?;
        Self::from_document(&document, ctx.cargo().metadata())
    }

    fn from_document(document: &DocumentMut, metadata: &Metadata) -> RailResult<Self> {
        let workspace = document.get("workspace").and_then(Item::as_table);
        let workspace_package = workspace
            .and_then(|table| table.get("package"))
            .and_then(Item::as_table)
            .cloned();
        let workspace_lints = workspace.and_then(|table| table.get("lints")).cloned();
        let mut dependencies = BTreeMap::new();

        if let Some(table) = workspace
            .and_then(|table| table.get("dependencies"))
            .and_then(Item::as_table)
        {
            for (alias, item) in table {
                let package = dependency_field(item, "package")
                    .filter(|name| *name != alias)
                    .map(str::to_string);
                let package_name = package.as_deref().unwrap_or(alias);
                let declared = item.as_str().or_else(|| dependency_field(item, "version"));
                let requirement = declared.and_then(|value| VersionReq::parse(value).ok());
                let mut candidates = metadata
                    .packages
                    .iter()
                    .filter(|candidate| candidate.name == package_name)
                    .filter(|candidate| {
                        requirement
                            .as_ref()
                            .is_none_or(|requirement| requirement.matches(&candidate.version))
                    })
                    .collect::<Vec<_>>();
                candidates.sort_unstable_by(|left, right| left.version.cmp(&right.version));
                let version = candidates
                    .last()
                    .map(|package| package.version.to_string())
                    .or_else(|| declared.map(str::to_string));
                if let Some(version) = version {
                    dependencies.insert(alias.to_string(), StandaloneDependency { package, version });
                }
            }
        }

        Ok(Self {
            workspace_package,
            workspace_lints,
            dependencies,
        })
    }

    /// Transform one captured Cargo manifest into standalone split form.
    pub(crate) fn transform_to_split(&self, content: &str, target_has_workspace: bool) -> RailResult<String> {
        let mut document = content
            .parse::<DocumentMut>()
            .context("failed to parse Cargo.toml selected for split transformation")?;

        if let Some(workspace_package) = &self.workspace_package {
            manifest_ops::resolve_package_workspace_inheritance(&mut document, workspace_package)?;
        }
        self.transform_dependencies_to_standalone(&mut document)?;
        if !target_has_workspace {
            self.resolve_lints_workspace_inheritance(&mut document);
        }

        Ok(document.to_string())
    }

    fn resolve_lints_workspace_inheritance(&self, document: &mut DocumentMut) {
        let inherited = document
            .get("lints")
            .and_then(Item::as_table)
            .and_then(|table| table.get("workspace"))
            .and_then(Item::as_value)
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !inherited {
            return;
        }

        document.remove("lints");
        if let Some(workspace_lints) = &self.workspace_lints {
            document.insert("lints", workspace_lints.clone());
        }
    }

    fn transform_dependencies_to_standalone(&self, document: &mut DocumentMut) -> RailResult<()> {
        transform_dependency_sections(document.as_table_mut(), |name, item| {
            self.transform_dependency(name, item)
        })?;

        let Some(targets) = document.get_mut("target").and_then(Item::as_table_mut) else {
            return Ok(());
        };
        for (_, target) in targets.iter_mut() {
            if let Some(target) = target.as_table_mut() {
                transform_dependency_sections(target, |name, item| self.transform_dependency(name, item))?;
            }
        }
        Ok(())
    }

    fn transform_dependency(&self, name: &str, item: &mut Item) -> RailResult<()> {
        if manifest_ops::is_workspace_dep(item) {
            let dependency = self.dependencies.get(name).ok_or_else(|| {
                RailError::message(format!(
                    "workspace dependency '{name}' has no captured standalone version"
                ))
            })?;
            manifest_ops::extract_workspace_marker(item);
            manifest_ops::set_version(item, &dependency.version)?;
            if let Some(package) = &dependency.package {
                set_dependency_package(item, package)?;
            }
        }

        manifest_ops::remove_path(item);
        Ok(())
    }
}

fn dependency_field<'a>(item: &'a Item, field: &str) -> Option<&'a str> {
    item.as_inline_table()
        .and_then(|table| table.get(field))
        .and_then(Value::as_str)
        .or_else(|| {
            item.as_table()
                .and_then(|table| table.get(field))
                .and_then(Item::as_value)
                .and_then(Value::as_str)
        })
}

fn set_dependency_package(item: &mut Item, package: &str) -> RailResult<()> {
    if let Some(table) = item.as_inline_table_mut() {
        table.insert("package", Value::from(package));
        return Ok(());
    }
    if let Some(table) = item.as_table_mut() {
        table.insert("package", Item::Value(Value::from(package)));
        return Ok(());
    }
    Err(RailError::message(
        "cannot set package alias on a non-table dependency declaration",
    ))
}

fn transform_dependency_sections(
    document: &mut Table,
    mut transform: impl FnMut(&str, &mut Item) -> RailResult<()>,
) -> RailResult<()> {
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        let Some(dependencies) = document.get_mut(section).and_then(Item::as_table_mut) else {
            continue;
        };
        let names = dependencies
            .iter()
            .map(|(name, _)| name.to_string())
            .collect::<Vec<_>>();
        for name in names {
            if let Some(item) = dependencies.get_mut(&name) {
                transform(&name, item)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_metadata() -> Metadata {
        serde_json::from_value(serde_json::json!({
            "packages": [],
            "workspace_members": [],
            "resolve": null,
            "target_directory": "/tmp",
            "version": 1,
            "workspace_root": "/tmp",
            "metadata": null
        }))
        .unwrap()
    }

    #[test]
    fn captured_policy_resolves_aliases_lints_and_target_dependencies() {
        let workspace = r#"
[workspace]

[workspace.package]
edition = "2024"

[workspace.dependencies]
renamed = { package = "actual", version = "1.2" }

[workspace.lints.rust]
unsafe_code = "forbid"
"#
        .parse::<DocumentMut>()
        .unwrap();
        let policy = ManifestTransformPolicy::from_document(&workspace, &empty_metadata()).unwrap();
        let member = r#"
[package]
name = "member"
version = "0.1.0"
edition.workspace = true

[lints]
workspace = true

[target.'cfg(unix)'.dependencies]
renamed = { workspace = true, features = ["extra"] }
"#;

        let transformed = policy.transform_to_split(member, false).unwrap();
        let document = transformed.parse::<DocumentMut>().unwrap();
        assert_eq!(document["package"]["edition"].as_str(), Some("2024"));
        assert_eq!(document["lints"]["rust"]["unsafe_code"].as_str(), Some("forbid"));
        let dependency = document["target"]["cfg(unix)"]["dependencies"]["renamed"]
            .as_inline_table()
            .unwrap();
        assert_eq!(dependency.get("version").and_then(Value::as_str), Some("1.2"));
        assert_eq!(dependency.get("package").and_then(Value::as_str), Some("actual"));
        assert!(dependency.get("workspace").is_none());
        assert!(dependency.get("features").is_some());
    }
}
