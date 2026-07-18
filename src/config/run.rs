//! Run/execution profile configuration.

use crate::error::ConfigError;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Deserializer, Serialize, de};

const BUILTIN_PROFILE_NAMES: &[&str] = &["local", "ci", "nightly"];
const SUPPORTED_SURFACES: &[&str] = &["build", "test", "bench", "docs"];
const ALLOWED_RUN_ARG_TOKENS: &[&str] = &["workspace_root", "base_ref", "cargo_args"];
const ALLOWED_SINCE_TOKENS: &[&str] = &["workspace_root", "base_ref"];

/// Executor profile configuration for `cargo rail run`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RunConfig {
  /// Optional default profile when `cargo rail run` is invoked without `--surface` or `--profile`.
  #[serde(default)]
  pub default_profile: Option<String>,
  /// User-defined profiles keyed by profile name.
  #[serde(default, rename = "profile")]
  pub profiles: FxHashMap<String, RunProfile>,
  /// Optional workflow-to-profile mapping (for CI wrappers and conventions).
  #[serde(default)]
  pub workflow: FxHashMap<String, String>,
}

/// A named run profile.
#[derive(Debug, Clone, Serialize, Default)]
pub struct RunProfile {
  /// Surfaces executed by this profile.
  #[serde(default)]
  pub surfaces: Vec<String>,
  /// Arguments prepended to command-line `RUN_ARGS`.
  #[serde(default)]
  pub run_args: Vec<String>,
  /// Optional baseline policy if the CLI does not provide one.
  #[serde(default)]
  pub baseline: Option<RunBaseline>,
}

/// One valid baseline policy for a run profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RunBaseline {
  /// Use one explicit Git reference or supported placeholder.
  Since {
    /// Git reference or `{base_ref}` placeholder.
    reference: String,
  },
  /// Resolve the merge base against the default branch.
  MergeBase,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RunProfileInput {
  surfaces: Vec<String>,
  run_args: Vec<String>,
  baseline: Option<RunBaseline>,
  since: Option<String>,
  merge_base: Option<bool>,
}

impl<'de> Deserialize<'de> for RunProfile {
  fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
  where
    D: Deserializer<'de>,
  {
    let input = RunProfileInput::deserialize(deserializer)?;
    if input.baseline.is_some() && (input.since.is_some() || input.merge_base.is_some()) {
      return Err(de::Error::custom(
        "run profile baseline cannot be combined with deprecated since or merge_base; run `cargo rail config migrate`",
      ));
    }
    if input.since.is_some() && input.merge_base == Some(true) {
      return Err(de::Error::custom(
        "deprecated run profile since and merge_base = true are mutually exclusive",
      ));
    }
    let baseline = input.baseline.or_else(|| {
      input
        .since
        .map(|reference| RunBaseline::Since { reference })
        .or_else(|| (input.merge_base == Some(true)).then_some(RunBaseline::MergeBase))
    });
    Ok(Self {
      surfaces: input.surfaces,
      run_args: input.run_args,
      baseline,
    })
  }
}

impl RunConfig {
  /// Validate run profile configuration.
  pub fn validate(&self) -> Result<(), ConfigError> {
    if let Some(default_profile) = self.default_profile.as_deref()
      && !self.profiles.contains_key(default_profile)
      && !is_builtin_profile(default_profile)
    {
      return Err(ConfigError::InvalidField {
        field: "run.default_profile".to_string(),
        reason: format!(
          "unknown profile '{}'; define [run.profile.{}] or use one of: {}",
          default_profile,
          default_profile,
          BUILTIN_PROFILE_NAMES.join(", ")
        ),
      });
    }

    for (name, profile) in &self.profiles {
      profile.validate(name)?;
    }

    for (workflow_name, profile_name) in &self.workflow {
      if self.profiles.contains_key(profile_name) || is_builtin_profile(profile_name) {
        continue;
      }

      return Err(ConfigError::InvalidField {
        field: format!("run.workflow.{}", workflow_name),
        reason: format!(
          "unknown profile '{}'; define [run.profile.{}] or use one of: {}",
          profile_name,
          profile_name,
          BUILTIN_PROFILE_NAMES.join(", ")
        ),
      });
    }

    Ok(())
  }
}

impl RunProfile {
  fn validate(&self, profile_name: &str) -> Result<(), ConfigError> {
    if self.surfaces.is_empty() {
      return Err(ConfigError::InvalidField {
        field: format!("run.profile.{}.surfaces", profile_name),
        reason: "must contain at least one surface".to_string(),
      });
    }

    for surface in &self.surfaces {
      if surface == "infra" {
        return Err(ConfigError::InvalidField {
          field: format!("run.profile.{}.surfaces", profile_name),
          reason: format!(
            "invalid surface '{}'\n\n\
             `infra` is a planner OUTPUT for CI gating, not a run surface.\n\
             Valid profile surfaces: {}\n\n\
             To gate CI jobs on infra, extract it from plan JSON output:\n  \
             INFRA=$(echo \"$PLAN_JSON\" | jq -r '.surfaces.infra.enabled')",
            surface,
            SUPPORTED_SURFACES.join(", ")
          ),
        });
      }

      // custom:* surfaces are plan OUTPUTS, not profile inputs
      if surface.starts_with("custom:") {
        return Err(ConfigError::InvalidField {
          field: format!("run.profile.{}.surfaces", profile_name),
          reason: format!(
            "invalid surface '{}'\n\n\
             Custom surfaces are plan OUTPUTS for CI gating, not profile inputs.\n\
             Valid profile surfaces: {}\n\n\
             To gate CI jobs on custom surfaces, extract from plan JSON output:\n  \
             WORKLOADS=$(echo \"$PLAN_JSON\" | jq -r '.surfaces[\"custom:workloads\"].enabled')",
            surface,
            SUPPORTED_SURFACES.join(", ")
          ),
        });
      }

      if !SUPPORTED_SURFACES.contains(&surface.as_str()) {
        return Err(ConfigError::InvalidField {
          field: format!("run.profile.{}.surfaces", profile_name),
          reason: format!(
            "unknown surface '{}'; supported surfaces: {}",
            surface,
            SUPPORTED_SURFACES.join(", ")
          ),
        });
      }
    }

    if let Some(RunBaseline::Since { reference }) = &self.baseline {
      validate_tokens(
        reference,
        ALLOWED_SINCE_TOKENS,
        &format!("run.profile.{}.baseline.reference", profile_name),
      )?;
    }

    for (index, arg) in self.run_args.iter().enumerate() {
      validate_tokens(
        arg,
        ALLOWED_RUN_ARG_TOKENS,
        &format!("run.profile.{}.run_args[{}]", profile_name, index),
      )?;
    }

    Ok(())
  }
}

/// Returns true when name is one of the built-in profiles.
pub fn is_builtin_profile(name: &str) -> bool {
  BUILTIN_PROFILE_NAMES.contains(&name)
}

fn validate_tokens(value: &str, allowed: &[&str], field: &str) -> Result<(), ConfigError> {
  for token in extract_tokens(value) {
    if !allowed.contains(&token.as_str()) {
      return Err(ConfigError::InvalidField {
        field: field.to_string(),
        reason: format!("unknown token '{{{}}}'; allowed tokens: {}", token, allowed.join(", ")),
      });
    }
  }
  Ok(())
}

fn extract_tokens(value: &str) -> Vec<String> {
  let mut tokens = Vec::new();
  let bytes = value.as_bytes();
  let mut i = 0;
  while i < bytes.len() {
    if bytes[i] == b'{' {
      let start = i + 1;
      if let Some(end_rel) = bytes[start..].iter().position(|b| *b == b'}') {
        let end = start + end_rel;
        if end > start {
          let token = &value[start..end];
          if token
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
          {
            tokens.push(token.to_string());
          }
        }
        i = end + 1;
        continue;
      }
    }
    i += 1;
  }
  tokens
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn validate_rejects_empty_surfaces() {
    let mut cfg = RunConfig::default();
    cfg.profiles.insert("custom".to_string(), RunProfile::default());

    let err = cfg.validate().expect_err("profile without surfaces should fail");
    assert!(err.to_string().contains("must contain at least one surface"));
  }

  #[test]
  fn validate_accepts_builtin_default_profile() {
    let cfg = RunConfig {
      default_profile: Some("local".to_string()),
      ..RunConfig::default()
    };

    assert!(cfg.validate().is_ok());
  }

  #[test]
  fn validate_rejects_unknown_default_profile() {
    let cfg = RunConfig {
      default_profile: Some("missing".to_string()),
      ..RunConfig::default()
    };

    let err = cfg.validate().expect_err("unknown default profile should fail");
    assert!(err.to_string().contains("unknown profile 'missing'"));
  }

  #[test]
  fn validate_rejects_unknown_run_arg_token() {
    let mut cfg = RunConfig::default();
    cfg.profiles.insert(
      "custom".to_string(),
      RunProfile {
        surfaces: vec!["test".to_string()],
        run_args: vec!["--manifest-path".to_string(), "{unknown}".to_string()],
        ..RunProfile::default()
      },
    );
    let err = cfg
      .validate()
      .expect_err("unknown run_args token should fail validation");
    assert!(err.to_string().contains("unknown token '{unknown}'"));
  }

  #[test]
  fn validate_rejects_unknown_since_token() {
    let mut cfg = RunConfig::default();
    cfg.profiles.insert(
      "custom".to_string(),
      RunProfile {
        surfaces: vec!["test".to_string()],
        baseline: Some(RunBaseline::Since {
          reference: "{cargo_args}".to_string(),
        }),
        ..RunProfile::default()
      },
    );
    let err = cfg.validate().expect_err("unknown since token should fail validation");
    assert!(err.to_string().contains("unknown token '{cargo_args}'"));
  }

  #[test]
  fn baseline_type_cannot_represent_conflicting_modes() {
    let err = toml_edit::de::from_str::<RunProfile>("surfaces = [\"test\"]\nsince = \"HEAD~1\"\nmerge_base = true\n")
      .unwrap_err();
    assert!(err.to_string().contains("mutually exclusive"));
  }

  #[test]
  fn valid_legacy_baselines_map_to_typed_policy() {
    let profile: RunProfile = toml_edit::de::from_str("surfaces = [\"test\"]\nmerge_base = true\n").unwrap();
    assert_eq!(profile.baseline, Some(RunBaseline::MergeBase));

    let profile: RunProfile = toml_edit::de::from_str("surfaces = [\"test\"]\nsince = \"HEAD~1\"\n").unwrap();
    assert_eq!(
      profile.baseline,
      Some(RunBaseline::Since {
        reference: "HEAD~1".to_string()
      })
    );
  }

  #[test]
  fn validate_rejects_custom_surface_in_profile() {
    let mut cfg = RunConfig::default();
    cfg.profiles.insert(
      "ci".to_string(),
      RunProfile {
        surfaces: vec!["custom:workloads".to_string()],
        ..RunProfile::default()
      },
    );
    let err = cfg.validate().expect_err("custom surface in profile should fail");
    let msg = err.to_string();
    assert!(msg.contains("invalid surface 'custom:workloads'"));
    assert!(msg.contains("plan OUTPUTS"));
  }

  #[test]
  fn validate_rejects_infra_surface_in_profile() {
    let mut cfg = RunConfig::default();
    cfg.profiles.insert(
      "ci".to_string(),
      RunProfile {
        surfaces: vec!["infra".to_string()],
        ..RunProfile::default()
      },
    );
    let err = cfg.validate().expect_err("infra surface in profile should fail");
    let msg = err.to_string();
    assert!(msg.contains("invalid surface 'infra'"));
    assert!(msg.contains("planner OUTPUT"));
  }

  #[test]
  fn validate_rejects_workflow_mapping_to_missing_profile() {
    let mut cfg = RunConfig::default();
    cfg.workflow.insert("commit".to_string(), "missing".to_string());
    let err = cfg.validate().expect_err("missing profile mapping should fail");
    assert!(err.to_string().contains("unknown profile 'missing'"));
  }
}
