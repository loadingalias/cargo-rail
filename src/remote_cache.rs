//! Command-owned loopback coordinator for the native S3 cache protocol.

mod s3;

use std::fmt;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::compiler::native_cache::RemoteAuthorityId;

const STATE_VERSION: u32 = 1;
const MAX_APPROVED_ENVIRONMENT_NAMES: usize = 512;
const MAX_APPROVED_ENVIRONMENT_NAME_BYTES: usize = 256;
const MAX_APPROVED_ENVIRONMENT_TOTAL_BYTES: usize = 32 * 1024;
const MAX_SELECTOR_STATE_BYTES: u64 = (2 * MAX_APPROVED_ENVIRONMENT_TOTAL_BYTES + 16 * 1024) as u64;
const MAX_ACTION_STATE_BYTES: u64 = 512;
const IPC_MAGIC: &[u8; 8] = b"CRNIPC4\0";
const IPC_RESPONSE_MAGIC: &[u8; 8] = b"CRNRES2\0";
const IPC_MAX_TOKEN_BYTES: usize = 128;
const IPC_MAX_IDENTITY_BYTES: usize = 128;
const IPC_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const IPC_TRANSFER_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const COORDINATOR_WORKERS: usize = 4;

const IPC_RESOLVE_SELECTOR: u8 = 1;
const IPC_FETCH: u8 = 2;
const IPC_PUBLISH: u8 = 3;
const RESPONSE_MISS: u8 = 0;
const RESPONSE_UNIQUE: u8 = 1;
const RESPONSE_CONFLICT: u8 = 2;
const RESPONSE_EXPIRED: u8 = 3;
const RESPONSE_PACK: u8 = 4;
const RESPONSE_PUBLISHED: u8 = 5;
const RESPONSE_SKIPPED: u8 = 6;
const RESPONSE_INTEGRITY: u8 = 20;
const RESPONSE_UNAVAILABLE: u8 = 21;

pub(crate) const TARGETS_ENV: &str = s3::TARGETS_ENV;
static REMOTE_UNAVAILABLE_WARNED: AtomicBool = AtomicBool::new(false);
const PRIVATE_REMOTE_ENVIRONMENT: &[&str] = &[
  TARGETS_ENV,
  "AWS_ACCESS_KEY_ID",
  "AWS_SECRET_ACCESS_KEY",
  "AWS_SESSION_TOKEN",
  "AWS_SECURITY_TOKEN",
  "AWS_WEB_IDENTITY_TOKEN_FILE",
  "AWS_ROLE_ARN",
  "AWS_ROLE_SESSION_NAME",
  "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
  "AWS_CONTAINER_CREDENTIALS_FULL_URI",
  "AWS_CONTAINER_AUTHORIZATION_TOKEN",
  "AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE",
  "AWS_PROFILE",
  "AWS_DEFAULT_PROFILE",
  "AWS_CONFIG_FILE",
  "AWS_SHARED_CREDENTIALS_FILE",
  "AWS_ENDPOINT_URL",
  "AWS_ENDPOINT_URL_S3",
  "AWS_ENDPOINT_URL_STS",
  "AWS_ENDPOINT_URL_SSO",
  "AWS_ENDPOINT_URL_SSO_OIDC",
  "AWS_IGNORE_CONFIGURED_ENDPOINT_URLS",
];

pub(crate) fn scrub_child_environment(command: &mut std::process::Command) {
  for name in PRIVATE_REMOTE_ENVIRONMENT {
    command.env_remove(name);
  }
}

pub(crate) fn warn_unavailable_once() {
  if !REMOTE_UNAVAILABLE_WARNED.swap(true, Ordering::AcqRel) {
    crate::warn!("shared compiler cache unavailable; continuing with local cache");
  }
}

/// Stable remote failure category used at the wrapper boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteStoreFault {
  Integrity,
  Authentication,
  Unavailable,
  Configuration,
}

/// One redacted remote-cache failure.
#[derive(Debug, Clone)]
pub(crate) struct RemoteStoreError {
  pub(crate) fault: RemoteStoreFault,
  message: String,
}

impl RemoteStoreError {
  fn integrity(message: impl Into<String>) -> Self {
    Self::new(RemoteStoreFault::Integrity, message)
  }

  fn unavailable(message: impl Into<String>) -> Self {
    Self::new(RemoteStoreFault::Unavailable, message)
  }

  fn configuration(message: impl Into<String>) -> Self {
    Self::new(RemoteStoreFault::Configuration, message)
  }

  fn new(fault: RemoteStoreFault, message: impl Into<String>) -> Self {
    Self {
      fault,
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

type RemoteStoreResult<T> = Result<T, RemoteStoreError>;

/// One result identity retained as terminal conflict evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteResultIdentity(String);

impl RemoteResultIdentity {
  pub(crate) fn result_key(&self) -> &str {
    &self.0
  }
}

/// Monotonic compiler-environment selector authority for one base action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteSelectorResolution {
  Miss,
  Unique(Vec<String>),
  Conflict(Vec<String>, Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteSelectorState {
  version: u32,
  resolution: StoredSelectorResolution,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum StoredSelectorResolution {
  Unique { names: Vec<String> },
  Conflict { first: Vec<String>, second: Vec<String> },
}

impl RemoteSelectorState {
  fn unique(names: Vec<String>) -> RemoteStoreResult<Self> {
    validate_selector_names(&names)?;
    Ok(Self {
      version: STATE_VERSION,
      resolution: StoredSelectorResolution::Unique { names },
    })
  }

  fn conflict(first: Vec<String>, second: Vec<String>) -> RemoteStoreResult<Self> {
    let (first, second) = canonical_selector_pair(first, second)?;
    Ok(Self {
      version: STATE_VERSION,
      resolution: StoredSelectorResolution::Conflict { first, second },
    })
  }

  fn into_resolution(self) -> RemoteStoreResult<RemoteSelectorResolution> {
    if self.version != STATE_VERSION {
      return Err(RemoteStoreError::integrity(
        "remote selector state has an incompatible version",
      ));
    }
    match self.resolution {
      StoredSelectorResolution::Unique { names } => {
        validate_selector_names(&names)?;
        Ok(RemoteSelectorResolution::Unique(names))
      }
      StoredSelectorResolution::Conflict { first, second } => {
        validate_selector_names(&first)?;
        validate_selector_names(&second)?;
        if first >= second {
          return Err(RemoteStoreError::integrity("remote selector conflict is not canonical"));
        }
        Ok(RemoteSelectorResolution::Conflict(first, second))
      }
    }
  }
}

/// Canonical permanent state for one exact compiler action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum RemoteActionState {
  Unique { result: String },
  Conflict { first: String, second: String },
}

impl RemoteActionState {
  fn unique(result: &str) -> RemoteStoreResult<Self> {
    validate_remote_result_key(result)?;
    Ok(Self::Unique {
      result: result.to_string(),
    })
  }

  fn conflict(first: &str, second: &str) -> RemoteStoreResult<Self> {
    let (first, second) = canonical_result_pair(first, second)?;
    Ok(Self::Conflict { first, second })
  }

  fn encode(&self) -> RemoteStoreResult<Vec<u8>> {
    self.validate()?;
    let bytes = serde_json::to_vec(self).map_err(|error| RemoteStoreError::integrity(error.to_string()))?;
    if bytes.len() as u64 > MAX_ACTION_STATE_BYTES {
      return Err(RemoteStoreError::integrity(
        "remote compiler action state exceeds its byte bound",
      ));
    }
    Ok(bytes)
  }

  fn decode(bytes: &[u8]) -> RemoteStoreResult<Self> {
    if bytes.len() as u64 > MAX_ACTION_STATE_BYTES {
      return Err(RemoteStoreError::integrity(
        "remote compiler action state exceeds its byte bound",
      ));
    }
    let state = serde_json::from_slice::<Self>(bytes)
      .map_err(|error| RemoteStoreError::integrity(format!("remote compiler action state is malformed: {error}")))?;
    state.validate()?;
    if state.encode()? != bytes {
      return Err(RemoteStoreError::integrity(
        "remote compiler action state is not canonically encoded",
      ));
    }
    Ok(state)
  }

  fn validate(&self) -> RemoteStoreResult<()> {
    match self {
      Self::Unique { result } => validate_remote_result_key(result),
      Self::Conflict { first, second } => {
        validate_remote_result_key(first)?;
        validate_remote_result_key(second)?;
        if first >= second {
          return Err(RemoteStoreError::integrity(
            "remote compiler action conflict is not canonical",
          ));
        }
        Ok(())
      }
    }
  }
}

fn validate_remote_result_key(result: &str) -> RemoteStoreResult<()> {
  crate::compiler::native_cache::validate_result_key(result)
    .map_err(|error| RemoteStoreError::integrity(error.to_string()))
}

fn canonical_result_pair(first: &str, second: &str) -> RemoteStoreResult<(String, String)> {
  validate_remote_result_key(first)?;
  validate_remote_result_key(second)?;
  match first.cmp(second) {
    std::cmp::Ordering::Less => Ok((first.to_string(), second.to_string())),
    std::cmp::Ordering::Greater => Ok((second.to_string(), first.to_string())),
    std::cmp::Ordering::Equal => Err(RemoteStoreError::integrity(
      "remote compiler action conflict repeats one result",
    )),
  }
}

fn validate_selector_names(names: &[String]) -> RemoteStoreResult<()> {
  if names.len() > MAX_APPROVED_ENVIRONMENT_NAMES
    || !strictly_sorted_unique(names)
    || names.iter().any(|name| !valid_environment_name(name))
    || names
      .iter()
      .try_fold(0_usize, |total, name| total.checked_add(name.len()))
      .is_none_or(|bytes| bytes > MAX_APPROVED_ENVIRONMENT_TOTAL_BYTES)
  {
    return Err(RemoteStoreError::integrity(
      "remote compiler environment selector is invalid",
    ));
  }
  Ok(())
}

fn canonical_selector_pair(first: Vec<String>, second: Vec<String>) -> RemoteStoreResult<(Vec<String>, Vec<String>)> {
  validate_selector_names(&first)?;
  validate_selector_names(&second)?;
  match first.cmp(&second) {
    std::cmp::Ordering::Less => Ok((first, second)),
    std::cmp::Ordering::Greater => Ok((second, first)),
    std::cmp::Ordering::Equal => Err(RemoteStoreError::integrity(
      "remote compiler environment conflict repeats one selector",
    )),
  }
}

fn strictly_sorted_unique(values: &[String]) -> bool {
  values.windows(2).all(|pair| pair[0] < pair[1])
}

fn valid_environment_name(name: &str) -> bool {
  !name.is_empty()
    && name.len() <= MAX_APPROVED_ENVIRONMENT_NAME_BYTES
    && !name.as_bytes().contains(&0)
    && !name.contains('=')
    && !name.chars().any(char::is_control)
}

fn environment_name_may_be_secret(name: &str) -> bool {
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

/// Redacted projection of one selected machine-owned target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RemoteCacheConfigurationStatus {
  pub(crate) alias: String,
  pub(crate) transport: &'static str,
  pub(crate) authority: String,
  pub(crate) role: &'static str,
  pub(crate) shared_environment_names: usize,
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

/// Strictly resolve credentials and verify the immutable S3 protocol marker.
pub(crate) fn probe(
  source_root: &Path,
  alias: Option<&str>,
) -> RemoteStoreResult<Option<RemoteCacheConfigurationStatus>> {
  let Some(alias) = alias else {
    return Ok(None);
  };
  let target = s3::S3Target::load(source_root, alias)?;
  let status = status(alias, &target);
  let _store = s3::connect(target)?;
  Ok(Some(status))
}

fn status(alias: &str, target: &s3::S3Target) -> RemoteCacheConfigurationStatus {
  RemoteCacheConfigurationStatus {
    alias: alias.to_string(),
    transport: "s3",
    authority: target.authority().as_str().to_string(),
    role: target.role_name(),
    shared_environment_names: target.shareable_environment_names().len(),
  }
}

/// Private wrapper capability for one command-owned coordinator.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoteWrapperContext {
  endpoint: String,
  token: String,
  authority: RemoteAuthorityId,
  approved_environment_names: Vec<String>,
}

impl RemoteWrapperContext {
  pub(crate) fn authority(&self) -> &RemoteAuthorityId {
    &self.authority
  }

  pub(crate) fn approves_environment_name(&self, name: &str) -> bool {
    self
      .approved_environment_names
      .binary_search_by(|approved| approved.as_str().cmp(name))
      .is_ok()
  }
}

/// One command-owned coordinator and its four loopback workers.
pub(crate) struct RemoteCoordinator {
  context: RemoteWrapperContext,
  publication_token: String,
  can_publish: bool,
  stop: Arc<AtomicBool>,
  workers: Mutex<Option<Vec<std::thread::JoinHandle<()>>>>,
  metrics: Arc<CoordinatorMetrics>,
}

#[derive(Debug, Default)]
struct CoordinatorMetrics {
  requests: AtomicU64,
  bytes: AtomicU64,
  hits: AtomicU64,
  misses: AtomicU64,
  conflicts: AtomicU64,
  failures: AtomicU64,
  publications: AtomicU64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct RemoteCoordinatorReport {
  pub(crate) requests: u64,
  pub(crate) bytes: u64,
  pub(crate) hits: u64,
  pub(crate) misses: u64,
  pub(crate) conflicts: u64,
  pub(crate) failures: u64,
  pub(crate) publications: u64,
}

impl RemoteCoordinator {
  pub(crate) fn prepare(source_root: &Path, alias: Option<&str>) -> RemoteStoreResult<Option<Self>> {
    let Some(alias) = alias else {
      return Ok(None);
    };
    Self::prepare_enabled(source_root, alias).map(Some)
  }

  fn prepare_enabled(source_root: &Path, alias: &str) -> RemoteStoreResult<Self> {
    let target = s3::S3Target::load(source_root, alias)?;
    let authority = target.authority().clone();
    let can_publish = target.can_write();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).map_err(io_unavailable)?;
    let endpoint = listener.local_addr().map_err(io_unavailable)?.to_string();
    let token = coordinator_token()?;
    let publication_token = coordinator_token()?;
    let context = RemoteWrapperContext {
      endpoint: endpoint.clone(),
      token: token.clone(),
      authority: authority.clone(),
      approved_environment_names: target.shareable_environment_names().to_vec(),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let metrics = Arc::new(CoordinatorMetrics::default());
    let shared = Arc::new(CoordinatorShared {
      target,
      authority,
      read_token: token,
      publication_token: publication_token.clone(),
      stop: Arc::clone(&stop),
      circuit_open: AtomicBool::new(false),
      store: OnceLock::new(),
      metrics: Arc::clone(&metrics),
    });
    let worker_listeners = (0..COORDINATOR_WORKERS)
      .map(|_| listener.try_clone().map_err(io_unavailable))
      .collect::<RemoteStoreResult<Vec<_>>>()?;
    let mut workers = Vec::with_capacity(COORDINATOR_WORKERS);
    for (index, worker_listener) in worker_listeners.into_iter().enumerate() {
      let worker_shared = Arc::clone(&shared);
      match std::thread::Builder::new()
        .name(format!("cargo-rail-remote-{index}"))
        .spawn(move || worker_loop(worker_listener, worker_shared))
      {
        Ok(worker) => workers.push(worker),
        Err(error) => {
          stop.store(true, Ordering::Release);
          wake_workers(&endpoint, workers.len());
          for worker in workers {
            let _ = worker.join();
          }
          return Err(io_unavailable(error));
        }
      }
    }
    drop(listener);
    Ok(Self {
      context,
      publication_token,
      can_publish,
      stop,
      workers: Mutex::new(Some(workers)),
      metrics,
    })
  }

  pub(crate) fn context(&self) -> RemoteWrapperContext {
    self.context.clone()
  }

  pub(crate) const fn can_publish(&self) -> bool {
    self.can_publish
  }

  pub(crate) fn authority(&self) -> &RemoteAuthorityId {
    self.context.authority()
  }

  pub(crate) fn publish(&self, action_key: &str, result_key: &str, base_action_key: &str) -> RemoteStoreResult<bool> {
    if !self.can_publish {
      return Ok(false);
    }
    let mut stream = connect_loopback(&self.context)?;
    write_request(
      &mut stream,
      &self.publication_token,
      IPC_PUBLISH,
      action_key,
      Some(result_key),
      Some(base_action_key),
    )?;
    match read_response_prelude(&mut stream, &self.context)? {
      RESPONSE_PUBLISHED => Ok(true),
      RESPONSE_SKIPPED => Ok(false),
      code => Err(response_error(code)),
    }
  }

  pub(crate) fn report(&self) -> RemoteCoordinatorReport {
    self.drain()
  }

  fn drain(&self) -> RemoteCoordinatorReport {
    self.stop.store(true, Ordering::Release);
    wake_workers(&self.context.endpoint, COORDINATOR_WORKERS);
    let mut workers = match self.workers.lock() {
      Ok(workers) => workers,
      Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(workers) = workers.take() {
      for worker in workers {
        let _ = worker.join();
      }
    }
    self.snapshot()
  }

  fn snapshot(&self) -> RemoteCoordinatorReport {
    RemoteCoordinatorReport {
      requests: self.metrics.requests.load(Ordering::Relaxed),
      bytes: self.metrics.bytes.load(Ordering::Relaxed),
      hits: self.metrics.hits.load(Ordering::Relaxed),
      misses: self.metrics.misses.load(Ordering::Relaxed),
      conflicts: self.metrics.conflicts.load(Ordering::Relaxed),
      failures: self.metrics.failures.load(Ordering::Relaxed),
      publications: self.metrics.publications.load(Ordering::Relaxed),
    }
  }
}

impl Drop for RemoteCoordinator {
  fn drop(&mut self) {
    let _ = self.drain();
  }
}

struct CoordinatorShared {
  target: s3::S3Target,
  authority: RemoteAuthorityId,
  read_token: String,
  publication_token: String,
  stop: Arc<AtomicBool>,
  circuit_open: AtomicBool,
  store: OnceLock<RemoteStoreResult<s3::S3Store>>,
  metrics: Arc<CoordinatorMetrics>,
}

impl CoordinatorShared {
  fn store(&self) -> RemoteStoreResult<&s3::S3Store> {
    if self.circuit_open.load(Ordering::Acquire) {
      return Err(RemoteStoreError::unavailable("remote cache circuit is open"));
    }
    match self.store.get_or_init(|| s3::connect(self.target.clone())) {
      Ok(store) => Ok(store),
      Err(error) => Err(error.clone()),
    }
  }

  fn record_failure(&self, error: &RemoteStoreError) {
    atomic_saturating_add(&self.metrics.failures, 1);
    if remote_failure_allows_fallback(error.fault) {
      self.circuit_open.store(true, Ordering::Release);
      warn_unavailable_once();
    }
  }
}

fn remote_failure_allows_fallback(fault: RemoteStoreFault) -> bool {
  !matches!(fault, RemoteStoreFault::Integrity)
}

fn worker_loop(listener: TcpListener, shared: Arc<CoordinatorShared>) {
  loop {
    match listener.accept() {
      Ok((mut stream, peer)) => {
        if shared.stop.load(Ordering::Acquire) {
          break;
        }
        if !peer.ip().is_loopback() {
          continue;
        }
        if let Err(error) = stream
          .set_nodelay(true)
          .and_then(|()| stream.set_read_timeout(Some(IPC_TRANSFER_TIMEOUT)))
          .and_then(|()| stream.set_write_timeout(Some(IPC_TRANSFER_TIMEOUT)))
        {
          shared.record_failure(&io_unavailable(error));
          continue;
        }
        handle_connection(&mut stream, &shared);
      }
      Err(error) => {
        if shared.stop.load(Ordering::Acquire) {
          break;
        }
        shared.record_failure(&io_unavailable(error));
        break;
      }
    }
  }
}

fn handle_connection(stream: &mut TcpStream, shared: &CoordinatorShared) {
  let request = match read_request(stream) {
    Ok(request) => request,
    Err(_) => return,
  };
  let expected_token = if request.operation == IPC_PUBLISH {
    &shared.publication_token
  } else {
    &shared.read_token
  };
  if !capability_matches(&request.token, expected_token) {
    let _ = write_response_prelude(stream, &shared.authority, RESPONSE_INTEGRITY);
    return;
  }
  atomic_saturating_add(&shared.metrics.requests, 1);
  if shared.circuit_open.load(Ordering::Acquire) {
    atomic_saturating_add(&shared.metrics.failures, 1);
    let _ = write_response_prelude(stream, &shared.authority, RESPONSE_UNAVAILABLE);
    return;
  }
  let result = match request.operation {
    IPC_RESOLVE_SELECTOR => handle_selector(stream, shared, &request.action_key),
    IPC_FETCH => handle_fetch(stream, shared, &request.action_key),
    IPC_PUBLISH => match (request.result_key.as_deref(), request.base_action_key.as_deref()) {
      (Some(result_key), Some(base_action_key)) => {
        match handle_publication(shared, &request.action_key, result_key, base_action_key) {
          Ok(published) => write_response_prelude(
            stream,
            &shared.authority,
            if published {
              RESPONSE_PUBLISHED
            } else {
              RESPONSE_SKIPPED
            },
          )
          .map_err(HandleFailure::AfterResponse),
          Err(error) => Err(HandleFailure::BeforeResponse(error)),
        }
      }
      _ => Err(HandleFailure::BeforeResponse(RemoteStoreError::integrity(
        "remote publication omitted required identities",
      ))),
    },
    _ => Err(HandleFailure::BeforeResponse(RemoteStoreError::integrity(
      "remote coordinator operation is invalid",
    ))),
  };
  if let Err(failure) = result {
    let (error, response_started) = match failure {
      HandleFailure::BeforeResponse(error) => (error, false),
      HandleFailure::AfterResponse(error) => (error, true),
      HandleFailure::Abandoned => return,
    };
    shared.record_failure(&error);
    if response_started {
      let _ = stream.shutdown(Shutdown::Both);
    } else {
      let _ = write_response_prelude(stream, &shared.authority, response_code(error.fault));
    }
  }
}

enum HandleFailure {
  BeforeResponse(RemoteStoreError),
  AfterResponse(RemoteStoreError),
  Abandoned,
}

fn handle_selector(
  stream: &mut TcpStream,
  shared: &CoordinatorShared,
  base_action_key: &str,
) -> Result<(), HandleFailure> {
  let resolution = shared
    .store()
    .and_then(|store| store.resolve_selector(base_action_key))
    .map_err(HandleFailure::BeforeResponse)?;
  match resolution {
    RemoteSelectorResolution::Miss => {
      atomic_saturating_add(&shared.metrics.misses, 1);
      write_response_prelude(stream, &shared.authority, RESPONSE_MISS).map_err(HandleFailure::AfterResponse)
    }
    RemoteSelectorResolution::Unique(names) => {
      atomic_saturating_add(&shared.metrics.hits, 1);
      write_response_prelude(stream, &shared.authority, RESPONSE_UNIQUE)
        .and_then(|()| write_selector_names(stream, &names))
        .map_err(HandleFailure::AfterResponse)
    }
    RemoteSelectorResolution::Conflict(first, second) => {
      atomic_saturating_add(&shared.metrics.conflicts, 1);
      write_response_prelude(stream, &shared.authority, RESPONSE_CONFLICT)
        .and_then(|()| write_selector_names(stream, &first))
        .and_then(|()| write_selector_names(stream, &second))
        .map_err(HandleFailure::AfterResponse)
    }
  }
}

fn handle_fetch(stream: &mut TcpStream, shared: &CoordinatorShared, action_key: &str) -> Result<(), HandleFailure> {
  let store = shared.store().map_err(HandleFailure::BeforeResponse)?;
  match store.lookup(action_key).map_err(HandleFailure::BeforeResponse)? {
    s3::S3Lookup::Miss => {
      atomic_saturating_add(&shared.metrics.misses, 1);
      write_response_prelude(stream, &shared.authority, RESPONSE_MISS).map_err(HandleFailure::AfterResponse)
    }
    s3::S3Lookup::Expired => {
      atomic_saturating_add(&shared.metrics.misses, 1);
      write_response_prelude(stream, &shared.authority, RESPONSE_EXPIRED).map_err(HandleFailure::AfterResponse)
    }
    s3::S3Lookup::Conflict(first, second) => {
      atomic_saturating_add(&shared.metrics.conflicts, 1);
      write_response_prelude(stream, &shared.authority, RESPONSE_CONFLICT)
        .and_then(|()| write_string(stream, &first, IPC_MAX_IDENTITY_BYTES))
        .and_then(|()| write_string(stream, &second, IPC_MAX_IDENTITY_BYTES))
        .map_err(HandleFailure::AfterResponse)
    }
    s3::S3Lookup::Unique { result_key, result } => {
      let length = result.bytes();
      write_response_prelude(stream, &shared.authority, RESPONSE_PACK)
        .and_then(|()| write_string(stream, &result_key, IPC_MAX_IDENTITY_BYTES))
        .and_then(|()| stream.write_all(&length.to_le_bytes()).map_err(io_unavailable))
        .map_err(HandleFailure::AfterResponse)?;
      copy_result_to_client(stream, |output| store.copy_result(result, output))?;
      atomic_saturating_add(&shared.metrics.bytes, length);
      atomic_saturating_add(&shared.metrics.hits, 1);
      Ok(())
    }
  }
}

struct ClientOutput<'a, W> {
  inner: &'a mut W,
  failed: bool,
}

impl<W: Write> Write for ClientOutput<'_, W> {
  fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
    match self.inner.write(bytes) {
      Ok(0) if !bytes.is_empty() => {
        self.failed = true;
        Ok(0)
      }
      Ok(written) => Ok(written),
      Err(error) => {
        self.failed = true;
        Err(error)
      }
    }
  }

  fn flush(&mut self) -> std::io::Result<()> {
    match self.inner.flush() {
      Ok(()) => Ok(()),
      Err(error) => {
        self.failed = true;
        Err(error)
      }
    }
  }
}

fn copy_result_to_client<W: Write>(
  output: &mut W,
  copy: impl FnOnce(&mut ClientOutput<'_, W>) -> RemoteStoreResult<u64>,
) -> Result<u64, HandleFailure> {
  let mut output = ClientOutput {
    inner: output,
    failed: false,
  };
  match copy(&mut output) {
    Ok(bytes) => Ok(bytes),
    Err(_) if output.failed => Err(HandleFailure::Abandoned),
    Err(error) => Err(HandleFailure::AfterResponse(error)),
  }
}

fn handle_publication(
  shared: &CoordinatorShared,
  action_key: &str,
  result_key: &str,
  base_action_key: &str,
) -> RemoteStoreResult<bool> {
  if !shared.target.can_write() {
    return Ok(false);
  }
  let cas = crate::cache::cas::LocalCas::open_initialized()
    .map_err(|error| RemoteStoreError::unavailable(error.to_string()))?;
  let crate::cache::cas::NativeActionLookup::Hit(hit) = cas
    .native_action(action_key)
    .map_err(|error| RemoteStoreError::unavailable(error.to_string()))?
  else {
    return Ok(false);
  };
  if hit.validation.result_key() != result_key {
    return Ok(false);
  }
  let environment_names = match hit.validate_remote_publication(base_action_key) {
    Ok(names) => names.to_vec(),
    Err(_) => return Ok(false),
  };
  if !hit
    .validation
    .remote_environment_is_approved(shared.target.shareable_environment_names())
  {
    return Ok(false);
  }
  let association = hit
    .association()
    .map_err(|error| RemoteStoreError::integrity(error.to_string()))?;
  let mut pack = tempfile::tempfile().map_err(io_unavailable)?;
  let export = hit
    .export_pack(&mut pack)
    .map_err(|error| RemoteStoreError::integrity(error.to_string()))?;
  if export.content_length != association.pack_length() || export.bytes_written != association.pack_length() {
    return Err(RemoteStoreError::integrity(
      "local result export does not match its verified association",
    ));
  }
  drop(hit);
  let store = shared.store()?;
  match store.publish(&association, base_action_key, &environment_names, pack)? {
    s3::S3Publication::SelectorConflict(_, _) => {
      atomic_saturating_add(&shared.metrics.conflicts, 1);
      return Err(RemoteStoreError::integrity(
        "remote compiler environment selector is conflicted",
      ));
    }
    s3::S3Publication::ResultConflict(first, second) => {
      atomic_saturating_add(&shared.metrics.conflicts, 1);
      cas
        .record_remote_conflict(action_key, &first, &second)
        .map_err(|error| RemoteStoreError::integrity(error.to_string()))?;
      return Err(RemoteStoreError::integrity("remote compiler action is conflicted"));
    }
    s3::S3Publication::Unique { .. } => {}
  }
  match store.lookup(action_key)? {
    s3::S3Lookup::Unique {
      result_key: committed,
      result: _,
    } if committed == result_key => {
      if !cas
        .attach_remote_origin(action_key, result_key, &shared.authority)
        .map_err(|error| RemoteStoreError::integrity(error.to_string()))?
      {
        return Err(RemoteStoreError::integrity(
          "published result no longer has matching local authority",
        ));
      }
    }
    s3::S3Lookup::Conflict(first, second) => {
      atomic_saturating_add(&shared.metrics.conflicts, 1);
      cas
        .record_remote_conflict(action_key, &first, &second)
        .map_err(|error| RemoteStoreError::integrity(error.to_string()))?;
      return Err(RemoteStoreError::integrity(
        "remote compiler action conflicted after publication",
      ));
    }
    s3::S3Lookup::Miss | s3::S3Lookup::Expired | s3::S3Lookup::Unique { .. } => {
      return Err(RemoteStoreError::integrity(
        "action-last publication did not expose the published result",
      ));
    }
  }
  atomic_saturating_add(&shared.metrics.bytes, association.pack_length());
  atomic_saturating_add(&shared.metrics.publications, 1);
  Ok(true)
}

fn atomic_saturating_add(value: &AtomicU64, amount: u64) {
  let _ = value.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
    Some(current.saturating_add(amount))
  });
}

fn wake_workers(endpoint: &str, count: usize) {
  for _ in 0..count {
    let _ = TcpStream::connect(endpoint);
  }
}

fn response_code(fault: RemoteStoreFault) -> u8 {
  match fault {
    RemoteStoreFault::Integrity => RESPONSE_INTEGRITY,
    RemoteStoreFault::Authentication | RemoteStoreFault::Unavailable | RemoteStoreFault::Configuration => {
      RESPONSE_UNAVAILABLE
    }
  }
}

fn capability_matches(actual: &str, expected: &str) -> bool {
  let mut difference = actual.len() ^ expected.len();
  for (actual, expected) in actual.bytes().zip(expected.bytes()) {
    difference |= usize::from(actual ^ expected);
  }
  difference == 0
}

struct IpcRequest {
  token: String,
  operation: u8,
  action_key: String,
  result_key: Option<String>,
  base_action_key: Option<String>,
}

fn read_request(stream: &mut TcpStream) -> RemoteStoreResult<IpcRequest> {
  let mut magic = [0_u8; 8];
  stream.read_exact(&mut magic).map_err(io_unavailable)?;
  if &magic != IPC_MAGIC {
    return Err(RemoteStoreError::integrity(
      "remote coordinator request has invalid magic",
    ));
  }
  let token = read_string(stream, IPC_MAX_TOKEN_BYTES)?;
  let mut operation = [0_u8; 1];
  stream.read_exact(&mut operation).map_err(io_unavailable)?;
  let action_key = read_string(stream, IPC_MAX_IDENTITY_BYTES)?;
  if operation[0] == IPC_RESOLVE_SELECTOR {
    crate::compiler::native_cache::validate_base_action_key(&action_key)
      .map_err(|error| RemoteStoreError::integrity(error.to_string()))?;
  } else {
    crate::compiler::native_cache::validate_action_key(&action_key)
      .map_err(|error| RemoteStoreError::integrity(error.to_string()))?;
  }
  let result_key = if operation[0] == IPC_PUBLISH {
    let result = read_string(stream, IPC_MAX_IDENTITY_BYTES)?;
    validate_remote_result_key(&result)?;
    Some(result)
  } else {
    None
  };
  let base_action_key = if operation[0] == IPC_PUBLISH {
    let base = read_string(stream, IPC_MAX_IDENTITY_BYTES)?;
    crate::compiler::native_cache::validate_base_action_key(&base)
      .map_err(|error| RemoteStoreError::integrity(error.to_string()))?;
    Some(base)
  } else {
    None
  };
  Ok(IpcRequest {
    token,
    operation: operation[0],
    action_key,
    result_key,
    base_action_key,
  })
}

fn write_request(
  stream: &mut TcpStream,
  token: &str,
  operation: u8,
  action_key: &str,
  result_key: Option<&str>,
  base_action_key: Option<&str>,
) -> RemoteStoreResult<()> {
  if !matches!(
    (operation, result_key.is_some(), base_action_key.is_some()),
    (IPC_RESOLVE_SELECTOR | IPC_FETCH, false, false) | (IPC_PUBLISH, true, true)
  ) {
    return Err(RemoteStoreError::integrity(
      "remote coordinator request fields do not match its operation",
    ));
  }
  if operation == IPC_RESOLVE_SELECTOR {
    crate::compiler::native_cache::validate_base_action_key(action_key)
      .map_err(|error| RemoteStoreError::integrity(error.to_string()))?;
  } else {
    crate::compiler::native_cache::validate_action_key(action_key)
      .map_err(|error| RemoteStoreError::integrity(error.to_string()))?;
  }
  stream.write_all(IPC_MAGIC).map_err(io_unavailable)?;
  write_string(stream, token, IPC_MAX_TOKEN_BYTES)?;
  stream.write_all(&[operation]).map_err(io_unavailable)?;
  write_string(stream, action_key, IPC_MAX_IDENTITY_BYTES)?;
  if let Some(result_key) = result_key {
    validate_remote_result_key(result_key)?;
    write_string(stream, result_key, IPC_MAX_IDENTITY_BYTES)?;
  }
  if let Some(base_action_key) = base_action_key {
    crate::compiler::native_cache::validate_base_action_key(base_action_key)
      .map_err(|error| RemoteStoreError::integrity(error.to_string()))?;
    write_string(stream, base_action_key, IPC_MAX_IDENTITY_BYTES)?;
  }
  Ok(())
}

fn write_response_prelude(stream: &mut TcpStream, authority: &RemoteAuthorityId, status: u8) -> RemoteStoreResult<()> {
  stream.write_all(IPC_RESPONSE_MAGIC).map_err(io_unavailable)?;
  write_string(stream, authority.as_str(), IPC_MAX_IDENTITY_BYTES)?;
  stream.write_all(&[status]).map_err(io_unavailable)
}

fn read_response_prelude(stream: &mut TcpStream, context: &RemoteWrapperContext) -> RemoteStoreResult<u8> {
  let mut magic = [0_u8; 8];
  stream.read_exact(&mut magic).map_err(io_unavailable)?;
  if &magic != IPC_RESPONSE_MAGIC {
    return Err(RemoteStoreError::integrity(
      "remote coordinator response has invalid magic",
    ));
  }
  let authority = RemoteAuthorityId::parse(read_string(stream, IPC_MAX_IDENTITY_BYTES)?)
    .map_err(|error| RemoteStoreError::integrity(error.to_string()))?;
  if authority != context.authority {
    return Err(RemoteStoreError::integrity(
      "remote coordinator returned the wrong authority",
    ));
  }
  let mut status = [0_u8; 1];
  stream.read_exact(&mut status).map_err(io_unavailable)?;
  Ok(status[0])
}

fn write_selector_names(stream: &mut TcpStream, names: &[String]) -> RemoteStoreResult<()> {
  validate_selector_names(names)?;
  let count = u16::try_from(names.len())
    .map_err(|_| RemoteStoreError::integrity("remote selector name count is out of range"))?;
  stream.write_all(&count.to_le_bytes()).map_err(io_unavailable)?;
  for name in names {
    write_string(stream, name, MAX_APPROVED_ENVIRONMENT_NAME_BYTES)?;
  }
  Ok(())
}

fn read_selector_names(stream: &mut TcpStream) -> RemoteStoreResult<Vec<String>> {
  let mut count = [0_u8; 2];
  stream.read_exact(&mut count).map_err(io_unavailable)?;
  let count = usize::from(u16::from_le_bytes(count));
  if count > MAX_APPROVED_ENVIRONMENT_NAMES {
    return Err(RemoteStoreError::integrity(
      "remote selector name count exceeds its bound",
    ));
  }
  let mut names = Vec::with_capacity(count);
  for _ in 0..count {
    names.push(read_string(stream, MAX_APPROVED_ENVIRONMENT_NAME_BYTES)?);
  }
  validate_selector_names(&names)?;
  Ok(names)
}

fn write_string(stream: &mut TcpStream, value: &str, maximum: usize) -> RemoteStoreResult<()> {
  if value.is_empty() || value.len() > maximum {
    return Err(RemoteStoreError::integrity(
      "remote coordinator string is empty or over its bound",
    ));
  }
  let length = u16::try_from(value.len())
    .map_err(|_| RemoteStoreError::integrity("remote coordinator string length is out of range"))?;
  stream.write_all(&length.to_le_bytes()).map_err(io_unavailable)?;
  stream.write_all(value.as_bytes()).map_err(io_unavailable)
}

fn read_string(stream: &mut TcpStream, maximum: usize) -> RemoteStoreResult<String> {
  let mut length = [0_u8; 2];
  stream.read_exact(&mut length).map_err(io_unavailable)?;
  let length = usize::from(u16::from_le_bytes(length));
  if length == 0 || length > maximum {
    return Err(RemoteStoreError::integrity(
      "remote coordinator string is empty or over its bound",
    ));
  }
  let mut bytes = vec![0_u8; length];
  stream.read_exact(&mut bytes).map_err(io_unavailable)?;
  String::from_utf8(bytes).map_err(|_| RemoteStoreError::integrity("remote coordinator string is not UTF-8"))
}

fn connect_loopback(context: &RemoteWrapperContext) -> RemoteStoreResult<TcpStream> {
  let address = context
    .endpoint
    .parse::<SocketAddr>()
    .map_err(|_| RemoteStoreError::integrity("remote coordinator endpoint is invalid"))?;
  if !address.ip().is_loopback() {
    return Err(RemoteStoreError::integrity(
      "remote coordinator endpoint is not loopback",
    ));
  }
  let stream = TcpStream::connect_timeout(&address, IPC_CONNECT_TIMEOUT).map_err(io_unavailable)?;
  stream.set_nodelay(true).map_err(io_unavailable)?;
  stream
    .set_read_timeout(Some(IPC_TRANSFER_TIMEOUT))
    .map_err(io_unavailable)?;
  stream
    .set_write_timeout(Some(IPC_TRANSFER_TIMEOUT))
    .map_err(io_unavailable)?;
  Ok(stream)
}

fn response_error(code: u8) -> RemoteStoreError {
  match code {
    RESPONSE_INTEGRITY => RemoteStoreError::integrity("remote coordinator reported an integrity failure"),
    RESPONSE_UNAVAILABLE => RemoteStoreError::unavailable("remote coordinator is unavailable"),
    _ => RemoteStoreError::integrity("remote coordinator returned an unknown status"),
  }
}

fn coordinator_token() -> RemoteStoreResult<String> {
  let mut bytes = [0_u8; 32];
  getrandom::fill(&mut bytes)
    .map_err(|_| RemoteStoreError::unavailable("remote coordinator capability could not be created"))?;
  let mut encoded = String::with_capacity(bytes.len() * 2);
  for byte in bytes {
    use std::fmt::Write as _;

    write!(encoded, "{byte:02x}")
      .map_err(|_| RemoteStoreError::unavailable("remote coordinator capability could not be encoded"))?;
  }
  Ok(encoded)
}

/// One bounded pack stream from the command-owned coordinator.
pub(crate) struct RemotePackReader {
  stream: TcpStream,
  remaining: u64,
  transport_failed: bool,
}

impl RemotePackReader {
  pub(crate) const fn transport_failed(&self) -> bool {
    self.transport_failed
  }
}

impl Read for RemotePackReader {
  fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
    if self.remaining == 0 || output.is_empty() {
      return Ok(0);
    }
    let maximum = usize::try_from(self.remaining.min(output.len() as u64)).unwrap_or(output.len());
    match self.stream.read(&mut output[..maximum]) {
      Ok(0) => {
        self.transport_failed = true;
        Err(std::io::Error::new(
          std::io::ErrorKind::UnexpectedEof,
          "remote pack stream ended before its declared length",
        ))
      }
      Ok(read) => {
        self.remaining = self.remaining.saturating_sub(read as u64);
        Ok(read)
      }
      Err(error) => {
        self.transport_failed = true;
        Err(error)
      }
    }
  }
}

/// Combined action/result fetch outcome.
pub(crate) enum RemoteFetch {
  Miss,
  Expired,
  Conflict(RemoteResultIdentity, RemoteResultIdentity),
  Unique {
    result_key: String,
    stream: RemotePackReader,
    length: u64,
  },
}

pub(crate) fn resolve_remote_selector(
  context: &RemoteWrapperContext,
  base_action_key: &str,
) -> RemoteStoreResult<RemoteSelectorResolution> {
  let mut stream = connect_loopback(context)?;
  write_request(
    &mut stream,
    &context.token,
    IPC_RESOLVE_SELECTOR,
    base_action_key,
    None,
    None,
  )?;
  match read_response_prelude(&mut stream, context)? {
    RESPONSE_MISS => Ok(RemoteSelectorResolution::Miss),
    RESPONSE_UNIQUE => Ok(RemoteSelectorResolution::Unique(read_selector_names(&mut stream)?)),
    RESPONSE_CONFLICT => {
      let first = read_selector_names(&mut stream)?;
      let second = read_selector_names(&mut stream)?;
      if first >= second {
        return Err(RemoteStoreError::integrity(
          "remote selector conflict response is not canonical",
        ));
      }
      Ok(RemoteSelectorResolution::Conflict(first, second))
    }
    code => Err(response_error(code)),
  }
}

pub(crate) fn fetch_remote(context: &RemoteWrapperContext, action_key: &str) -> RemoteStoreResult<RemoteFetch> {
  let mut stream = connect_loopback(context)?;
  write_request(&mut stream, &context.token, IPC_FETCH, action_key, None, None)?;
  match read_response_prelude(&mut stream, context)? {
    RESPONSE_MISS => Ok(RemoteFetch::Miss),
    RESPONSE_EXPIRED => Ok(RemoteFetch::Expired),
    RESPONSE_CONFLICT => {
      let first = read_string(&mut stream, IPC_MAX_IDENTITY_BYTES)?;
      let second = read_string(&mut stream, IPC_MAX_IDENTITY_BYTES)?;
      validate_remote_result_key(&first)?;
      validate_remote_result_key(&second)?;
      if first >= second {
        return Err(RemoteStoreError::integrity(
          "remote action conflict response is not canonical",
        ));
      }
      Ok(RemoteFetch::Conflict(
        RemoteResultIdentity(first),
        RemoteResultIdentity(second),
      ))
    }
    RESPONSE_PACK => {
      let result_key = read_string(&mut stream, IPC_MAX_IDENTITY_BYTES)?;
      validate_remote_result_key(&result_key)?;
      let mut length = [0_u8; 8];
      stream.read_exact(&mut length).map_err(io_unavailable)?;
      let length = u64::from_le_bytes(length);
      if length > crate::compiler::native_cache::pack::MAX_PACK_BYTES {
        return Err(RemoteStoreError::integrity(
          "remote pack exceeds its absolute byte bound",
        ));
      }
      Ok(RemoteFetch::Unique {
        result_key,
        stream: RemotePackReader {
          stream,
          remaining: length,
          transport_failed: false,
        },
        length,
      })
    }
    code => Err(response_error(code)),
  }
}

fn io_unavailable(error: std::io::Error) -> RemoteStoreError {
  RemoteStoreError::unavailable(error.to_string())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn action(byte: char) -> String {
    format!(
      "{}{}",
      crate::compiler::native_cache::ACTION_KEY_PREFIX,
      byte.to_string().repeat(64)
    )
  }

  fn result(byte: char) -> String {
    format!(
      "{}{}",
      crate::compiler::native_cache::RESULT_KEY_PREFIX,
      byte.to_string().repeat(64)
    )
  }

  fn authority() -> RemoteAuthorityId {
    RemoteAuthorityId::parse(format!("remote-authority-v1-sha256-{}", "0".repeat(64))).expect("authority")
  }

  #[test]
  fn coordinator_capabilities_use_independent_fixed_hex_tokens() {
    let read = coordinator_token().expect("read capability");
    let publish = coordinator_token().expect("publish capability");
    for token in [&read, &publish] {
      assert_eq!(token.len(), 64);
      assert!(
        token
          .bytes()
          .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
      );
    }
    assert_ne!(read, publish);
  }

  #[test]
  fn action_and_selector_codecs_are_canonical() {
    let first = result('1');
    let second = result('2');
    let unique = RemoteActionState::unique(&first).expect("unique");
    assert_eq!(
      RemoteActionState::decode(&unique.encode().expect("encode")).expect("decode"),
      unique
    );
    let conflict = RemoteActionState::conflict(&second, &first).expect("conflict");
    assert_eq!(
      RemoteActionState::decode(&conflict.encode().expect("encode")).expect("decode"),
      conflict
    );

    let selector = RemoteSelectorState::conflict(vec!["RUSTFLAGS".to_string()], Vec::new()).expect("selector");
    assert_eq!(
      selector.into_resolution().expect("resolution"),
      RemoteSelectorResolution::Conflict(Vec::new(), vec!["RUSTFLAGS".to_string()])
    );
  }

  #[test]
  fn child_scrubbing_removes_remote_authority_but_preserves_region() {
    let mut command = std::process::Command::new("unused");
    for name in PRIVATE_REMOTE_ENVIRONMENT {
      command.env(name, "private");
    }
    command.env("AWS_REGION", "us-east-1");
    command.env("AWS_DEFAULT_REGION", "us-west-2");

    scrub_child_environment(&mut command);

    let environment = command
      .get_envs()
      .map(|(name, value)| {
        (
          name.to_string_lossy().into_owned(),
          value.map(|value| value.to_string_lossy().into_owned()),
        )
      })
      .collect::<std::collections::BTreeMap<_, _>>();
    for name in PRIVATE_REMOTE_ENVIRONMENT {
      assert_eq!(environment.get(*name), Some(&None), "{name} must be removed");
    }
    assert_eq!(environment.get("AWS_REGION"), Some(&Some("us-east-1".to_string())));
    assert_eq!(
      environment.get("AWS_DEFAULT_REGION"),
      Some(&Some("us-west-2".to_string()))
    );
  }

  #[test]
  fn abandoned_client_output_is_not_a_remote_failure() {
    struct BrokenOutput;

    impl Write for BrokenOutput {
      fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "client closed"))
      }

      fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
      }
    }

    let mut broken = BrokenOutput;
    let abandoned = copy_result_to_client(&mut broken, |output| {
      output.write_all(b"pack").map_err(io_unavailable)?;
      Ok(4)
    });
    assert!(matches!(abandoned, Err(HandleFailure::Abandoned)));

    let mut healthy = Vec::new();
    let remote = copy_result_to_client(&mut healthy, |_| {
      Err(RemoteStoreError::unavailable("remote stream failed"))
    });
    assert!(matches!(
      remote,
      Err(HandleFailure::AfterResponse(error)) if error.fault == RemoteStoreFault::Unavailable
    ));
  }

  #[test]
  fn combined_fetch_ipc_streams_one_exact_body() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("listener");
    let endpoint = listener.local_addr().expect("endpoint").to_string();
    let context = RemoteWrapperContext {
      endpoint,
      token: "read-capability".to_string(),
      authority: authority(),
      approved_environment_names: Vec::new(),
    };
    let expected_action = action('a');
    let expected_result = result('b');
    let server_context = context.clone();
    let server_action = expected_action.clone();
    let server_result = expected_result.clone();
    let server = std::thread::spawn(move || {
      let (mut stream, _) = listener.accept().expect("accept");
      let request = read_request(&mut stream).expect("request");
      assert_eq!(request.operation, IPC_FETCH);
      assert_eq!(request.action_key, server_action);
      write_response_prelude(&mut stream, &server_context.authority, RESPONSE_PACK).expect("prelude");
      write_string(&mut stream, &server_result, IPC_MAX_IDENTITY_BYTES).expect("result");
      stream.write_all(&3_u64.to_le_bytes()).expect("length");
      stream.write_all(b"pack").expect("body with one extra byte");
    });
    let RemoteFetch::Unique {
      result_key,
      mut stream,
      length,
    } = fetch_remote(&context, &expected_action).expect("fetch")
    else {
      panic!("unique fetch expected");
    };
    assert_eq!(result_key, expected_result);
    assert_eq!(length, 3);
    let mut body = Vec::new();
    stream.read_to_end(&mut body).expect("body");
    assert_eq!(body, b"pac");
    assert!(!stream.transport_failed());
    server.join().expect("server");
  }

  #[test]
  fn combined_fetch_marks_a_short_body_as_transport_failure() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("listener");
    let context = RemoteWrapperContext {
      endpoint: listener.local_addr().expect("endpoint").to_string(),
      token: "read-capability".to_string(),
      authority: authority(),
      approved_environment_names: Vec::new(),
    };
    let expected_action = action('a');
    let expected_result = result('b');
    let server_context = context.clone();
    let server = std::thread::spawn(move || {
      let (mut stream, _) = listener.accept().expect("accept");
      let request = read_request(&mut stream).expect("request");
      assert_eq!(request.action_key, expected_action);
      write_response_prelude(&mut stream, &server_context.authority, RESPONSE_PACK).expect("prelude");
      write_string(&mut stream, &expected_result, IPC_MAX_IDENTITY_BYTES).expect("result");
      stream.write_all(&4_u64.to_le_bytes()).expect("length");
      stream.write_all(b"pac").expect("short body");
    });
    let RemoteFetch::Unique { mut stream, .. } = fetch_remote(&context, &action('a')).expect("fetch") else {
      panic!("unique fetch expected");
    };
    let mut body = Vec::new();
    assert_eq!(
      stream.read_to_end(&mut body).expect_err("short body must fail").kind(),
      std::io::ErrorKind::UnexpectedEof
    );
    assert_eq!(body, b"pac");
    assert!(stream.transport_failed());
    server.join().expect("server");
  }

  #[test]
  fn response_authority_mismatch_is_integrity_failure() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("listener");
    let context = RemoteWrapperContext {
      endpoint: listener.local_addr().expect("endpoint").to_string(),
      token: "read-capability".to_string(),
      authority: authority(),
      approved_environment_names: Vec::new(),
    };
    let server = std::thread::spawn(move || {
      let (mut stream, _) = listener.accept().expect("accept");
      let _ = read_request(&mut stream).expect("request");
      let wrong =
        RemoteAuthorityId::parse(format!("remote-authority-v1-sha256-{}", "1".repeat(64))).expect("wrong authority");
      write_response_prelude(&mut stream, &wrong, RESPONSE_MISS).expect("response");
    });
    let error = match fetch_remote(&context, &action('a')) {
      Ok(_) => panic!("wrong authority must fail"),
      Err(error) => error,
    };
    assert_eq!(error.fault, RemoteStoreFault::Integrity);
    server.join().expect("server");
  }

  #[test]
  fn report_shutdown_race_joins_four_workers_once() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("listener");
    let endpoint = listener.local_addr().expect("endpoint").to_string();
    let stop = Arc::new(AtomicBool::new(false));
    let metrics = Arc::new(CoordinatorMetrics::default());
    atomic_saturating_add(&metrics.requests, 1);
    atomic_saturating_add(&metrics.failures, 1);
    let workers = (0..COORDINATOR_WORKERS)
      .map(|_| {
        let stop = Arc::clone(&stop);
        let listener = listener.try_clone().expect("clone listener");
        std::thread::spawn(move || {
          while listener.accept().is_ok() {
            if stop.load(Ordering::Acquire) {
              break;
            }
          }
        })
      })
      .collect();
    drop(listener);
    let coordinator = Arc::new(RemoteCoordinator {
      context: RemoteWrapperContext {
        endpoint,
        token: "read-capability".to_string(),
        authority: authority(),
        approved_environment_names: Vec::new(),
      },
      publication_token: "publish-capability".to_string(),
      can_publish: false,
      stop,
      workers: Mutex::new(Some(workers)),
      metrics,
    });
    let expected = RemoteCoordinatorReport {
      requests: 1,
      failures: 1,
      ..RemoteCoordinatorReport::default()
    };
    let first = Arc::clone(&coordinator);
    let second = Arc::clone(&coordinator);
    let first = std::thread::spawn(move || first.report());
    let second = std::thread::spawn(move || second.report());
    assert_eq!(first.join().expect("first report"), expected);
    assert_eq!(second.join().expect("second report"), expected);
    assert_eq!(coordinator.report(), expected);
  }
}
