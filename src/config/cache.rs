//! Build-result cache policy.

use crate::error::ConfigError;
use serde::{Deserialize, Serialize};

const MAX_L2_ALIAS_BYTES: usize = 64;

/// Repository policy for Cargo-Rail build-result caching.
///
/// Transparent local reuse is installed as machine state. An optional L2 alias
/// selects authority from machine-owned configuration; repository configuration
/// never contains storage locations or credentials.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheConfig {
  /// Optional machine-owned shared-cache target alias.
  #[serde(default)]
  pub l2: Option<String>,
}

impl CacheConfig {
  /// Validate repository-owned cache policy.
  pub fn validate(&self) -> Result<(), ConfigError> {
    if let Some(alias) = &self.l2 {
      validate_l2_alias(alias)?;
    }
    Ok(())
  }
}

fn validate_l2_alias(alias: &str) -> Result<(), ConfigError> {
  let bytes = alias.as_bytes();
  if bytes.is_empty()
    || bytes.len() > MAX_L2_ALIAS_BYTES
    || !bytes[0].is_ascii_lowercase()
    || !bytes
      .iter()
      .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_'))
  {
    return Err(ConfigError::InvalidField {
      field: "cache.l2".to_string(),
      reason:
        "must start with a lowercase ASCII letter and contain only lowercase letters, digits, '-' or '_' (64 bytes max)"
          .to_string(),
    });
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::config::RailConfig;

  #[test]
  fn cache_defaults_to_local_reuse_without_l2() {
    let cache = CacheConfig::default();
    assert_eq!(cache.l2, None);

    let config: RailConfig = toml_edit::de::from_str("").expect("empty configuration");
    assert_eq!(config.cache, cache);
  }

  #[test]
  fn cache_policy_parses_sparse_repository_choices() {
    let config: RailConfig = toml_edit::de::from_str(
      r#"
[cache]
l2 = "team_2"
"#,
    )
    .expect("cache configuration");
    assert_eq!(config.cache.l2.as_deref(), Some("team_2"));
    config.cache.validate().expect("valid cache policy");
  }

  #[test]
  fn l2_alias_accepts_only_the_canonical_bounded_form() {
    for alias in ["a", "team", "team-2", "team_2", &format!("a{}", "0".repeat(63))] {
      CacheConfig {
        l2: Some(alias.to_string()),
      }
      .validate()
      .expect("canonical alias");
    }

    for alias in [
      "",
      "2team",
      "Team",
      "team.cache",
      "team/cache",
      "téam",
      &format!("a{}", "0".repeat(64)),
    ] {
      let error = CacheConfig {
        l2: Some(alias.to_string()),
      }
      .validate()
      .expect_err("noncanonical alias");
      assert!(matches!(error, ConfigError::InvalidField { ref field, .. } if field == "cache.l2"));
    }
  }

  #[test]
  fn unknown_cache_data_keeps_the_existing_non_strict_parse_contract() {
    let config: RailConfig = toml_edit::de::from_str(
      r#"
[cache]
l2 = "team"
future_policy = "retained by lossless tooling"
"#,
    )
    .expect("unknown fields remain a validation concern");
    assert_eq!(config.cache.l2.as_deref(), Some("team"));
  }
}
