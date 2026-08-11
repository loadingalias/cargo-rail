//! Machine-owned remote target validation and doctor probing.
//!
//! Transparent compiler reuse is local-only. The command-owned remote
//! coordinator was removed with runner cache ownership; a future remote
//! transport can consume the retained compiler-result protocol directly.

mod s3;

use std::fmt;
use std::path::Path;

use serde::Serialize;

const MAX_APPROVED_ENVIRONMENT_NAMES: usize = 512;
const MAX_APPROVED_ENVIRONMENT_NAME_BYTES: usize = 256;
const MAX_APPROVED_ENVIRONMENT_TOTAL_BYTES: usize = 32 * 1024;

pub(crate) const TARGETS_ENV: &str = s3::TARGETS_ENV;
/// One redacted remote-target failure.
#[derive(Debug, Clone)]
pub(crate) struct RemoteStoreError {
  message: String,
}

impl RemoteStoreError {
  pub(super) fn unavailable(message: impl Into<String>) -> Self {
    Self::new(message)
  }

  pub(super) fn configuration(message: impl Into<String>) -> Self {
    Self::new(message)
  }

  fn new(message: impl Into<String>) -> Self {
    Self {
      message: message.into(),
    }
  }
}

impl fmt::Display for RemoteStoreError {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter.write_str(&self.message)
  }
}

impl std::error::Error for RemoteStoreError {}

pub(super) type RemoteStoreResult<T> = Result<T, RemoteStoreError>;

/// Redacted projection of one selected machine-owned target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RemoteCacheConfigurationStatus {
  pub(crate) alias: String,
  pub(crate) transport: &'static str,
  pub(crate) authority: String,
  pub(crate) role: &'static str,
  pub(crate) shared_environment_names: usize,
  pub(crate) activation: &'static str,
}

pub(crate) fn configuration_status(
  source_root: &Path,
  alias: Option<&str>,
) -> RemoteStoreResult<Option<RemoteCacheConfigurationStatus>> {
  let Some(alias) = alias else {
    return Ok(None);
  };
  let target = s3::S3Target::load(source_root, alias)?;
  Ok(Some(status(alias, &target)))
}

fn status(alias: &str, target: &s3::S3Target) -> RemoteCacheConfigurationStatus {
  RemoteCacheConfigurationStatus {
    alias: alias.to_string(),
    transport: "s3",
    authority: target.authority().as_str().to_string(),
    role: target.role_name(),
    shared_environment_names: target.shareable_environment_names().len(),
    activation: "configuration_only_transparent_cache_is_local",
  }
}

pub(super) fn strictly_sorted_unique(values: &[String]) -> bool {
  values.windows(2).all(|pair| pair[0] < pair[1])
}

pub(super) fn valid_environment_name(name: &str) -> bool {
  !name.is_empty()
    && name.len() <= MAX_APPROVED_ENVIRONMENT_NAME_BYTES
    && !name.as_bytes().contains(&0)
    && !name.contains('=')
    && !name.chars().any(char::is_control)
}

pub(super) fn environment_name_may_be_secret(name: &str) -> bool {
  let name = name.to_ascii_uppercase();
  matches!(
    name.as_str(),
    "SSH_AUTH_SOCK"
      | "GPG_AGENT_INFO"
      | "DOCKER_AUTH_CONFIG"
      | "GOOGLE_APPLICATION_CREDENTIALS"
      | "AWS_ACCESS_KEY_ID"
      | "AWS_SECRET_ACCESS_KEY"
      | "AWS_SESSION_TOKEN"
  ) || [
    "_TOKEN",
    "_SECRET",
    "_PASSWORD",
    "_PASSWD",
    "_PRIVATE_KEY",
    "_ACCESS_KEY",
    "_CREDENTIAL",
    "_CREDENTIALS",
    "_AUTHORIZATION",
  ]
  .iter()
  .any(|suffix| name.ends_with(suffix))
}

pub(super) fn validate_environment_names(names: &[String]) -> RemoteStoreResult<()> {
  if names.len() > MAX_APPROVED_ENVIRONMENT_NAMES
    || !strictly_sorted_unique(names)
    || names.iter().any(|name| !valid_environment_name(name))
    || names
      .iter()
      .try_fold(0_usize, |total, name| total.checked_add(name.len()))
      .is_none_or(|bytes| bytes > MAX_APPROVED_ENVIRONMENT_TOTAL_BYTES)
  {
    return Err(RemoteStoreError::configuration(
      "cache target shared environment is invalid",
    ));
  }
  Ok(())
}
