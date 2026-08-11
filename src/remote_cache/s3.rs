//! Strict validation for retained, configuration-only S3 target declarations.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use sha2::{Digest as _, Sha256};

use super::{RemoteStoreError, RemoteStoreResult, environment_name_may_be_secret, validate_environment_names};
use crate::compiler::native_cache::RemoteAuthorityId;
use crate::source::ContentDigest;

pub(crate) const TARGETS_ENV: &str = "CARGO_RAIL_CACHE_TARGETS_FILE";
const TARGETS_VERSION: u32 = 1;
const TARGETS_MAX_BYTES: u64 = 64 * 1024;
const TARGETS_MAX_ENTRIES: usize = 64;
const MAX_ALIAS_BYTES: usize = 64;
const MAX_REGION_BYTES: usize = 64;
const MAX_BUCKET_BYTES: usize = 63;
const MAX_PREFIX_BYTES: usize = 512;
const AUTHORITY_DOMAIN: &[u8] = b"cargo-rail-configuration-only-s3-authority-v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum S3Role {
  Read,
  ReadWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Protocol {
  S3,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetWire {
  protocol: Protocol,
  region: String,
  expected_bucket_owner: String,
  bucket: String,
  prefix: String,
  role: S3Role,
  shareable_environment: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetMap {
  version: u32,
  targets: TargetEntries,
}

struct TargetEntries(BTreeMap<String, TargetWire>);

impl<'de> Deserialize<'de> for TargetEntries {
  fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
  where
    D: Deserializer<'de>,
  {
    struct EntriesVisitor;

    impl<'de> Visitor<'de> for EntriesVisitor {
      type Value = TargetEntries;

      fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded map of unique cache targets")
      }

      fn visit_map<A>(self, mut entries: A) -> Result<Self::Value, A::Error>
      where
        A: MapAccess<'de>,
      {
        let mut targets = BTreeMap::new();
        while let Some((alias, target)) = entries.next_entry::<String, TargetWire>()? {
          if targets.len() >= TARGETS_MAX_ENTRIES {
            return Err(serde::de::Error::custom("cache target map exceeds its entry bound"));
          }
          if targets.insert(alias, target).is_some() {
            return Err(serde::de::Error::custom("cache target map contains a duplicate alias"));
          }
        }
        Ok(TargetEntries(targets))
      }
    }

    deserializer.deserialize_map(EntriesVisitor)
  }
}

/// Redacted configuration authority for one declared classic S3 namespace.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct S3Target {
  role: S3Role,
  shareable_environment_names: Vec<String>,
  authority: RemoteAuthorityId,
}

impl fmt::Debug for S3Target {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter
      .debug_struct("S3Target")
      .field("authority", &self.authority.as_str())
      .field("role", &self.role)
      .field("shareable_environment_names", &self.shareable_environment_names.len())
      .finish()
  }
}

impl S3Target {
  pub(crate) fn load(source_root: &Path, alias: &str) -> RemoteStoreResult<Self> {
    let path = std::env::var_os(TARGETS_ENV)
      .filter(|path| !path.is_empty())
      .map(PathBuf::from)
      .ok_or_else(|| RemoteStoreError::unavailable("cache target map is not configured"))?;
    Self::load_from_path(source_root, alias, &path)
  }

  fn load_from_path(source_root: &Path, alias: &str, path: &Path) -> RemoteStoreResult<Self> {
    validate_alias(alias)?;
    let bytes = read_target_map(source_root, path)?;
    let mut target_map = serde_json::from_slice::<TargetMap>(&bytes)
      .map_err(|_| RemoteStoreError::configuration("cache target map is invalid"))?;
    if target_map.version != TARGETS_VERSION || target_map.targets.0.is_empty() {
      return Err(RemoteStoreError::configuration(
        "cache target map has an incompatible schema",
      ));
    }
    if target_map
      .targets
      .0
      .keys()
      .any(|candidate| validate_alias(candidate).is_err())
    {
      return Err(RemoteStoreError::configuration(
        "cache target map contains an invalid alias",
      ));
    }
    let wire = target_map
      .targets
      .0
      .remove(alias)
      .ok_or_else(|| RemoteStoreError::unavailable("selected cache target is not configured"))?;
    Self::from_wire(wire)
  }

  fn from_wire(wire: TargetWire) -> RemoteStoreResult<Self> {
    if wire.protocol != Protocol::S3 {
      return Err(RemoteStoreError::configuration(
        "selected cache target has an unsupported protocol",
      ));
    }
    validate_region(&wire.region)?;
    validate_expected_owner(&wire.expected_bucket_owner)?;
    validate_bucket(&wire.bucket)?;
    let prefix = normalize_prefix(&wire.prefix)?;
    validate_environment_names(&wire.shareable_environment)?;
    if wire
      .shareable_environment
      .iter()
      .any(|name| environment_name_may_be_secret(name))
    {
      return Err(RemoteStoreError::configuration(
        "cache target environment sharing policy is invalid",
      ));
    }
    let authority = authority_id(&wire.region, &wire.expected_bucket_owner, &wire.bucket, &prefix)?;
    Ok(Self {
      role: wire.role,
      shareable_environment_names: wire.shareable_environment,
      authority,
    })
  }

  pub(crate) fn authority(&self) -> &RemoteAuthorityId {
    &self.authority
  }

  pub(crate) const fn role_name(&self) -> &'static str {
    match self.role {
      S3Role::Read => "read",
      S3Role::ReadWrite => "read_write",
    }
  }

  pub(crate) fn shareable_environment_names(&self) -> &[String] {
    &self.shareable_environment_names
  }
}

fn read_target_map(source_root: &Path, path: &Path) -> RemoteStoreResult<Vec<u8>> {
  if !path.is_absolute() {
    return Err(RemoteStoreError::configuration(
      "cache target map must be an absolute machine-owned file",
    ));
  }
  let metadata = fs::symlink_metadata(path).map_err(|error| {
    if error.kind() == std::io::ErrorKind::NotFound {
      RemoteStoreError::unavailable("cache target map is unavailable")
    } else {
      io_configuration(error)
    }
  })?;
  if !metadata.is_file()
    || crate::utils::is_symlink_or_reparse(&metadata)
    || metadata.len() == 0
    || metadata.len() > TARGETS_MAX_BYTES
  {
    return Err(RemoteStoreError::configuration(
      "cache target map is not one bounded regular file",
    ));
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt as _;

    if metadata.mode() & 0o022 != 0 {
      return Err(RemoteStoreError::configuration(
        "cache target map is writable by another OS principal",
      ));
    }
  }
  let canonical_path = fs::canonicalize(path).map_err(io_configuration)?;
  let canonical_source = fs::canonicalize(source_root).map_err(io_configuration)?;
  if canonical_path.starts_with(&canonical_source) {
    return Err(RemoteStoreError::configuration(
      "cache target map must live outside the source checkout",
    ));
  }
  let file = File::open(path).map_err(io_configuration)?;
  if !crate::utils::private_file_matches_path(&file, path, metadata.len()).map_err(io_configuration)? {
    return Err(RemoteStoreError::configuration(
      "cache target map changed while it was opened",
    ));
  }
  let mut bytes = Vec::with_capacity(metadata.len() as usize);
  (&file)
    .take(TARGETS_MAX_BYTES + 1)
    .read_to_end(&mut bytes)
    .map_err(io_configuration)?;
  if bytes.len() as u64 != metadata.len()
    || !crate::utils::private_file_matches_path(&file, path, metadata.len()).map_err(io_configuration)?
  {
    return Err(RemoteStoreError::configuration(
      "cache target map changed while it was read",
    ));
  }
  Ok(bytes)
}

fn validate_alias(alias: &str) -> RemoteStoreResult<()> {
  let mut bytes = alias.bytes();
  if alias.is_empty()
    || alias.len() > MAX_ALIAS_BYTES
    || !bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
    || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_'))
  {
    Err(RemoteStoreError::configuration("cache target alias is invalid"))
  } else {
    Ok(())
  }
}

fn validate_region(region: &str) -> RemoteStoreResult<()> {
  if region.is_empty()
    || region.len() > MAX_REGION_BYTES
    || !region
      .bytes()
      .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    || !region.as_bytes()[0].is_ascii_alphanumeric()
    || !region.as_bytes()[region.len() - 1].is_ascii_alphanumeric()
    || region.contains("--")
  {
    Err(RemoteStoreError::configuration("cache target region is invalid"))
  } else {
    Ok(())
  }
}

fn validate_expected_owner(owner: &str) -> RemoteStoreResult<()> {
  if owner.len() != 12 || !owner.bytes().all(|byte| byte.is_ascii_digit()) {
    Err(RemoteStoreError::configuration(
      "cache target expected bucket owner is invalid",
    ))
  } else {
    Ok(())
  }
}

fn validate_bucket(bucket: &str) -> RemoteStoreResult<()> {
  let valid_length = (3..=MAX_BUCKET_BYTES).contains(&bucket.len());
  let valid_bytes = bucket
    .bytes()
    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.'));
  let valid_edges = bucket
    .as_bytes()
    .first()
    .zip(bucket.as_bytes().last())
    .is_some_and(|(first, last)| first.is_ascii_alphanumeric() && last.is_ascii_alphanumeric());
  let reserved = bucket.starts_with("xn--")
    || bucket.starts_with("sthree-")
    || bucket.starts_with("amzn_s3_demo_")
    || ["-s3alias", "--ol-s3", ".mrap", "--x-s3", "--table-s3"]
      .iter()
      .any(|suffix| bucket.ends_with(suffix));
  if !valid_length
    || !valid_bytes
    || !valid_edges
    || bucket.contains("..")
    || bucket
      .split('.')
      .any(|label| label.is_empty() || label.starts_with('-') || label.ends_with('-'))
    || looks_like_ipv4(bucket)
    || reserved
  {
    Err(RemoteStoreError::configuration(
      "cache target bucket is not a classic DNS bucket name",
    ))
  } else {
    Ok(())
  }
}

fn looks_like_ipv4(value: &str) -> bool {
  let mut count = 0_usize;
  for part in value.split('.') {
    if part.is_empty() || part.len() > 3 || !part.bytes().all(|byte| byte.is_ascii_digit()) {
      return false;
    }
    if part.parse::<u8>().is_err() {
      return false;
    }
    count += 1;
  }
  count == 4
}

fn normalize_prefix(prefix: &str) -> RemoteStoreResult<String> {
  let normalized = prefix.trim_matches('/');
  if normalized.len() > MAX_PREFIX_BYTES
    || normalized.as_bytes().contains(&0)
    || normalized.chars().any(char::is_control)
    || (!normalized.is_empty()
      && normalized
        .split('/')
        .any(|segment| segment.is_empty() || matches!(segment, "." | "..")))
  {
    return Err(RemoteStoreError::configuration("cache target prefix is invalid"));
  }
  Ok(normalized.to_string())
}

fn authority_id(region: &str, owner: &str, bucket: &str, prefix: &str) -> RemoteStoreResult<RemoteAuthorityId> {
  let mut hasher = Sha256::new();
  hasher.update(AUTHORITY_DOMAIN);
  for (tag, value) in [
    (b"protocol".as_slice(), b"s3".as_slice()),
    (b"region".as_slice(), region.as_bytes()),
    (b"expected-owner".as_slice(), owner.as_bytes()),
    (b"bucket".as_slice(), bucket.as_bytes()),
    (b"prefix".as_slice(), prefix.as_bytes()),
  ] {
    hasher.update((tag.len() as u32).to_le_bytes());
    hasher.update(tag);
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
  }
  RemoteAuthorityId::parse(format!(
    "remote-authority-v1-sha256-{}",
    ContentDigest::from_sha256_bytes(hasher.finalize().into())
  ))
  .map_err(|_| RemoteStoreError::configuration("cache target authority could not be derived"))
}

fn io_configuration(_error: std::io::Error) -> RemoteStoreError {
  RemoteStoreError::configuration("cache target map could not be read safely")
}

#[cfg(test)]
mod tests {
  use std::fs;

  use super::*;

  fn wire(role: S3Role, names: &[&str]) -> TargetWire {
    TargetWire {
      protocol: Protocol::S3,
      region: "us-east-1".to_string(),
      expected_bucket_owner: "123456789012".to_string(),
      bucket: "cargo-rail-cache-fixture".to_string(),
      prefix: "/teams/example/".to_string(),
      role,
      shareable_environment: names.iter().map(|name| (*name).to_string()).collect(),
    }
  }

  #[test]
  fn target_authority_excludes_alias_role_and_environment_policy() {
    let read = S3Target::from_wire(wire(S3Role::Read, &[])).expect("read target");
    let write = S3Target::from_wire(wire(S3Role::ReadWrite, &["RUSTFLAGS"])).expect("write target");
    assert_eq!(read.authority(), write.authority());
    assert_eq!(read.role_name(), "read");
    assert_eq!(write.role_name(), "read_write");
  }

  #[test]
  fn target_debug_is_location_redacted() {
    let target = S3Target::from_wire(wire(S3Role::ReadWrite, &["RUSTFLAGS"])).expect("target");
    let debug = format!("{target:?}");
    assert!(!debug.contains("us-east-1"));
    assert!(!debug.contains("cargo-rail-cache-fixture"));
    assert!(!debug.contains("teams/example"));
    assert!(debug.contains(target.authority().as_str()));
  }

  #[test]
  fn target_rejects_nonclassic_bucket_and_sensitive_environment() {
    let mut invalid_bucket = wire(S3Role::Read, &[]);
    invalid_bucket.bucket = "arn:aws:s3:::cache".to_string();
    assert!(S3Target::from_wire(invalid_bucket).is_err());
    assert!(S3Target::from_wire(wire(S3Role::Read, &["CI_TOKEN"])).is_err());
  }

  #[test]
  fn alias_validation_is_exact() {
    for valid in ["a", "team-cache", "team_cache2"] {
      validate_alias(valid).expect("canonical alias");
    }
    for invalid in ["", "1team", "Team", "team.cache"] {
      assert!(validate_alias(invalid).is_err(), "alias should be rejected: {invalid}");
    }
  }

  #[test]
  fn empty_prefix_is_canonical_and_location_changes_authority() {
    let mut first = wire(S3Role::Read, &[]);
    first.prefix.clear();
    let first = S3Target::from_wire(first).expect("empty prefix");
    let mut second = wire(S3Role::Read, &[]);
    second.prefix = "other".to_string();
    let second = S3Target::from_wire(second).expect("other prefix");
    assert_ne!(first.authority(), second.authority());
  }

  #[test]
  fn target_map_is_external_exact_and_rejects_unknown_fields() {
    let root = tempfile::tempdir().expect("root");
    let source = root.path().join("source");
    fs::create_dir(&source).expect("source");
    let map = root.path().join("targets.json");
    fs::write(
      &map,
      r#"{"version":1,"targets":{"team":{"protocol":"s3","region":"us-east-1","expected_bucket_owner":"123456789012","bucket":"cargo-rail-cache-fixture","prefix":"cache","role":"read","shareable_environment":[]}}}"#,
    )
    .expect("map");
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;
      fs::set_permissions(&map, fs::Permissions::from_mode(0o600)).expect("permissions");
    }
    let target = S3Target::load_from_path(&source, "team", &map).expect("selected target");
    assert_eq!(target.shareable_environment_names(), &[] as &[String]);

    fs::write(
      &map,
      r#"{"version":1,"targets":{"team":{"protocol":"s3","region":"us-east-1","expected_bucket_owner":"123456789012","bucket":"cargo-rail-cache-fixture","prefix":"cache","role":"read","shareable_environment":[],"endpoint":"https://invalid.example"}}}"#,
    )
    .expect("unknown field map");
    assert!(S3Target::load_from_path(&source, "team", &map).is_err());
  }
}
