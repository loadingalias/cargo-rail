//! Input-only declarations for repository-owned planner work.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use glob::Pattern;
use serde::{Deserialize, Serialize};

use crate::error::ConfigError;

/// Planner configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanConfig {
    /// Repository-owned work declarations, keyed by stable work ID.
    #[serde(default)]
    pub work: BTreeMap<String, PlanWorkConfig>,
}

/// Inputs and output scope for one repository-owned work item.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanWorkConfig {
    /// Scope emitted when this work is required.
    pub scope: PlanWorkScope,
    /// Positive repository-relative input globs.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Exact effective configuration-field inputs.
    #[serde(default)]
    pub config: Vec<String>,
    /// Code-owned Cargo work subscriptions.
    #[serde(default)]
    pub cargo: Vec<String>,
    /// Optional checked-in variant catalog.
    pub variant_catalog: Option<String>,
    /// Bounded one-hop Cargo artifact prerequisites emitted as this work item.
    #[serde(default)]
    pub cargo_prerequisites: Vec<CargoPrerequisiteConfig>,
}

/// A conditional edge from selected Cargo work to prerequisite artifacts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CargoPrerequisiteConfig {
    /// Code-owned Cargo work whose execution selection activates the edge.
    pub source_work: String,
    /// Selected source packages or targets that activate the edge.
    pub when: Vec<CargoRootConfig>,
    /// Packages or exact targets that must be built first.
    pub require: Vec<CargoRootConfig>,
}

/// A workspace package or one exact target within that package.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CargoRootConfig {
    /// Exact workspace package name.
    pub package: String,
    /// Optional exact target identity.
    pub target: Option<CargoTargetRootConfig>,
}

/// Exact Cargo target name and kind.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CargoTargetRootConfig {
    /// Cargo target name.
    pub name: String,
    /// One target kind reported by Cargo metadata.
    pub kind: String,
}

/// Scope kind for repository-owned work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanWorkScope {
    /// One indivisible repository operation.
    Repository,
    /// Cargo package and target selectors.
    Cargo,
    /// A declarative set of CI or remote variants.
    Variants,
}

/// Stable code-owned Cargo work IDs that declarations may subscribe to.
pub const CARGO_WORK_IDS: &[&str] = &[
    "cargo.build",
    "cargo.clippy",
    "cargo.doc",
    "cargo.doctest",
    "cargo.package",
    "cargo.test",
];

impl PlanConfig {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        for (id, work) in &self.work {
            validate_stable_id(id, &format!("plan.work.{id}"))?;
            work.validate(id)?;
        }
        Ok(())
    }
}

impl PlanWorkConfig {
    fn validate(&self, id: &str) -> Result<(), ConfigError> {
        let field = format!("plan.work.{id}");
        if self.paths.is_empty()
            && self.config.is_empty()
            && self.cargo.is_empty()
            && self.cargo_prerequisites.is_empty()
            && self.variant_catalog.is_none()
        {
            return Err(invalid(
                &field,
                "at least one of paths, config, cargo, variant_catalog, or cargo_prerequisites is required",
            ));
        }
        validate_unique(&self.paths, &format!("{field}.paths"))?;
        validate_unique(&self.config, &format!("{field}.config"))?;
        validate_unique(&self.cargo, &format!("{field}.cargo"))?;

        for pattern in &self.paths {
            validate_positive_path(pattern, &format!("{field}.paths"), true)?;
        }
        for path in &self.config {
            if crate::config::schema::field_spec(path).is_none() {
                return Err(invalid(
                    &format!("{field}.config"),
                    &format!("'{path}' is not an exact configuration schema field"),
                ));
            }
        }
        for cargo in &self.cargo {
            if CARGO_WORK_IDS.binary_search(&cargo.as_str()).is_err() {
                return Err(invalid(
                    &format!("{field}.cargo"),
                    &format!("'{cargo}' is not a code-owned Cargo work ID"),
                ));
            }
        }
        if let Some(path) = &self.variant_catalog {
            if self.scope != PlanWorkScope::Variants {
                return Err(invalid(
                    &format!("{field}.variant_catalog"),
                    "variant_catalog is allowed only for variants scope",
                ));
            }
            validate_positive_path(path, &format!("{field}.variant_catalog"), false)?;
        }
        if !self.cargo_prerequisites.is_empty() && self.scope != PlanWorkScope::Cargo {
            return Err(invalid(
                &format!("{field}.cargo_prerequisites"),
                "cargo_prerequisites is allowed only for cargo scope",
            ));
        }
        let mut prerequisites = BTreeSet::new();
        for (index, prerequisite) in self.cargo_prerequisites.iter().enumerate() {
            let item = format!("{field}.cargo_prerequisites.{index}");
            if CARGO_WORK_IDS
                .binary_search(&prerequisite.source_work.as_str())
                .is_err()
            {
                return Err(invalid(
                    &format!("{item}.source_work"),
                    &format!("'{}' is not a code-owned Cargo work ID", prerequisite.source_work),
                ));
            }
            if prerequisite.when.is_empty() || prerequisite.require.is_empty() {
                return Err(invalid(
                    &item,
                    "when and require must each name at least one Cargo root",
                ));
            }
            validate_cargo_roots(&prerequisite.when, &format!("{item}.when"))?;
            validate_cargo_roots(&prerequisite.require, &format!("{item}.require"))?;
            if !prerequisites.insert(prerequisite) {
                return Err(invalid(
                    &format!("{field}.cargo_prerequisites"),
                    "duplicate Cargo prerequisite edge",
                ));
            }
        }
        Ok(())
    }
}

fn validate_cargo_roots(roots: &[CargoRootConfig], field: &str) -> Result<(), ConfigError> {
    let mut unique = BTreeSet::new();
    for root in roots {
        if root.package.is_empty() || root.package.starts_with('-') {
            return Err(invalid(field, "package names must be non-empty and not option-like"));
        }
        if let Some(target) = &root.target
            && (target.name.is_empty() || target.kind.is_empty())
        {
            return Err(invalid(field, "target names and kinds must be non-empty"));
        }
        if !unique.insert(root) {
            return Err(invalid(field, "duplicate Cargo root"));
        }
    }
    Ok(())
}

pub(crate) fn validate_stable_id(id: &str, field: &str) -> Result<(), ConfigError> {
    let mut chars = id.chars();
    if !matches!(chars.next(), Some('a'..='z'))
        || !chars.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || matches!(character, '.' | '-')
        })
    {
        return Err(invalid(field, "must match [a-z][a-z0-9.-]*"));
    }
    Ok(())
}

pub(crate) fn validate_positive_path(value: &str, field: &str, glob: bool) -> Result<(), ConfigError> {
    if value.is_empty() {
        return Err(invalid(field, "path selector must not be empty"));
    }
    if value.starts_with('!') {
        return Err(invalid(field, "negative path selectors are not supported"));
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(invalid(field, "path selector must stay inside the repository"));
    }
    if glob {
        Pattern::new(value).map_err(|error| ConfigError::InvalidGlobPattern {
            pattern: value.to_string(),
            message: error.to_string(),
        })?;
    }
    Ok(())
}

pub(crate) fn validate_unique(values: &[String], field: &str) -> Result<(), ConfigError> {
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            return Err(invalid(field, &format!("duplicate selector '{value}'")));
        }
    }
    Ok(())
}

fn invalid(field: &str, reason: &str) -> ConfigError {
    ConfigError::InvalidField {
        field: field.to_string(),
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn rejects_commands_negative_and_escaping_selectors() {
        for source in [
            "[plan.work.x]\nscope = 'repository'\ncommand = 'cargo test'\npaths = ['src/**']\n",
            "[plan.work.x]\nscope = 'repository'\npaths = ['!docs/**']\n",
            "[plan.work.x]\nscope = 'repository'\npaths = ['../outside']\n",
            "[plan.work.x]\nscope = 'repository'\nconfig = ['not.a.field']\n",
        ] {
            let parsed = toml_edit::de::from_str::<crate::config::RailConfig>(source);
            if let Ok(config) = parsed {
                assert!(config.plan.validate().is_err(), "accepted invalid config: {source}");
            }
        }
    }

    #[test]
    fn rejects_undeclared_cargo_work_ids() {
        for id in ["cargo.bench", "cargo.check", "cargo.unknown"] {
            let source = format!("[plan.work.x]\nscope = 'repository'\ncargo = ['{id}']\n");
            let config = toml_edit::de::from_str::<crate::config::RailConfig>(&source)
                .expect("configuration shape should parse");
            assert!(config.plan.validate().is_err(), "accepted undeclared work ID {id}");
        }
    }
}
