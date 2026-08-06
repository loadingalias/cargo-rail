//! Pinned S3 target authority and bounded object operations.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File};
use std::future::Future;
use std::io::{Read as _, Seek as _, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

use aws_config::BehaviorVersion;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::config::endpoint::ResolveEndpoint as _;
use aws_sdk_s3::config::retry::RetryConfig;
use aws_sdk_s3::config::timeout::TimeoutConfig;
use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::operation::get_object::GetObjectError;
use aws_sdk_s3::operation::put_object::PutObjectError;
use aws_sdk_s3::primitives::{ByteStream, Length};
use aws_types::service_config::ServiceConfigKey;
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use sha2::{Digest as _, Sha256};

use super::{
  MAX_ACTION_STATE_BYTES, MAX_APPROVED_ENVIRONMENT_NAMES, MAX_APPROVED_ENVIRONMENT_TOTAL_BYTES,
  MAX_SELECTOR_STATE_BYTES, RemoteActionState, RemoteSelectorResolution, RemoteSelectorState, RemoteStoreError,
  RemoteStoreFault, RemoteStoreResult, environment_name_may_be_secret, strictly_sorted_unique, valid_environment_name,
};
use crate::compiler::native_cache::{RemoteAuthorityId, pack::NativeAssociation};
use crate::source::ContentDigest;

pub(crate) const TARGETS_ENV: &str = "CARGO_RAIL_CACHE_TARGETS_FILE";
const TARGETS_VERSION: u32 = 1;
const TARGETS_MAX_BYTES: u64 = 64 * 1024;
const TARGETS_MAX_ENTRIES: usize = 64;
const MAX_ALIAS_BYTES: usize = 64;
const MAX_REGION_BYTES: usize = 64;
const MAX_BUCKET_BYTES: usize = 63;
const MAX_PREFIX_BYTES: usize = 512;
const MAX_ETAG_BYTES: usize = 128;
const MAX_CONDITIONAL_ATTEMPTS: usize = 16;
const STREAM_BUFFER_BYTES: usize = 64 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CREDENTIAL_TIMEOUT: Duration = Duration::from_secs(30);
const READ_TIMEOUT: Duration = Duration::from_secs(60);
const OPERATION_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const STREAM_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const AUTHORITY_DOMAIN: &[u8] = b"cargo-rail-s3-authority-v1\0";
const OBJECT_NAMESPACE: &str = "native-v3";
const PROTOCOL_MARKER: &[u8] = b"cargo-rail-native-cache-v3\n";

/// Maximum authority granted to one command for a pinned S3 target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum S3Role {
  /// Read existing remote objects only.
  Read,
  /// Read and conditionally publish remote objects.
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

/// Validated, owned authority for one classic S3 bucket namespace.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct S3Target {
  region: String,
  expected_bucket_owner: String,
  bucket: String,
  prefix: String,
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
  /// Load one selected alias from the explicit machine-owned target map.
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
    let endpoint_identity = official_endpoint_identity(&wire.region, &wire.bucket)?;
    let authority = authority_id(
      &endpoint_identity,
      &wire.region,
      &wire.expected_bucket_owner,
      &wire.bucket,
      &prefix,
    )?;
    Ok(Self {
      region: wire.region,
      expected_bucket_owner: wire.expected_bucket_owner,
      bucket: wire.bucket,
      prefix,
      role: wire.role,
      shareable_environment_names: wire.shareable_environment,
      authority,
    })
  }

  /// Return the canonical, location-redacted authority identity.
  pub(crate) fn authority(&self) -> &RemoteAuthorityId {
    &self.authority
  }

  /// Return whether this target permits conditional publication.
  pub(crate) fn can_write(&self) -> bool {
    self.role == S3Role::ReadWrite
  }

  /// Return the redaction-safe role name used by status output.
  pub(crate) const fn role_name(&self) -> &'static str {
    match self.role {
      S3Role::Read => "read",
      S3Role::ReadWrite => "read_write",
    }
  }

  /// Return the exact sorted environment-name sharing policy.
  pub(crate) fn shareable_environment_names(&self) -> &[String] {
    &self.shareable_environment_names
  }

  fn object_key(&self, class: ObjectClass, identity: &str) -> RemoteStoreResult<String> {
    match class {
      ObjectClass::Selectors => crate::compiler::native_cache::validate_base_action_key(identity),
      ObjectClass::Actions | ObjectClass::Results => crate::compiler::native_cache::validate_action_key(identity),
    }
    .map_err(|_| RemoteStoreError::integrity("remote cache object identity is invalid"))?;
    let shard = identity_shard(identity)?;
    let suffix = format!("{OBJECT_NAMESPACE}/{}/{shard}/{identity}", class.as_str());
    if self.prefix.is_empty() {
      Ok(suffix)
    } else {
      Ok(format!("{}/{suffix}", self.prefix))
    }
  }

  fn protocol_marker_key(&self) -> String {
    let suffix = format!("{OBJECT_NAMESPACE}/protocol");
    if self.prefix.is_empty() {
      suffix
    } else {
      format!("{}/{suffix}", self.prefix)
    }
  }
}

/// One immutable remote object class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObjectClass {
  /// Compiler-environment selectors keyed by the base action.
  Selectors,
  /// Action state keyed by the exact compiler action.
  Actions,
  /// Result packs keyed by the exact compiler action.
  Results,
}

impl ObjectClass {
  fn as_str(self) -> &'static str {
    match self {
      Self::Selectors => "selectors",
      Self::Actions => "actions",
      Self::Results => "results",
    }
  }

  fn max_bytes(self) -> u64 {
    match self {
      Self::Selectors => MAX_SELECTOR_STATE_BYTES,
      Self::Actions => MAX_ACTION_STATE_BYTES,
      Self::Results => crate::compiler::native_cache::pack::MAX_PACK_BYTES,
    }
  }
}

/// Exact outcome of one S3 object read.
pub(crate) enum GetOutcome {
  /// The key does not exist.
  Absent,
  /// The key exists and its bounded body was streamed to an anonymous file.
  Present(S3Object),
}

/// One bounded S3 object body and its opaque compare-and-swap revision.
pub(crate) struct S3Object {
  body: File,
  bytes: u64,
  etag: String,
}

/// One exact result response whose body has not yet been consumed.
pub(crate) struct S3Result {
  body: ByteStream,
  bytes: u64,
}

impl S3Result {
  /// Return the exact declared body length checked against the pack bound.
  pub(crate) const fn bytes(&self) -> u64 {
    self.bytes
  }
}

/// Strongly consistent action/result lookup outcome.
pub(crate) enum S3Lookup {
  /// Neither action state nor an orphan result can grant a hit.
  Miss,
  /// The action is unique but its immutable result has expired.
  Expired,
  /// The action permanently names two distinct results.
  Conflict(String, String),
  /// The action is unique and its result body is ready for one streaming read.
  Unique { result_key: String, result: S3Result },
}

/// Result-first/action-last publication outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum S3Publication {
  /// The selected result is the unique remote authority.
  Unique { result_created: bool, action_changed: bool },
  /// Two exact environment selectors became terminal evidence.
  SelectorConflict(Vec<String>, Vec<String>),
  /// Two exact result identities became terminal evidence.
  ResultConflict(String, String),
}

struct SelectorObject {
  state: RemoteSelectorState,
  etag: String,
}

struct ActionObject {
  state: RemoteActionState,
  etag: String,
}

#[derive(Debug)]
enum SelectorTransition {
  Converged(RemoteSelectorResolution),
  Write(RemoteSelectorState, RemoteSelectorResolution),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ActionResolution {
  Unique(String),
  Conflict(String, String),
}

#[derive(Debug)]
enum ActionTransition {
  Converged(ActionResolution),
  Write(RemoteActionState, ActionResolution),
}

impl S3Object {
  /// Consume this object into its rewound anonymous body, byte count, and ETag.
  pub(crate) fn into_parts(self) -> (File, u64, String) {
    (self.body, self.bytes, self.etag)
  }
}

/// Exact outcome of one conditional object write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PutOutcome {
  /// The conditional write committed and returned this opaque revision.
  Written(String),
  /// The condition did not hold; no write was authorized.
  PreconditionFailed,
}

/// Command-owned S3 client and its fixed executor.
pub(crate) struct S3Store {
  runtime: tokio::runtime::Runtime,
  client: aws_sdk_s3::Client,
  target: S3Target,
}

fn block_on_timeout<F>(
  runtime: &tokio::runtime::Runtime,
  timeout: Duration,
  future: F,
) -> Result<F::Output, tokio::time::error::Elapsed>
where
  F: Future,
{
  runtime.block_on(async move { tokio::time::timeout(timeout, future).await })
}

/// Connect one validated target using the standard AWS credential chain.
pub(crate) fn connect(target: S3Target) -> RemoteStoreResult<S3Store> {
  reject_credential_endpoint_environment()?;
  let runtime = tokio::runtime::Builder::new_multi_thread()
    .worker_threads(4)
    .thread_name("cargo-rail-s3")
    .enable_all()
    .build()
    .map_err(|_| RemoteStoreError::configuration("remote cache runtime could not be created"))?;
  reject_profile_endpoint_masking(&runtime)?;
  let shared = runtime.block_on(
    aws_config::defaults(BehaviorVersion::latest())
      .region(Region::new(target.region.clone()))
      .use_fips(false)
      .use_dual_stack(false)
      .load(),
  );
  reject_credential_endpoint_override(&shared)?;
  let credentials = shared.credentials_provider().ok_or_else(|| {
    RemoteStoreError::new(
      RemoteStoreFault::Authentication,
      "remote cache credentials are unavailable",
    )
  })?;
  match block_on_timeout(&runtime, CREDENTIAL_TIMEOUT, credentials.as_ref().provide_credentials()) {
    Ok(Ok(_)) => {}
    Ok(Err(_)) => {
      return Err(RemoteStoreError::new(
        RemoteStoreFault::Authentication,
        "remote cache credentials are unavailable",
      ));
    }
    Err(_) => {
      return Err(RemoteStoreError::unavailable(
        "remote cache credential resolution timed out",
      ));
    }
  }
  let timeout = TimeoutConfig::builder()
    .connect_timeout(CONNECT_TIMEOUT)
    .read_timeout(READ_TIMEOUT)
    .operation_attempt_timeout(OPERATION_TIMEOUT)
    .operation_timeout(OPERATION_TIMEOUT)
    .build();
  let mut builder = aws_sdk_s3::config::Builder::from(&shared);
  builder
    .set_region(Some(Region::new(target.region.clone())))
    .set_endpoint_url(None)
    .set_use_fips(Some(false))
    .set_use_dual_stack(Some(false))
    .set_force_path_style(Some(false))
    .set_accelerate(Some(false))
    .set_use_arn_region(Some(false))
    .set_disable_multi_region_access_points(Some(true));
  builder.set_disable_s3_express_session_auth(Some(true));
  builder
    .set_timeout_config(Some(timeout))
    .set_retry_config(Some(RetryConfig::standard().with_max_attempts(3)));
  let client = aws_sdk_s3::Client::from_conf(builder.build());
  match block_on_timeout(
    &runtime,
    READ_TIMEOUT,
    verify_protocol_marker_request(client.clone(), target.clone()),
  ) {
    Ok(result) => result?,
    Err(_) => {
      return Err(RemoteStoreError::unavailable(
        "remote cache protocol marker read timed out",
      ));
    }
  }
  Ok(S3Store {
    runtime,
    client,
    target,
  })
}

fn reject_credential_endpoint_environment() -> RemoteStoreResult<()> {
  for name in [
    "AWS_ENDPOINT_URL",
    "AWS_ENDPOINT_URL_STS",
    "AWS_ENDPOINT_URL_SSO",
    "AWS_ENDPOINT_URL_SSO_OIDC",
  ] {
    if std::env::var_os(name).is_some_and(|value| !value.is_empty()) {
      return Err(RemoteStoreError::configuration(
        "remote credentials require official AWS service endpoints",
      ));
    }
  }
  if std::env::var_os("AWS_IGNORE_CONFIGURED_ENDPOINT_URLS")
    .is_some_and(|value| value.to_str().is_none_or(|value| value.eq_ignore_ascii_case("true")))
  {
    return Err(RemoteStoreError::configuration(
      "remote credentials require inspectable AWS service endpoints",
    ));
  }
  Ok(())
}

#[allow(deprecated)]
fn reject_profile_endpoint_masking(runtime: &tokio::runtime::Runtime) -> RemoteStoreResult<()> {
  use aws_config::profile::profile_file::ProfileFiles;
  use aws_types::os_shim_internal::{Env, Fs};

  let profiles = runtime
    .block_on(aws_config::profile::load(
      &Fs::real(),
      &Env::real(),
      &ProfileFiles::default(),
      None,
    ))
    .map_err(|_| RemoteStoreError::configuration("remote credential profile could not be checked"))?;
  if profiles
    .get("ignore_configured_endpoint_urls")
    .is_some_and(|value| value.eq_ignore_ascii_case("true"))
  {
    return Err(RemoteStoreError::configuration(
      "remote credentials require inspectable AWS service endpoints",
    ));
  }
  Ok(())
}

fn reject_credential_endpoint_override(shared: &aws_config::SdkConfig) -> RemoteStoreResult<()> {
  if shared.endpoint_url().is_some() {
    return Err(RemoteStoreError::configuration(
      "remote credentials require official AWS service endpoints",
    ));
  }
  if let Some(configuration) = shared.service_config() {
    for service_id in ["STS", "SSO", "SSO OIDC"] {
      let key = ServiceConfigKey::builder()
        .service_id(service_id)
        .env("AWS_ENDPOINT_URL")
        .profile("endpoint_url")
        .build()
        .map_err(|_| RemoteStoreError::configuration("remote credential endpoint policy is invalid"))?;
      if configuration.load_config(key).is_some() {
        return Err(RemoteStoreError::configuration(
          "remote credentials require official AWS service endpoints",
        ));
      }
    }
  }
  Ok(())
}

impl S3Store {
  /// Fetch one exact key and stream its body through a fixed bound.
  pub(crate) fn get(&self, class: ObjectClass, identity: &str) -> RemoteStoreResult<GetOutcome> {
    if class == ObjectClass::Results {
      return Err(RemoteStoreError::integrity(
        "remote cache result reads must use the streaming path",
      ));
    }
    self.runtime.block_on(self.get_metadata(class, identity))
  }

  async fn get_metadata(&self, class: ObjectClass, identity: &str) -> RemoteStoreResult<GetOutcome> {
    get_metadata_request(self.client.clone(), self.target.clone(), class, identity.to_string()).await
  }

  /// Begin one exact immutable result read without staging its body locally.
  pub(crate) fn get_result(&self, identity: &str) -> RemoteStoreResult<Option<S3Result>> {
    self.runtime.block_on(self.begin_result(identity))
  }

  /// Read action metadata and begin its result request concurrently.
  pub(crate) fn get_action_and_result(
    &self,
    identity: &str,
  ) -> (RemoteStoreResult<GetOutcome>, RemoteStoreResult<Option<S3Result>>) {
    let action = self.runtime.spawn(get_metadata_request(
      self.client.clone(),
      self.target.clone(),
      ObjectClass::Actions,
      identity.to_string(),
    ));
    let result = self.runtime.spawn(begin_result_request(
      self.client.clone(),
      self.target.clone(),
      identity.to_string(),
    ));
    self.runtime.block_on(async {
      let action = action
        .await
        .map_err(|_| RemoteStoreError::unavailable("remote cache action request did not complete"))
        .and_then(|outcome| outcome);
      let result = result
        .await
        .map_err(|_| RemoteStoreError::unavailable("remote cache result request did not complete"))
        .and_then(|outcome| outcome);
      (action, result)
    })
  }

  async fn begin_result(&self, identity: &str) -> RemoteStoreResult<Option<S3Result>> {
    begin_result_request(self.client.clone(), self.target.clone(), identity.to_string()).await
  }

  /// Stream one begun result exactly once into its caller-owned verification path.
  pub(crate) fn copy_result<W: Write>(&self, mut result: S3Result, output: &mut W) -> RemoteStoreResult<u64> {
    let expected = result.bytes;
    let streamed = block_on_timeout(&self.runtime, STREAM_TIMEOUT, async {
      let mut bytes = 0_u64;
      while let Some(chunk) = result.body.next().await {
        let chunk = chunk.map_err(|_| RemoteStoreError::unavailable("remote cache result stream failed"))?;
        bytes = bytes
          .checked_add(chunk.len() as u64)
          .ok_or_else(|| RemoteStoreError::integrity("remote cache result length overflowed"))?;
        if bytes > expected {
          return Err(RemoteStoreError::integrity(
            "remote cache result exceeded its declared length",
          ));
        }
        output.write_all(&chunk).map_err(io_unavailable)?;
      }
      Ok::<u64, RemoteStoreError>(bytes)
    });
    let bytes = streamed.map_err(|_| RemoteStoreError::unavailable("remote cache result stream timed out"))??;
    if bytes != expected {
      return Err(RemoteStoreError::integrity(
        "remote cache result length changed while it was read",
      ));
    }
    Ok(bytes)
  }

  /// Resolve the exact compiler-environment selector for one base action.
  pub(crate) fn resolve_selector(&self, base_action_key: &str) -> RemoteStoreResult<RemoteSelectorResolution> {
    Ok(match self.selector_state(base_action_key)? {
      None => RemoteSelectorResolution::Miss,
      Some(object) => object.state.into_resolution()?,
    })
  }

  /// Monotonically publish one exact compiler-environment selector.
  pub(crate) fn publish_selector(
    &self,
    base_action_key: &str,
    names: &[String],
  ) -> RemoteStoreResult<RemoteSelectorResolution> {
    for _ in 0..MAX_CONDITIONAL_ATTEMPTS {
      let current = self.selector_state(base_action_key)?;
      match selector_transition(current.as_ref().map(|object| &object.state), names)? {
        SelectorTransition::Converged(resolution) => return Ok(resolution),
        SelectorTransition::Write(state, resolution) => {
          let bytes = encode_selector_state(&state)?;
          let outcome = match current {
            None => self.put_bytes_if_absent(ObjectClass::Selectors, base_action_key, &bytes)?,
            Some(object) => {
              self.compare_and_swap_bytes(ObjectClass::Selectors, base_action_key, &object.etag, &bytes)?
            }
          };
          if matches!(outcome, PutOutcome::Written(_)) {
            return Ok(resolution);
          }
        }
      }
    }
    Err(RemoteStoreError::unavailable(
      "remote cache selector publication remained contended",
    ))
  }

  /// Resolve action and result concurrently; the action remains authoritative.
  pub(crate) fn lookup(&self, action_key: &str) -> RemoteStoreResult<S3Lookup> {
    crate::compiler::native_cache::validate_action_key(action_key)
      .map_err(|error| RemoteStoreError::integrity(error.to_string()))?;
    let (action, result) = self.get_action_and_result(action_key);
    let action = match action? {
      GetOutcome::Absent => None,
      GetOutcome::Present(object) => Some(decode_action_object(object)?),
    };
    lookup_outcome(action.as_ref().map(|object| &object.state), result)
  }

  /// Publish one verified pack result-first and its canonical action state last.
  pub(crate) fn publish(
    &self,
    association: &NativeAssociation,
    base_action_key: &str,
    environment_names: &[String],
    mut pack: File,
  ) -> RemoteStoreResult<S3Publication> {
    if !self.target.can_write() {
      return Err(RemoteStoreError::configuration(
        "selected cache target does not permit publication",
      ));
    }
    crate::compiler::native_cache::validate_base_action_key(base_action_key)
      .map_err(|error| RemoteStoreError::integrity(error.to_string()))?;
    validate_environment_names(environment_names)?;
    if environment_names
      .iter()
      .any(|name| self.target.shareable_environment_names.binary_search(name).is_err())
    {
      return Err(RemoteStoreError::integrity(
        "remote publication environment is outside the selected target policy",
      ));
    }
    let metadata = pack.metadata().map_err(io_unavailable)?;
    if !metadata.is_file() || metadata.len() != association.pack_length() {
      return Err(RemoteStoreError::integrity(
        "remote publication pack does not match its verified association",
      ));
    }
    pack.rewind().map_err(io_unavailable)?;

    match self.publish_selector(base_action_key, environment_names)? {
      RemoteSelectorResolution::Unique(existing) if existing == environment_names => {}
      RemoteSelectorResolution::Conflict(first, second) => {
        return Ok(S3Publication::SelectorConflict(first, second));
      }
      RemoteSelectorResolution::Miss | RemoteSelectorResolution::Unique(_) => {
        return Err(RemoteStoreError::integrity(
          "remote selector publication did not bind the requested environment",
        ));
      }
    }

    let (body_result, result_created) = self.publish_result(association, pack)?;
    match self.resolve_selector(base_action_key)? {
      RemoteSelectorResolution::Unique(existing) if existing == environment_names => {}
      RemoteSelectorResolution::Conflict(first, second) => {
        return Ok(S3Publication::SelectorConflict(first, second));
      }
      RemoteSelectorResolution::Miss | RemoteSelectorResolution::Unique(_) => {
        return Err(RemoteStoreError::integrity(
          "remote selector changed before action publication",
        ));
      }
    }
    let (action, action_changed) =
      self.publish_action(association.action_key(), &body_result, association.result_key())?;
    Ok(match action {
      ActionResolution::Unique(result) if result == association.result_key() => S3Publication::Unique {
        result_created,
        action_changed,
      },
      ActionResolution::Conflict(first, second) => S3Publication::ResultConflict(first, second),
      ActionResolution::Unique(_) => {
        return Err(RemoteStoreError::integrity(
          "remote action publication selected the wrong result",
        ));
      }
    })
  }

  fn selector_state(&self, base_action_key: &str) -> RemoteStoreResult<Option<SelectorObject>> {
    crate::compiler::native_cache::validate_base_action_key(base_action_key)
      .map_err(|error| RemoteStoreError::integrity(error.to_string()))?;
    match self.get(ObjectClass::Selectors, base_action_key)? {
      GetOutcome::Absent => Ok(None),
      GetOutcome::Present(object) => decode_selector_object(object).map(Some),
    }
  }

  fn action_state(&self, action_key: &str) -> RemoteStoreResult<Option<ActionObject>> {
    crate::compiler::native_cache::validate_action_key(action_key)
      .map_err(|error| RemoteStoreError::integrity(error.to_string()))?;
    match self.get(ObjectClass::Actions, action_key)? {
      GetOutcome::Absent => Ok(None),
      GetOutcome::Present(object) => decode_action_object(object).map(Some),
    }
  }

  fn publish_result(&self, association: &NativeAssociation, pack: File) -> RemoteStoreResult<(String, bool)> {
    match self.put_result_if_absent(association.action_key(), pack, association.pack_length())? {
      PutOutcome::Written(_) => Ok((association.result_key().to_string(), true)),
      PutOutcome::PreconditionFailed => {
        let result = self
          .get_result(association.action_key())?
          .ok_or_else(|| RemoteStoreError::unavailable("remote result disappeared after conditional publication"))?;
        let bytes = result.bytes();
        let mut staged = tempfile::tempfile().map_err(io_unavailable)?;
        self.copy_result(result, &mut staged)?;
        staged.rewind().map_err(io_unavailable)?;
        let (_decoded, existing) =
          crate::compiler::native_cache::pack::decode_for_action(staged, association.action_key(), Some(bytes), None)
            .map_err(|error| RemoteStoreError::integrity(format!("remote result is malformed: {error}")))?;
        Ok((existing.result_key().to_string(), false))
      }
    }
  }

  fn publish_action(
    &self,
    action_key: &str,
    body_result: &str,
    local_result: &str,
  ) -> RemoteStoreResult<(ActionResolution, bool)> {
    for _ in 0..MAX_CONDITIONAL_ATTEMPTS {
      let current = self.action_state(action_key)?;
      match action_transition(current.as_ref().map(|object| &object.state), body_result, local_result)? {
        ActionTransition::Converged(resolution) => return Ok((resolution, false)),
        ActionTransition::Write(state, resolution) => {
          let bytes = state.encode()?;
          let outcome = match current {
            None => self.put_bytes_if_absent(ObjectClass::Actions, action_key, &bytes)?,
            Some(object) => self.compare_and_swap_bytes(ObjectClass::Actions, action_key, &object.etag, &bytes)?,
          };
          if matches!(outcome, PutOutcome::Written(_)) {
            return Ok((resolution, true));
          }
        }
      }
    }
    Err(RemoteStoreError::unavailable(
      "remote cache action publication remained contended",
    ))
  }

  /// Create one bounded metadata object only when its key is absent.
  pub(crate) fn put_bytes_if_absent(
    &self,
    class: ObjectClass,
    identity: &str,
    bytes: &[u8],
  ) -> RemoteStoreResult<PutOutcome> {
    self.require_write()?;
    if class == ObjectClass::Results || bytes.len() as u64 > class.max_bytes() {
      return Err(RemoteStoreError::integrity(
        "remote cache metadata body does not match its object class",
      ));
    }
    self.put(class, identity, ByteStream::from(bytes.to_vec()), PutCondition::Absent)
  }

  /// Replace one bounded metadata object only at an exact opaque revision.
  pub(crate) fn compare_and_swap_bytes(
    &self,
    class: ObjectClass,
    identity: &str,
    expected_etag: &str,
    bytes: &[u8],
  ) -> RemoteStoreResult<PutOutcome> {
    self.require_write()?;
    let expected_etag = parse_etag(Some(expected_etag))?;
    if class == ObjectClass::Results || bytes.len() as u64 > class.max_bytes() {
      return Err(RemoteStoreError::integrity(
        "remote cache metadata body does not match its object class",
      ));
    }
    self.put(
      class,
      identity,
      ByteStream::from(bytes.to_vec()),
      PutCondition::Match(expected_etag),
    )
  }

  /// Create one immutable result from an already-opened exact bounded file.
  pub(crate) fn put_result_if_absent(
    &self,
    identity: &str,
    mut body: File,
    bytes: u64,
  ) -> RemoteStoreResult<PutOutcome> {
    self.require_write()?;
    let metadata = body.metadata().map_err(io_unavailable)?;
    if !metadata.is_file() || metadata.len() != bytes || bytes > ObjectClass::Results.max_bytes() {
      return Err(RemoteStoreError::integrity(
        "remote cache result body is not one exact bounded file",
      ));
    }
    body.rewind().map_err(io_unavailable)?;
    let body = self.runtime.block_on(
      ByteStream::read_from()
        .file(tokio::fs::File::from_std(body))
        .length(Length::Exact(bytes))
        .buffer_size(STREAM_BUFFER_BYTES)
        .build(),
    );
    let body = body.map_err(|_| RemoteStoreError::unavailable("remote cache result stream could not be opened"))?;
    self.put(ObjectClass::Results, identity, body, PutCondition::Absent)
  }

  fn require_write(&self) -> RemoteStoreResult<()> {
    if self.target.can_write() {
      Ok(())
    } else {
      Err(RemoteStoreError::configuration(
        "selected cache target does not permit publication",
      ))
    }
  }

  fn put(
    &self,
    class: ObjectClass,
    identity: &str,
    body: ByteStream,
    condition: PutCondition,
  ) -> RemoteStoreResult<PutOutcome> {
    let key = self.target.object_key(class, identity)?;
    let request = self
      .client
      .put_object()
      .bucket(&self.target.bucket)
      .key(key)
      .expected_bucket_owner(&self.target.expected_bucket_owner)
      .body(body);
    let request = match condition {
      PutCondition::Absent => request.if_none_match("*"),
      PutCondition::Match(etag) => request.if_match(etag),
    };
    match self.runtime.block_on(request.send()) {
      Ok(output) => Ok(PutOutcome::Written(parse_etag(output.e_tag())?)),
      Err(error) => match classify_put_error(&error) {
        RequestFailure::Precondition => Ok(PutOutcome::PreconditionFailed),
        RequestFailure::Absent => Err(RemoteStoreError::integrity(
          "remote cache write returned an impossible absence",
        )),
        RequestFailure::Store(error) => Err(error),
      },
    }
  }
}

fn decode_selector_object(object: S3Object) -> RemoteStoreResult<SelectorObject> {
  let (bytes, etag) = read_object(object, MAX_SELECTOR_STATE_BYTES)?;
  let state = serde_json::from_slice::<RemoteSelectorState>(&bytes)
    .map_err(|error| RemoteStoreError::integrity(format!("remote selector state is malformed: {error}")))?;
  state.clone().into_resolution()?;
  if encode_selector_state(&state)? != bytes {
    return Err(RemoteStoreError::integrity(
      "remote selector state is not canonically encoded",
    ));
  }
  Ok(SelectorObject { state, etag })
}

fn decode_action_object(object: S3Object) -> RemoteStoreResult<ActionObject> {
  let (bytes, etag) = read_object(object, MAX_ACTION_STATE_BYTES)?;
  let state = RemoteActionState::decode(&bytes)?;
  Ok(ActionObject { state, etag })
}

fn read_object(object: S3Object, maximum: u64) -> RemoteStoreResult<(Vec<u8>, String)> {
  let (mut body, declared, etag) = object.into_parts();
  if declared > maximum {
    return Err(RemoteStoreError::integrity(
      "remote cache metadata exceeds its byte bound",
    ));
  }
  let capacity = usize::try_from(declared)
    .map_err(|_| RemoteStoreError::integrity("remote cache metadata length is out of range"))?;
  let mut bytes = Vec::with_capacity(capacity);
  std::io::Read::by_ref(&mut body)
    .take(maximum.saturating_add(1))
    .read_to_end(&mut bytes)
    .map_err(io_unavailable)?;
  if bytes.len() as u64 != declared {
    return Err(RemoteStoreError::integrity(
      "remote cache metadata length changed while it was read",
    ));
  }
  Ok((bytes, etag))
}

fn encode_selector_state(state: &RemoteSelectorState) -> RemoteStoreResult<Vec<u8>> {
  state.clone().into_resolution()?;
  let bytes = serde_json::to_vec(state).map_err(|error| RemoteStoreError::integrity(error.to_string()))?;
  if bytes.len() as u64 > MAX_SELECTOR_STATE_BYTES {
    return Err(RemoteStoreError::integrity(
      "remote selector state exceeds its byte bound",
    ));
  }
  Ok(bytes)
}

fn selector_transition(
  current: Option<&RemoteSelectorState>,
  names: &[String],
) -> RemoteStoreResult<SelectorTransition> {
  super::validate_selector_names(names)?;
  let proposed = RemoteSelectorState::unique(names.to_vec())?;
  let proposed_resolution = RemoteSelectorResolution::Unique(names.to_vec());
  let Some(current) = current else {
    return Ok(SelectorTransition::Write(proposed, proposed_resolution));
  };
  match current.clone().into_resolution()? {
    RemoteSelectorResolution::Unique(existing) if existing == names => Ok(SelectorTransition::Converged(
      RemoteSelectorResolution::Unique(existing),
    )),
    RemoteSelectorResolution::Unique(existing) => {
      let state = RemoteSelectorState::conflict(existing.clone(), names.to_vec())?;
      let resolution = state.clone().into_resolution()?;
      Ok(SelectorTransition::Write(state, resolution))
    }
    conflict @ RemoteSelectorResolution::Conflict(_, _) => Ok(SelectorTransition::Converged(conflict)),
    RemoteSelectorResolution::Miss => Err(RemoteStoreError::integrity("stored selector state resolved to absence")),
  }
}

fn action_transition(
  current: Option<&RemoteActionState>,
  body_result: &str,
  local_result: &str,
) -> RemoteStoreResult<ActionTransition> {
  super::validate_remote_result_key(body_result)?;
  super::validate_remote_result_key(local_result)?;
  if let Some(RemoteActionState::Conflict { first, second }) = current {
    return Ok(ActionTransition::Converged(ActionResolution::Conflict(
      first.clone(),
      second.clone(),
    )));
  }
  let mut results = [
    current.and_then(|state| match state {
      RemoteActionState::Unique { result } => Some(result.as_str()),
      RemoteActionState::Conflict { .. } => None,
    }),
    Some(body_result),
    Some(local_result),
  ]
  .into_iter()
  .flatten()
  .collect::<Vec<_>>();
  results.sort_unstable();
  results.dedup();
  match results.as_slice() {
    [result] => {
      let resolution = ActionResolution::Unique((*result).to_string());
      if current.is_some() {
        Ok(ActionTransition::Converged(resolution))
      } else {
        Ok(ActionTransition::Write(RemoteActionState::unique(result)?, resolution))
      }
    }
    [first, second] => {
      let resolution = ActionResolution::Conflict((*first).to_string(), (*second).to_string());
      Ok(ActionTransition::Write(
        RemoteActionState::conflict(first, second)?,
        resolution,
      ))
    }
    _ => Err(RemoteStoreError::integrity(
      "remote action publication observed more than two result identities",
    )),
  }
}

fn lookup_outcome(
  action: Option<&RemoteActionState>,
  result: RemoteStoreResult<Option<S3Result>>,
) -> RemoteStoreResult<S3Lookup> {
  match action {
    None => Ok(S3Lookup::Miss),
    Some(RemoteActionState::Conflict { first, second }) => Ok(S3Lookup::Conflict(first.clone(), second.clone())),
    Some(RemoteActionState::Unique { result: result_key }) => match result? {
      None => Ok(S3Lookup::Expired),
      Some(result) => Ok(S3Lookup::Unique {
        result_key: result_key.clone(),
        result,
      }),
    },
  }
}

async fn verify_protocol_marker_request(client: aws_sdk_s3::Client, target: S3Target) -> RemoteStoreResult<()> {
  let result = client
    .get_object()
    .bucket(&target.bucket)
    .key(target.protocol_marker_key())
    .expected_bucket_owner(&target.expected_bucket_owner)
    .send()
    .await;
  let mut output = match result {
    Ok(output) => output,
    Err(error) => {
      return match classify_get_error(&error, RequestKind::ProtocolMarkerGet) {
        RequestFailure::Absent | RequestFailure::Precondition => Err(RemoteStoreError::configuration(
          "remote cache protocol marker is unavailable",
        )),
        RequestFailure::Store(error) => Err(error),
      };
    }
  };
  let declared = output
    .content_length()
    .map(u64::try_from)
    .transpose()
    .map_err(|_| RemoteStoreError::integrity("remote cache protocol marker has an invalid length"))?;
  if declared != Some(PROTOCOL_MARKER.len() as u64) {
    return Err(RemoteStoreError::integrity(
      "remote cache protocol marker has an invalid length",
    ));
  }
  let mut body = Vec::with_capacity(PROTOCOL_MARKER.len());
  while let Some(chunk) = output.body.next().await {
    let chunk = chunk.map_err(|_| RemoteStoreError::unavailable("remote cache protocol marker stream failed"))?;
    if body.len().saturating_add(chunk.len()) > PROTOCOL_MARKER.len() {
      return Err(RemoteStoreError::integrity(
        "remote cache protocol marker has an invalid length",
      ));
    }
    body.extend_from_slice(&chunk);
  }
  if body != PROTOCOL_MARKER {
    return Err(RemoteStoreError::integrity(
      "remote cache protocol marker has invalid contents",
    ));
  }
  Ok(())
}

async fn get_metadata_request(
  client: aws_sdk_s3::Client,
  target: S3Target,
  class: ObjectClass,
  identity: String,
) -> RemoteStoreResult<GetOutcome> {
  if class == ObjectClass::Results {
    return Err(RemoteStoreError::integrity(
      "remote cache result reads must use the streaming path",
    ));
  }
  let key = target.object_key(class, &identity)?;
  let result = client
    .get_object()
    .bucket(&target.bucket)
    .key(key)
    .expected_bucket_owner(&target.expected_bucket_owner)
    .send()
    .await;
  let mut output = match result {
    Ok(output) => output,
    Err(error) => {
      return match classify_get_error(&error, RequestKind::CacheGet) {
        RequestFailure::Absent => Ok(GetOutcome::Absent),
        RequestFailure::Precondition => Err(RemoteStoreError::integrity(
          "remote cache read returned an impossible precondition failure",
        )),
        RequestFailure::Store(error) => Err(error),
      };
    }
  };
  let etag = parse_etag(output.e_tag())?;
  let declared = output
    .content_length()
    .map(u64::try_from)
    .transpose()
    .map_err(|_| RemoteStoreError::integrity("remote cache object has an invalid length"))?;
  if declared.is_some_and(|bytes| bytes > class.max_bytes()) {
    return Err(RemoteStoreError::integrity(
      "remote cache object exceeds its byte bound",
    ));
  }
  let mut body = tempfile::tempfile().map_err(io_unavailable)?;
  let streamed = tokio::time::timeout(STREAM_TIMEOUT, async {
    let mut bytes = 0_u64;
    while let Some(chunk) = output.body.next().await {
      let chunk = chunk.map_err(|_| RemoteStoreError::unavailable("remote cache object stream failed"))?;
      bytes = bytes
        .checked_add(chunk.len() as u64)
        .ok_or_else(|| RemoteStoreError::integrity("remote cache object length overflowed"))?;
      if bytes > class.max_bytes() {
        return Err(RemoteStoreError::integrity(
          "remote cache object exceeds its byte bound",
        ));
      }
      body.write_all(&chunk).map_err(io_unavailable)?;
    }
    Ok::<u64, RemoteStoreError>(bytes)
  })
  .await;
  let bytes = streamed.map_err(|_| RemoteStoreError::unavailable("remote cache object stream timed out"))??;
  if declared.is_some_and(|declared| declared != bytes) {
    return Err(RemoteStoreError::integrity(
      "remote cache object length changed while it was read",
    ));
  }
  body.flush().map_err(io_unavailable)?;
  body.rewind().map_err(io_unavailable)?;
  Ok(GetOutcome::Present(S3Object { body, bytes, etag }))
}

async fn begin_result_request(
  client: aws_sdk_s3::Client,
  target: S3Target,
  identity: String,
) -> RemoteStoreResult<Option<S3Result>> {
  let key = target.object_key(ObjectClass::Results, &identity)?;
  let result = client
    .get_object()
    .bucket(&target.bucket)
    .key(key)
    .expected_bucket_owner(&target.expected_bucket_owner)
    .send()
    .await;
  let output = match result {
    Ok(output) => output,
    Err(error) => {
      return match classify_get_error(&error, RequestKind::CacheGet) {
        RequestFailure::Absent => Ok(None),
        RequestFailure::Precondition => Err(RemoteStoreError::integrity(
          "remote cache read returned an impossible precondition failure",
        )),
        RequestFailure::Store(error) => Err(error),
      };
    }
  };
  let _etag = parse_etag(output.e_tag())?;
  let bytes = output
    .content_length()
    .ok_or_else(|| RemoteStoreError::integrity("remote cache result has no exact length"))
    .and_then(|bytes| {
      u64::try_from(bytes).map_err(|_| RemoteStoreError::integrity("remote cache result has an invalid length"))
    })?;
  if bytes > ObjectClass::Results.max_bytes() {
    return Err(RemoteStoreError::integrity(
      "remote cache result exceeds its byte bound",
    ));
  }
  Ok(Some(S3Result {
    body: output.body,
    bytes,
  }))
}

enum PutCondition {
  Absent,
  Match(String),
}

enum RequestFailure {
  Absent,
  Precondition,
  Store(RemoteStoreError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestKind {
  ProtocolMarkerGet,
  CacheGet,
  ConditionalPut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusDisposition {
  Absent,
  Precondition,
  Authentication,
  Unavailable,
  Configuration,
}

fn classify_http_status(kind: RequestKind, status: u16) -> StatusDisposition {
  match status {
    403 | 404 if kind == RequestKind::CacheGet => StatusDisposition::Absent,
    401 | 403 => StatusDisposition::Authentication,
    409 | 412 if kind == RequestKind::ConditionalPut => StatusDisposition::Precondition,
    408 | 425 | 429 | 500..=599 => StatusDisposition::Unavailable,
    _ => StatusDisposition::Configuration,
  }
}

fn classify_get_error(error: &SdkError<GetObjectError>, kind: RequestKind) -> RequestFailure {
  let typed_absence = matches!(error.as_service_error(), Some(GetObjectError::NoSuchKey(_)));
  classify_sdk_error(error, kind, typed_absence && kind == RequestKind::CacheGet)
}

fn classify_put_error(error: &SdkError<PutObjectError>) -> RequestFailure {
  classify_sdk_error(error, RequestKind::ConditionalPut, false)
}

fn classify_sdk_error<E>(error: &SdkError<E>, kind: RequestKind, typed_absence: bool) -> RequestFailure {
  if typed_absence {
    return RequestFailure::Absent;
  }
  if let SdkError::ServiceError(context) = error {
    return match classify_http_status(kind, context.raw().status().as_u16()) {
      StatusDisposition::Absent => RequestFailure::Absent,
      StatusDisposition::Precondition => RequestFailure::Precondition,
      StatusDisposition::Authentication => RequestFailure::Store(RemoteStoreError::new(
        RemoteStoreFault::Authentication,
        "remote cache authentication was rejected",
      )),
      StatusDisposition::Unavailable => {
        RequestFailure::Store(RemoteStoreError::unavailable("remote cache service is unavailable"))
      }
      StatusDisposition::Configuration => RequestFailure::Store(RemoteStoreError::configuration(
        "remote cache request was rejected by its pinned service",
      )),
    };
  }
  match error {
    SdkError::ConstructionFailure(_) => RequestFailure::Store(RemoteStoreError::new(
      RemoteStoreFault::Authentication,
      "remote cache credentials are unavailable",
    )),
    SdkError::ResponseError(_) => RequestFailure::Store(RemoteStoreError::integrity(
      "remote cache service returned an invalid response",
    )),
    SdkError::TimeoutError(_) | SdkError::DispatchFailure(_) => {
      RequestFailure::Store(RemoteStoreError::unavailable("remote cache service is unavailable"))
    }
    _ => RequestFailure::Store(RemoteStoreError::unavailable("remote cache request failed")),
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

fn validate_environment_names(names: &[String]) -> RemoteStoreResult<()> {
  if names.len() > MAX_APPROVED_ENVIRONMENT_NAMES
    || !strictly_sorted_unique(names)
    || names
      .iter()
      .any(|name| !valid_environment_name(name) || environment_name_may_be_secret(name))
    || names
      .iter()
      .try_fold(0_usize, |total, name| total.checked_add(name.len()))
      .is_none_or(|bytes| bytes > MAX_APPROVED_ENVIRONMENT_TOTAL_BYTES)
  {
    Err(RemoteStoreError::configuration(
      "cache target environment sharing policy is invalid",
    ))
  } else {
    Ok(())
  }
}

fn official_endpoint_identity(region: &str, bucket: &str) -> RemoteStoreResult<String> {
  let params = aws_sdk_s3::config::endpoint::Params::builder()
    .region(region)
    .bucket(bucket)
    .use_fips(false)
    .use_dual_stack(false)
    .force_path_style(false)
    .accelerate(false)
    .use_global_endpoint(false)
    .disable_access_points(true)
    .disable_multi_region_access_points(true)
    .use_arn_region(false)
    .disable_s3_express_session_auth(true)
    .build()
    .map_err(|_| RemoteStoreError::configuration("cache target endpoint parameters are invalid"))?;
  let resolver = aws_sdk_s3::config::endpoint::DefaultResolver::new();
  let mut future = Box::pin(resolver.resolve_endpoint(&params));
  let waker = Waker::from(Arc::new(NoopWake));
  let mut context = Context::from_waker(&waker);
  match Pin::as_mut(&mut future).poll(&mut context) {
    Poll::Ready(Ok(endpoint)) if endpoint.url().starts_with("https://") => Ok(endpoint.url().to_string()),
    Poll::Ready(_) => Err(RemoteStoreError::configuration(
      "cache target does not resolve to an official HTTPS endpoint",
    )),
    Poll::Pending => Err(RemoteStoreError::configuration(
      "cache target endpoint resolution did not complete",
    )),
  }
}

struct NoopWake;

impl Wake for NoopWake {
  fn wake(self: Arc<Self>) {}
}

fn authority_id(
  endpoint: &str,
  region: &str,
  owner: &str,
  bucket: &str,
  prefix: &str,
) -> RemoteStoreResult<RemoteAuthorityId> {
  let mut hasher = Sha256::new();
  hasher.update(AUTHORITY_DOMAIN);
  for (tag, value) in [
    (b"protocol".as_slice(), b"s3".as_slice()),
    (b"protocol-version".as_slice(), OBJECT_NAMESPACE.as_bytes()),
    (b"endpoint".as_slice(), endpoint.as_bytes()),
    (b"region".as_slice(), region.as_bytes()),
    (b"expected-owner".as_slice(), owner.as_bytes()),
    (b"bucket".as_slice(), bucket.as_bytes()),
    (b"prefix".as_slice(), prefix.as_bytes()),
  ] {
    let tag_length = u32::try_from(tag.len())
      .map_err(|_| RemoteStoreError::configuration("cache target authority field is too large"))?;
    let value_length = u64::try_from(value.len())
      .map_err(|_| RemoteStoreError::configuration("cache target authority field is too large"))?;
    hasher.update(tag_length.to_le_bytes());
    hasher.update(tag);
    hasher.update(value_length.to_le_bytes());
    hasher.update(value);
  }
  RemoteAuthorityId::parse(format!(
    "remote-authority-v1-sha256-{}",
    ContentDigest::from_sha256_bytes(hasher.finalize().into())
  ))
  .map_err(|_| RemoteStoreError::configuration("cache target authority could not be derived"))
}

fn identity_shard(identity: &str) -> RemoteStoreResult<&str> {
  let digest = identity
    .rsplit_once("-sha256-")
    .map(|(_, digest)| digest)
    .filter(|digest| digest.len() == 64)
    .filter(|digest| {
      digest
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
    .ok_or_else(|| RemoteStoreError::integrity("remote cache object identity is invalid"))?;
  Ok(&digest[..2])
}

fn parse_etag(value: Option<&str>) -> RemoteStoreResult<String> {
  let value = value.ok_or_else(|| RemoteStoreError::integrity("remote cache object has no ETag"))?;
  let body = value.strip_prefix('"').and_then(|value| value.strip_suffix('"'));
  if value.len() > MAX_ETAG_BYTES
    || body
      .is_none_or(|body| body.is_empty() || !body.bytes().all(|byte| byte == b'!' || (b'#'..=b'~').contains(&byte)))
  {
    Err(RemoteStoreError::integrity("remote cache object has an invalid ETag"))
  } else {
    Ok(value.to_string())
  }
}

fn io_configuration(_error: std::io::Error) -> RemoteStoreError {
  RemoteStoreError::configuration("cache target map could not be read safely")
}

fn io_unavailable(_error: std::io::Error) -> RemoteStoreError {
  RemoteStoreError::unavailable("remote cache local staging I/O failed")
}

#[cfg(test)]
mod tests {
  use std::fs;

  use aws_types::service_config::{LoadServiceConfig, ServiceConfigKey};

  use super::*;

  #[derive(Debug)]
  struct CredentialEndpointOverride(&'static str);

  impl LoadServiceConfig for CredentialEndpointOverride {
    fn load_config(&self, key: ServiceConfigKey<'_>) -> Option<String> {
      (key.service_id() == self.0 && key.env() == "AWS_ENDPOINT_URL" && key.profile() == "endpoint_url")
        .then(|| "https://credential-proxy.invalid".to_string())
    }
  }

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

  fn result(byte: char) -> String {
    format!(
      "{}{}",
      crate::compiler::native_cache::RESULT_KEY_PREFIX,
      byte.to_string().repeat(64)
    )
  }

  #[test]
  fn synchronous_timeout_enters_the_runtime_before_creating_its_timer() {
    let runtime = tokio::runtime::Builder::new_current_thread()
      .enable_time()
      .build()
      .expect("runtime");

    assert_eq!(
      block_on_timeout(&runtime, Duration::from_secs(1), async { 7 }).expect("completed future"),
      7
    );
  }

  #[test]
  fn target_authority_excludes_alias_role_and_environment_policy() {
    let read = S3Target::from_wire(wire(S3Role::Read, &[])).expect("read target");
    let write = S3Target::from_wire(wire(S3Role::ReadWrite, &["RUSTFLAGS"])).expect("write target");
    assert_eq!(read.authority(), write.authority());
    assert!(!read.can_write());
    assert!(write.can_write());
  }

  #[test]
  fn credential_service_endpoint_overrides_fail_closed_before_resolution() {
    let clean = aws_config::SdkConfig::builder().build();
    reject_credential_endpoint_override(&clean).expect("official endpoint defaults");

    let global = aws_config::SdkConfig::builder()
      .endpoint_url("https://credential-proxy.invalid")
      .build();
    assert_eq!(
      reject_credential_endpoint_override(&global)
        .expect_err("global endpoint override")
        .fault,
      RemoteStoreFault::Configuration
    );

    for service_id in ["STS", "SSO", "SSO OIDC"] {
      let service = aws_config::SdkConfig::builder()
        .service_config(CredentialEndpointOverride(service_id))
        .build();
      assert_eq!(
        reject_credential_endpoint_override(&service)
          .expect_err("credential endpoint override")
          .fault,
        RemoteStoreFault::Configuration
      );
    }
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

    let sensitive = wire(S3Role::Read, &["CI_TOKEN"]);
    assert!(S3Target::from_wire(sensitive).is_err());
  }

  #[test]
  fn key_uses_digest_shard_and_normalized_prefix() {
    let target = S3Target::from_wire(wire(S3Role::Read, &[])).expect("target");
    let identity = format!("compiler-action-v8-sha256-{}", "ab".repeat(32));
    assert_eq!(
      target.object_key(ObjectClass::Results, &identity).expect("key"),
      format!("teams/example/native-v3/results/ab/{identity}")
    );
    assert_eq!(target.protocol_marker_key(), "teams/example/native-v3/protocol");
  }

  #[test]
  fn alias_and_etag_validation_preserve_canonical_opaque_evidence() {
    for valid in ["a", "team-cache", "team_cache2"] {
      validate_alias(valid).expect("canonical alias");
    }
    for invalid in ["", "1team", "Team", "team.cache"] {
      assert!(validate_alias(invalid).is_err(), "alias should be rejected: {invalid}");
    }
    assert_eq!(
      parse_etag(Some("\"opaque+revision/7\"")).expect("opaque etag"),
      "\"opaque+revision/7\""
    );
    for invalid in [None, Some("opaque"), Some("\"bad\"quote\""), Some("\"line\nfeed\"")] {
      assert!(parse_etag(invalid).is_err());
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
    fs::write(
      &map,
      r#"{"version":1,"targets":{"team":{"protocol":"s3","region":"us-east-1","expected_bucket_owner":"123456789012","bucket":"cargo-rail-cache-fixture","prefix":"cache","role":"read","shareable_environment":[]}}}"#,
    )
    .expect("restored map");

    let inside = source.join("targets.json");
    fs::copy(&map, &inside).expect("inside map");
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt as _;

      fs::set_permissions(&inside, fs::Permissions::from_mode(0o600)).expect("permissions");
    }
    assert!(S3Target::load_from_path(&source, "team", &inside).is_err());
  }

  #[test]
  fn conditional_statuses_are_not_collapsed_into_outages() {
    assert_eq!(
      classify_http_status(RequestKind::CacheGet, 404),
      StatusDisposition::Absent
    );
    assert_eq!(
      classify_http_status(RequestKind::CacheGet, 403),
      StatusDisposition::Absent
    );
    assert_eq!(
      classify_http_status(RequestKind::ProtocolMarkerGet, 403),
      StatusDisposition::Authentication
    );
    assert_eq!(
      classify_http_status(RequestKind::ProtocolMarkerGet, 404),
      StatusDisposition::Configuration
    );
    assert_eq!(
      classify_http_status(RequestKind::ConditionalPut, 409),
      StatusDisposition::Precondition
    );
    assert_eq!(
      classify_http_status(RequestKind::ConditionalPut, 412),
      StatusDisposition::Precondition
    );
    assert_eq!(
      classify_http_status(RequestKind::CacheGet, 503),
      StatusDisposition::Unavailable
    );
    assert_eq!(
      classify_http_status(RequestKind::ConditionalPut, 404),
      StatusDisposition::Configuration
    );
  }

  #[test]
  fn selector_transition_converges_or_records_one_sorted_terminal_conflict() {
    let first = vec!["RUSTFLAGS".to_string()];
    let second = vec!["CARGO_CFG_TARGET_FEATURE".to_string()];
    let SelectorTransition::Write(unique, RemoteSelectorResolution::Unique(names)) =
      selector_transition(None, &first).expect("initial selector")
    else {
      panic!("initial selector must be written as unique");
    };
    assert_eq!(names, first);
    assert!(matches!(
      selector_transition(Some(&unique), &first).expect("equal selector"),
      SelectorTransition::Converged(RemoteSelectorResolution::Unique(_))
    ));

    let SelectorTransition::Write(conflict, RemoteSelectorResolution::Conflict(low, high)) =
      selector_transition(Some(&unique), &second).expect("divergent selector")
    else {
      panic!("divergent selector must become conflict");
    };
    assert_eq!(low, second);
    assert_eq!(high, first);
    assert!(matches!(
      selector_transition(Some(&conflict), &first).expect("terminal selector"),
      SelectorTransition::Converged(RemoteSelectorResolution::Conflict(_, _))
    ));
  }

  #[test]
  fn lookup_ignores_orphans_and_gives_action_conflicts_precedence() {
    let orphan = lookup_outcome(None, Err(RemoteStoreError::unavailable("orphan request failed")))
      .expect("orphan must be ignored");
    assert!(matches!(orphan, S3Lookup::Miss));

    let low = result('1');
    let high = result('2');
    let conflict = RemoteActionState::conflict(&high, &low).expect("conflict");
    let lookup = lookup_outcome(
      Some(&conflict),
      Err(RemoteStoreError::unavailable("result request failed")),
    )
    .expect("action conflict must win");
    let S3Lookup::Conflict(first, second) = lookup else {
      panic!("conflict expected");
    };
    assert_eq!((first, second), (low, high));

    let unique = RemoteActionState::unique(&result('3')).expect("unique");
    assert!(matches!(
      lookup_outcome(Some(&unique), Ok(None)).expect("expired result"),
      S3Lookup::Expired
    ));
  }

  #[test]
  fn expired_unique_result_replacement_becomes_terminal_conflict() {
    let old = result('1');
    let new = result('2');
    let current = RemoteActionState::unique(&old).expect("old unique");
    let ActionTransition::Write(state, ActionResolution::Conflict(first, second)) =
      action_transition(Some(&current), &new, &new).expect("expired replacement")
    else {
      panic!("replacement must publish conflict");
    };
    assert_eq!((first.as_str(), second.as_str()), (old.as_str(), new.as_str()));
    assert_eq!(
      state,
      RemoteActionState::conflict(&old, &new).expect("canonical conflict")
    );
  }

  #[test]
  fn action_conflict_is_terminal_and_three_results_are_rejected() {
    let first = result('1');
    let second = result('2');
    let third = result('3');
    let conflict = RemoteActionState::conflict(&first, &second).expect("conflict");
    let ActionTransition::Converged(ActionResolution::Conflict(low, high)) =
      action_transition(Some(&conflict), &third, &third).expect("terminal conflict")
    else {
      panic!("existing conflict must remain terminal");
    };
    assert_eq!((low, high), (first.clone(), second.clone()));

    let unique = RemoteActionState::unique(&first).expect("unique");
    let error = action_transition(Some(&unique), &second, &third).expect_err("three result identities");
    assert_eq!(error.fault, RemoteStoreFault::Integrity);
  }
}
