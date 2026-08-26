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
        if self.paths.is_empty() && self.config.is_empty() && self.cargo.is_empty() {
            return Err(invalid(&field, "at least one of paths, config, or cargo is required"));
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
        Ok(())
    }
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
