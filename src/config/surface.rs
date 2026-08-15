//! Rust source-surface analysis policy.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;

const SURFACE_LINTS: &[&str] = &[
  "dead-public",
  "unnecessary-public",
  "unnecessary-restricted-visibility",
  "unnecessary-crate-visibility",
];

const SURFACE_ITEM_KINDS: &[&str] = &[
  "associated-constant",
  "associated-type",
  "constant",
  "enum",
  "field",
  "foreign-function",
  "foreign-static",
  "function",
  "impl",
  "macro",
  "method",
  "module",
  "reexport",
  "static",
  "struct",
  "trait",
  "type-alias",
  "union",
  "variant",
];

/// Whether the workspace is the complete consumer universe for internal packages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceConsumerScope {
  /// Preserve public visibility because consumers may exist outside the workspace.
  #[default]
  Open,
  /// Authorize closed-world conclusions for non-publishable internal packages.
  Workspace,
}

/// Whether the allow-by-default `pub(crate)` reduction is enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceCrateVisibility {
  /// Preserve `pub(crate)` declarations.
  #[default]
  Preserve,
  /// Report `pub(crate)` declarations whose uses fit within `pub(super)`.
  Allow,
}

/// Diagnostic disposition for one override or exclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceLintLevel {
  /// Suppress a matching diagnostic without requiring it to remain present.
  #[default]
  Allow,
  /// Keep a matching diagnostic enabled.
  Warn,
  /// Keep a matching diagnostic enabled.
  Deny,
  /// Suppress a matching diagnostic and fail when it disappears.
  Expect,
}

/// One complete shipped binary or library root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceProduct {
  /// Exact workspace package name.
  pub package: String,
  /// Exact Cargo binary target name.
  pub bin: Option<String>,
  /// Exact Cargo library target name.
  pub lib: Option<String>,
  /// Human-owned explanation for the root.
  pub reason: String,
}

/// One item-specific diagnostic policy override.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceOverride {
  /// Exact surface lint name.
  pub lint: String,
  /// Exact workspace package name.
  pub package: String,
  /// Exact compiler diagnostic path.
  pub item: String,
  /// Exact declaration kind.
  pub kind: String,
  /// Diagnostic disposition.
  #[serde(default)]
  pub level: SurfaceLintLevel,
  /// Human-owned explanation for the policy exception.
  pub reason: String,
}

/// One module or file scope excluded from surface diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceExclude {
  /// Exact workspace package name.
  pub package: String,
  /// Compiler diagnostic module path.
  pub module: Option<String>,
  /// Repository-relative source file.
  pub file: Option<String>,
  /// Diagnostic disposition.
  #[serde(default)]
  pub level: SurfaceLintLevel,
  /// Human-owned explanation for the excluded scope.
  pub reason: String,
}

/// Sparse source-surface analysis configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SurfaceConfig {
  /// Closed-world authority for non-publishable internal packages.
  pub consumer_scope: SurfaceConsumerScope,
  /// Required compiler target views (`host` or a configured target triple).
  pub targets: Vec<String>,
  /// Whether `pub(crate)` to `pub(super)` findings are enabled.
  pub crate_visibility: SurfaceCrateVisibility,
  /// Preserve uniform field visibility when visibility repair is enabled.
  pub preserve_uniform_fields: bool,
  /// Explicit complete production roots; workspace binaries are implicit when empty.
  pub product: Vec<SurfaceProduct>,
  /// Item-specific diagnostic policies.
  pub r#override: Vec<SurfaceOverride>,
  /// Module- or file-scoped diagnostic exclusions.
  pub exclude: Vec<SurfaceExclude>,
}

impl Default for SurfaceConfig {
  fn default() -> Self {
    Self {
      consumer_scope: SurfaceConsumerScope::Open,
      targets: vec!["host".to_string()],
      crate_visibility: SurfaceCrateVisibility::Preserve,
      preserve_uniform_fields: false,
      product: Vec::new(),
      r#override: Vec::new(),
      exclude: Vec::new(),
    }
  }
}

impl SurfaceConfig {
  /// Validate policy syntax that does not require Cargo workspace state.
  pub fn validate(&self) -> Result<(), ConfigError> {
    if self.targets.is_empty() {
      return Err(invalid("surface.targets", "must contain at least one target view"));
    }
    validate_unique_non_empty(&self.targets, "surface.targets")?;

    let mut products = BTreeSet::new();
    for (index, product) in self.product.iter().enumerate() {
      let field = format!("surface.product.{index}");
      validate_text(&product.package, &format!("{field}.package"))?;
      validate_text(&product.reason, &format!("{field}.reason"))?;
      let target = match (&product.bin, &product.lib) {
        (Some(bin), None) => ("bin", bin),
        (None, Some(lib)) => ("lib", lib),
        _ => return Err(invalid(field, "requires exactly one of 'bin' or 'lib'")),
      };
      validate_text(target.1, &format!("{field}.{}", target.0))?;
      if !products.insert((&product.package, target.0, target.1)) {
        return Err(invalid(field, "duplicates an earlier product root"));
      }
    }

    let mut overrides = BTreeSet::new();
    for (index, policy) in self.r#override.iter().enumerate() {
      let field = format!("surface.override.{index}");
      validate_lint(&policy.lint, &format!("{field}.lint"))?;
      validate_text(&policy.package, &format!("{field}.package"))?;
      validate_text(&policy.item, &format!("{field}.item"))?;
      if !SURFACE_ITEM_KINDS.contains(&policy.kind.as_str()) {
        return Err(invalid(
          format!("{field}.kind"),
          format!("unknown declaration kind '{}'", policy.kind),
        ));
      }
      validate_text(&policy.reason, &format!("{field}.reason"))?;
      if !overrides.insert((&policy.lint, &policy.package, &policy.item, &policy.kind)) {
        return Err(invalid(field, "duplicates an earlier item override"));
      }
    }

    let mut exclusions = BTreeSet::new();
    for (index, policy) in self.exclude.iter().enumerate() {
      let field = format!("surface.exclude.{index}");
      validate_text(&policy.package, &format!("{field}.package"))?;
      validate_text(&policy.reason, &format!("{field}.reason"))?;
      let selector = match (&policy.module, &policy.file) {
        (Some(module), None) => ("module", module),
        (None, Some(file)) => ("file", file),
        _ => return Err(invalid(field, "requires exactly one of 'module' or 'file'")),
      };
      validate_text(selector.1, &format!("{field}.{}", selector.0))?;
      if !exclusions.insert((&policy.package, selector.0, selector.1)) {
        return Err(invalid(field, "duplicates an earlier exclusion"));
      }
    }
    Ok(())
  }
}

fn validate_lint(value: &str, field: &str) -> Result<(), ConfigError> {
  if SURFACE_LINTS.contains(&value) {
    Ok(())
  } else {
    Err(invalid(field, format!("unknown surface lint '{value}'")))
  }
}

fn validate_unique_non_empty(values: &[String], field: &str) -> Result<(), ConfigError> {
  let mut unique = BTreeSet::new();
  for value in values {
    validate_text(value, field)?;
    if !unique.insert(value) {
      return Err(invalid(field, format!("contains duplicate value '{value}'")));
    }
  }
  Ok(())
}

fn validate_text(value: &str, field: &str) -> Result<(), ConfigError> {
  if value.trim().is_empty() || value != value.trim() {
    Err(invalid(field, "must be non-empty and have no surrounding whitespace"))
  } else {
    Ok(())
  }
}

fn invalid(field: impl Into<String>, reason: impl Into<String>) -> ConfigError {
  ConfigError::InvalidField {
    field: field.into(),
    reason: reason.into(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn product_and_filter_selectors_are_exact() {
    let mut config = SurfaceConfig::default();
    config.product.push(SurfaceProduct {
      package: "app".to_string(),
      bin: Some("app".to_string()),
      lib: None,
      reason: "shipped application".to_string(),
    });
    config.r#override.push(SurfaceOverride {
      lint: "unnecessary-public".to_string(),
      package: "app".to_string(),
      item: "migration::legacy".to_string(),
      kind: "function".to_string(),
      level: SurfaceLintLevel::Expect,
      reason: "migration compatibility".to_string(),
    });
    config.exclude.push(SurfaceExclude {
      package: "app".to_string(),
      module: Some("generated".to_string()),
      file: None,
      level: SurfaceLintLevel::Expect,
      reason: "generated source".to_string(),
    });
    assert!(config.validate().is_ok());

    config.product[0].lib = Some("app".to_string());
    assert!(config.validate().is_err());
  }

  #[test]
  fn unknown_lints_and_empty_reasons_are_rejected() {
    let mut config = SurfaceConfig::default();
    config.r#override.push(SurfaceOverride {
      lint: "dead-code".to_string(),
      package: "app".to_string(),
      item: "legacy".to_string(),
      kind: "function".to_string(),
      level: SurfaceLintLevel::Allow,
      reason: "".to_string(),
    });
    assert!(config.validate().is_err());
  }
}
