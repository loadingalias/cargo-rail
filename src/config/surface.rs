//! Rust source-surface analysis policy.

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

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

/// One ordered global lint-level directive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceLintDirective {
    /// `warnings` or one exact surface lint name.
    pub selector: String,
    /// Ordered disposition applied to the selector.
    pub level: SurfaceLintLevel,
}

/// One explicit Cargo feature profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceFeatureProfile {
    /// Stable human-owned profile name.
    pub name: String,
    /// Enable every declared feature.
    #[serde(default, rename = "all-features")]
    pub all_features: bool,
    /// Disable default features.
    #[serde(default, rename = "no-default-features")]
    pub no_default_features: bool,
    /// Explicit Cargo feature names.
    #[serde(default)]
    pub features: Vec<String>,
}

/// One package selected for the explicit doctest pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceDoctest {
    /// Exact workspace package name.
    pub package: String,
}

/// Default doctest acquisition when no exact package list is present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceDoctestCoverage {
    /// Compile every doctest-enabled workspace package.
    #[default]
    Automatic,
    /// Compile no doctests; intended for exact migration from an analyzer run that omitted them.
    Disabled,
}

/// Target views selected for complete source-surface analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceTargetSelection {
    /// Inherit the host view and every target from the top-level workspace policy.
    Workspace,
    /// Analyze one explicit non-empty subset containing `host` and/or configured target triples.
    Explicit(Vec<String>),
}

impl Default for SurfaceTargetSelection {
    fn default() -> Self {
        Self::Explicit(vec!["host".to_string()])
    }
}

impl Serialize for SurfaceTargetSelection {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Workspace => serializer.serialize_str("workspace"),
            Self::Explicit(targets) => targets.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for SurfaceTargetSelection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Representation {
            Mode(String),
            Explicit(Vec<String>),
        }

        match Representation::deserialize(deserializer)? {
            Representation::Mode(mode) if mode == "workspace" => Ok(Self::Workspace),
            Representation::Mode(mode) => Err(de::Error::custom(format!(
                "surface.targets mode must be 'workspace', found '{mode}'"
            ))),
            Representation::Explicit(targets) => Ok(Self::Explicit(targets)),
        }
    }
}

impl SurfaceTargetSelection {
    /// Resolve the exact target views selected by this policy.
    pub fn effective(&self, workspace_targets: &[String]) -> Vec<String> {
        match self {
            Self::Workspace => {
                let mut targets = Vec::with_capacity(workspace_targets.len() + 1);
                targets.push("host".to_string());
                targets.extend(workspace_targets.iter().cloned());
                targets
            }
            Self::Explicit(targets) => targets.clone(),
        }
    }

    pub(crate) fn explicit(&self) -> Option<&[String]> {
        match self {
            Self::Workspace => None,
            Self::Explicit(targets) => Some(targets),
        }
    }

    pub(crate) fn inherits_workspace(&self) -> bool {
        matches!(self, Self::Workspace)
    }
}

/// One whole compiler crate kept outside closed-world authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceExternal {
    /// Exact Rust compiler crate name.
    #[serde(rename = "crate")]
    pub crate_name: String,
    /// Human-owned explanation for the external boundary.
    pub reason: String,
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
    /// Optional Cargo target name or `cfg(...)` selector.
    pub target: Option<String>,
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
    #[serde(default)]
    pub package: Option<String>,
    /// Exact Rust compiler crate name.
    #[serde(default, rename = "crate")]
    pub crate_name: Option<String>,
    /// Exact compiler diagnostic path.
    pub item: String,
    /// Exact declaration kind.
    pub kind: Option<String>,
    /// Optional Cargo target name or `cfg(...)` selector.
    pub target: Option<String>,
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
    #[serde(default)]
    pub package: Option<String>,
    /// Exact Rust compiler crate name.
    #[serde(default, rename = "crate")]
    pub crate_name: Option<String>,
    /// Compiler diagnostic module path.
    pub module: Option<String>,
    /// Repository-relative source file.
    pub file: Option<String>,
    /// Optional Cargo target name or `cfg(...)` selector.
    pub target: Option<String>,
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
    /// Include source-surface analysis in planner-selected CI.
    pub enabled: bool,
    /// Closed-world authority for non-publishable internal packages.
    pub consumer_scope: SurfaceConsumerScope,
    /// Required compiler target views, either an explicit subset or the workspace target policy.
    pub targets: SurfaceTargetSelection,
    /// Whether `pub(crate)` to `pub(super)` findings are enabled.
    pub crate_visibility: SurfaceCrateVisibility,
    /// Preserve uniform field visibility when visibility repair is enabled.
    pub preserve_uniform_fields: bool,
    /// Ordered global lint-level policy. Core findings deny by default.
    pub lint: Vec<SurfaceLintDirective>,
    /// Explicit complete production roots; workspace binaries are implicit when empty.
    pub product: Vec<SurfaceProduct>,
    /// Exact feature profiles; empty retains automatic complete coverage.
    #[serde(rename = "feature-profile")]
    pub feature_profile: Vec<SurfaceFeatureProfile>,
    /// Exact doctest package set; empty retains complete automatic coverage.
    pub doctest: Vec<SurfaceDoctest>,
    /// Behavior when no exact doctest package set is configured.
    pub doctest_coverage: SurfaceDoctestCoverage,
    /// Whole compiler crates kept open to external consumers.
    pub external: Vec<SurfaceExternal>,
    /// Item-specific diagnostic policies.
    pub r#override: Vec<SurfaceOverride>,
    /// Module- or file-scoped diagnostic exclusions.
    pub exclude: Vec<SurfaceExclude>,
}

impl Default for SurfaceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            consumer_scope: SurfaceConsumerScope::Open,
            targets: SurfaceTargetSelection::default(),
            crate_visibility: SurfaceCrateVisibility::Preserve,
            preserve_uniform_fields: false,
            lint: Vec::new(),
            product: Vec::new(),
            feature_profile: Vec::new(),
            doctest: Vec::new(),
            doctest_coverage: SurfaceDoctestCoverage::Automatic,
            external: Vec::new(),
            r#override: Vec::new(),
            exclude: Vec::new(),
        }
    }
}

impl SurfaceConfig {
    /// Validate policy syntax that does not require Cargo workspace state.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if let Some(targets) = self.targets.explicit() {
            if targets.is_empty() {
                return Err(invalid("surface.targets", "must contain at least one target view"));
            }
            validate_unique_non_empty(targets, "surface.targets")?;
        }

        for (index, directive) in self.lint.iter().enumerate() {
            let field = format!("surface.lint.{index}");
            if directive.selector != "warnings" {
                validate_lint(&directive.selector, &format!("{field}.selector"))?;
            }
            if directive.level == SurfaceLintLevel::Expect {
                return Err(invalid(field, "ordered lint policy cannot use 'expect'"));
            }
        }

        let mut products = BTreeSet::new();
        for (index, product) in self.product.iter().enumerate() {
            let field = format!("surface.product.{index}");
            validate_text(&product.package, &format!("{field}.package"))?;
            validate_text(&product.reason, &format!("{field}.reason"))?;
            validate_target_selector(product.target.as_deref(), &format!("{field}.target"))?;
            let target = match (&product.bin, &product.lib) {
                (Some(bin), None) => ("bin", bin),
                (None, Some(lib)) => ("lib", lib),
                _ => return Err(invalid(field, "requires exactly one of 'bin' or 'lib'")),
            };
            validate_text(target.1, &format!("{field}.{}", target.0))?;
            if !products.insert((&product.package, target.0, target.1, &product.target)) {
                return Err(invalid(field, "duplicates an earlier product root"));
            }
        }

        let mut profile_names = BTreeSet::new();
        for (index, profile) in self.feature_profile.iter().enumerate() {
            let field = format!("surface.feature-profile.{index}");
            validate_text(&profile.name, &format!("{field}.name"))?;
            if !profile
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            {
                return Err(invalid(
                    format!("{field}.name"),
                    "must use only ASCII letters, digits, '-' or '_'",
                ));
            }
            if !profile_names.insert(&profile.name) {
                return Err(invalid(field, "duplicates an earlier feature profile name"));
            }
            if profile.all_features && (profile.no_default_features || !profile.features.is_empty()) {
                return Err(invalid(
                    field,
                    "cannot combine 'all-features' with 'no-default-features' or explicit features",
                ));
            }
            validate_unique_non_empty(&profile.features, &format!("{field}.features"))?;
        }

        let doctests = self
            .doctest
            .iter()
            .map(|entry| entry.package.clone())
            .collect::<Vec<_>>();
        validate_unique_non_empty(&doctests, "surface.doctest.package")?;
        if self.doctest_coverage == SurfaceDoctestCoverage::Disabled && !self.doctest.is_empty() {
            return Err(invalid(
                "surface.doctest_coverage",
                "cannot be 'disabled' when explicit doctest packages are configured",
            ));
        }

        let mut external = BTreeSet::new();
        for (index, boundary) in self.external.iter().enumerate() {
            let field = format!("surface.external.{index}");
            validate_text(&boundary.crate_name, &format!("{field}.crate"))?;
            validate_text(&boundary.reason, &format!("{field}.reason"))?;
            if !external.insert(&boundary.crate_name) {
                return Err(invalid(field, "duplicates an earlier external crate boundary"));
            }
        }

        let mut overrides = BTreeSet::new();
        for (index, policy) in self.r#override.iter().enumerate() {
            let field = format!("surface.override.{index}");
            validate_lint(&policy.lint, &format!("{field}.lint"))?;
            validate_policy_owner(policy.package.as_deref(), policy.crate_name.as_deref(), &field)?;
            validate_text(&policy.item, &format!("{field}.item"))?;
            if policy
                .kind
                .as_deref()
                .is_some_and(|kind| !SURFACE_ITEM_KINDS.contains(&kind))
            {
                return Err(invalid(
                    format!("{field}.kind"),
                    format!(
                        "unknown declaration kind '{}'",
                        policy.kind.as_deref().unwrap_or_default()
                    ),
                ));
            }
            validate_target_selector(policy.target.as_deref(), &format!("{field}.target"))?;
            validate_text(&policy.reason, &format!("{field}.reason"))?;
            if !overrides.insert((
                &policy.lint,
                &policy.package,
                &policy.crate_name,
                &policy.item,
                &policy.kind,
                &policy.target,
            )) {
                return Err(invalid(field, "duplicates an earlier item override"));
            }
        }

        let mut exclusions = BTreeSet::new();
        for (index, policy) in self.exclude.iter().enumerate() {
            let field = format!("surface.exclude.{index}");
            validate_policy_owner(policy.package.as_deref(), policy.crate_name.as_deref(), &field)?;
            validate_text(&policy.reason, &format!("{field}.reason"))?;
            let selector = match (&policy.module, &policy.file) {
                (Some(module), None) => ("module", module),
                (None, Some(file)) => ("file", file),
                _ => return Err(invalid(field, "requires exactly one of 'module' or 'file'")),
            };
            validate_text(selector.1, &format!("{field}.{}", selector.0))?;
            validate_target_selector(policy.target.as_deref(), &format!("{field}.target"))?;
            if !exclusions.insert((
                &policy.package,
                &policy.crate_name,
                selector.0,
                selector.1,
                &policy.target,
            )) {
                return Err(invalid(field, "duplicates an earlier exclusion"));
            }
        }
        Ok(())
    }

    /// Validate an explicit Surface subset against the top-level target authority.
    pub fn validate_workspace_targets(&self, workspace_targets: &[String]) -> Result<(), ConfigError> {
        let Some(targets) = self.targets.explicit() else {
            return Ok(());
        };
        for target in targets.iter().filter(|target| target.as_str() != "host") {
            if !workspace_targets.contains(target) {
                return Err(invalid(
                    "surface.targets",
                    format!("target '{target}' is not declared in top-level targets"),
                ));
            }
        }
        Ok(())
    }
}

fn validate_policy_owner(package: Option<&str>, crate_name: Option<&str>, field: &str) -> Result<(), ConfigError> {
    match (package, crate_name) {
        (Some(package), None) => validate_text(package, &format!("{field}.package")),
        (None, Some(crate_name)) => validate_text(crate_name, &format!("{field}.crate")),
        _ => Err(invalid(field, "requires exactly one of 'package' or 'crate'")),
    }
}

fn validate_target_selector(target: Option<&str>, field: &str) -> Result<(), ConfigError> {
    let Some(target) = target else {
        return Ok(());
    };
    validate_text(target, field)?;
    target
        .parse::<cargo_platform::Platform>()
        .map(|_| ())
        .map_err(|error| invalid(field, format!("invalid Cargo target selector '{target}': {error}")))
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
    fn planner_enforcement_requires_explicit_enablement() {
        assert!(!SurfaceConfig::default().enabled);
        let config: SurfaceConfig = toml_edit::de::from_str("enabled = true").unwrap();
        assert!(config.enabled);
    }

    #[test]
    fn workspace_target_selection_includes_host_and_top_level_targets() {
        let config: SurfaceConfig = toml_edit::de::from_str("targets = \"workspace\"").unwrap();
        assert_eq!(
            config
                .targets
                .effective(&["aarch64-unknown-linux-gnu".to_string(), "wasm32-wasip1".to_string()]),
            ["host", "aarch64-unknown-linux-gnu", "wasm32-wasip1"]
        );
        config.validate().unwrap();
    }

    #[test]
    fn explicit_target_selection_must_be_a_workspace_subset() {
        let config: SurfaceConfig = toml_edit::de::from_str("targets = [\"host\", \"wasm32-wasip1\"]").unwrap();
        config
            .validate_workspace_targets(&["wasm32-wasip1".to_string()])
            .unwrap();
        assert!(config.validate_workspace_targets(&[]).is_err());
    }

    #[test]
    fn unknown_target_selection_mode_is_rejected() {
        toml_edit::de::from_str::<SurfaceConfig>("targets = \"automatic\"").unwrap_err();
    }

    #[test]
    fn product_and_filter_selectors_are_exact() {
        let mut config = SurfaceConfig::default();
        config.product.push(SurfaceProduct {
            package: "app".to_string(),
            bin: Some("app".to_string()),
            lib: None,
            target: None,
            reason: "shipped application".to_string(),
        });
        config.r#override.push(SurfaceOverride {
            lint: "unnecessary-public".to_string(),
            package: Some("app".to_string()),
            crate_name: None,
            item: "migration::legacy".to_string(),
            kind: Some("function".to_string()),
            target: None,
            level: SurfaceLintLevel::Expect,
            reason: "migration compatibility".to_string(),
        });
        config.exclude.push(SurfaceExclude {
            package: Some("app".to_string()),
            crate_name: None,
            module: Some("generated".to_string()),
            file: None,
            target: None,
            level: SurfaceLintLevel::Expect,
            reason: "generated source".to_string(),
        });
        config.validate().unwrap();

        config.product[0].lib = Some("app".to_string());
        assert!(config.validate().is_err());
    }

    #[test]
    fn unknown_lints_and_empty_reasons_are_rejected() {
        let mut config = SurfaceConfig::default();
        config.r#override.push(SurfaceOverride {
            lint: "dead-code".to_string(),
            package: Some("app".to_string()),
            crate_name: None,
            item: "legacy".to_string(),
            kind: Some("function".to_string()),
            target: None,
            level: SurfaceLintLevel::Allow,
            reason: "".to_string(),
        });
        assert!(config.validate().is_err());
    }
}
