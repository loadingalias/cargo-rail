//! Direct bounded S3 transport for the native compiler-result protocol.

use std::fs::File;
use std::future::Future;
use std::io::Seek as _;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use aws_config::BehaviorVersion;
use aws_sdk_s3::config::Credentials;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::config::retry::RetryConfig;
use aws_sdk_s3::config::timeout::TimeoutConfig;
use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::operation::get_object::GetObjectError;
use aws_sdk_s3::operation::put_object::PutObjectError;
use aws_sdk_s3::primitives::{ByteStream, Length};
use aws_types::service_config::ServiceConfigKey;
use tokio::io::{AsyncRead, AsyncReadExt as _};

use super::object::{
  ENTRY_PRELUDE_BYTES, EntryBody, EntryRecord, EntryState, PutCondition, PutOutcome, STREAM_BUFFER_BYTES, StoredEntry,
  TransferMetrics,
};
use super::{RemoteCacheSelection, RemoteStoreError, RemoteStoreResult};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CREDENTIAL_TIMEOUT: Duration = Duration::from_secs(30);
const READ_TIMEOUT: Duration = Duration::from_secs(60);
const OPERATION_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const STREAM_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Default)]
struct TransferCounters {
  request_attempts: AtomicU64,
  payload_bytes_read: AtomicU64,
  payload_bytes_written: AtomicU64,
}

impl TransferCounters {
  fn snapshot(&self) -> TransferMetrics {
    TransferMetrics {
      request_attempts: self.request_attempts.load(Ordering::Relaxed),
      payload_bytes_read: self.payload_bytes_read.load(Ordering::Relaxed),
      payload_bytes_written: self.payload_bytes_written.load(Ordering::Relaxed),
      service_elapsed_ns: 0,
    }
  }

  fn take(&self) -> TransferMetrics {
    TransferMetrics {
      request_attempts: self.request_attempts.swap(0, Ordering::AcqRel),
      payload_bytes_read: self.payload_bytes_read.swap(0, Ordering::AcqRel),
      payload_bytes_written: self.payload_bytes_written.swap(0, Ordering::AcqRel),
      service_elapsed_ns: 0,
    }
  }
}

#[derive(Debug)]
struct RequestMetricsInterceptor {
  counters: Arc<TransferCounters>,
}

impl aws_sdk_s3::config::Intercept for RequestMetricsInterceptor {
  fn name(&self) -> &'static str {
    "CargoRailRemoteCacheRequestMetrics"
  }

  fn read_before_attempt(
    &self,
    context: &aws_sdk_s3::config::interceptors::BeforeTransmitInterceptorContextRef<'_>,
    _runtime_components: &aws_sdk_s3::config::RuntimeComponents,
    _config: &mut aws_sdk_s3::config::ConfigBag,
  ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    self.counters.request_attempts.fetch_add(1, Ordering::Relaxed);
    let request = context.request();
    if request.method() == "PUT"
      && let Some(bytes) = request
        .headers()
        .get("x-amz-decoded-content-length")
        .or_else(|| request.headers().get("content-length"))
        .and_then(|value| value.parse::<u64>().ok())
    {
      self.counters.payload_bytes_written.fetch_add(bytes, Ordering::Relaxed);
    }
    Ok(())
  }
}

#[derive(Clone)]
struct S3Target {
  region: String,
  expected_owner: Option<String>,
  bucket: String,
  endpoint: Option<String>,
  force_path_style: bool,
  loopback_credentials: bool,
}

impl S3Target {
  fn from_selection(selection: &RemoteCacheSelection) -> RemoteStoreResult<Self> {
    let authority = &selection.authority;
    if !authority.supports_s3_transport() {
      return Err(RemoteStoreError::configuration(
        "selected remote provider is not qualified for direct transport",
      ));
    }
    let expected_owner = authority.expected_owner().map(str::to_string);
    if authority.is_official_aws() != expected_owner.is_some() {
      return Err(RemoteStoreError::integrity(
        "remote S3 authority has an inconsistent expected owner",
      ));
    }
    Ok(Self {
      region: authority.region().to_string(),
      expected_owner,
      bucket: authority.bucket().to_string(),
      endpoint: (!authority.is_official_aws()).then(|| authority.endpoint().to_string()),
      force_path_style: authority.addressing() == "path",
      loopback_credentials: authority.is_loopback_fixture(),
    })
  }
}

type AsyncEntryStream = Pin<Box<dyn AsyncRead + Send>>;

struct RemotePayload {
  runtime: Arc<tokio::runtime::Runtime>,
  stream: AsyncEntryStream,
  remaining: u64,
  deadline: Instant,
}

impl std::io::Read for RemotePayload {
  fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
    if output.is_empty() || self.remaining == 0 {
      return Ok(0);
    }
    let maximum = usize::try_from(self.remaining.min(output.len() as u64)).unwrap_or(output.len());
    let timeout = self
      .deadline
      .checked_duration_since(Instant::now())
      .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::TimedOut, "remote entry stream timed out"))?;
    let runtime = &self.runtime;
    let stream = &mut self.stream;
    let read =
      runtime.block_on(async { tokio::time::timeout(timeout, stream.as_mut().read(&mut output[..maximum])).await });
    let read = match read {
      Ok(Ok(read)) => read,
      Ok(Err(_)) => return Err(std::io::Error::other("remote entry stream failed")),
      Err(_) => {
        return Err(std::io::Error::new(
          std::io::ErrorKind::TimedOut,
          "remote entry stream timed out",
        ));
      }
    };
    if read == 0 {
      return Err(std::io::Error::new(
        std::io::ErrorKind::UnexpectedEof,
        "remote entry stream ended before its declared length",
      ));
    }
    self.remaining = self.remaining.saturating_sub(read as u64);
    Ok(read)
  }
}

enum RequestFailure {
  Absent,
  Precondition,
  Store(RemoteStoreError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestKind {
  Marker,
  CacheGet,
  ConditionalPut,
}

/// One lazy direct S3 client and its private executor.
pub(super) struct S3Backend {
  runtime: Arc<tokio::runtime::Runtime>,
  client: aws_sdk_s3::Client,
  target: S3Target,
  counters: Arc<TransferCounters>,
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

pub(super) fn connect(selection: &RemoteCacheSelection) -> RemoteStoreResult<S3Backend> {
  let target = S3Target::from_selection(selection)?;
  if target.endpoint.is_none() {
    reject_official_endpoint_overrides()?;
  }
  let runtime = Arc::new(
    tokio::runtime::Builder::new_multi_thread()
      .worker_threads(1)
      .thread_name("cargo-rail-s3")
      .enable_all()
      .build()
      .map_err(|_| RemoteStoreError::configuration("remote cache runtime could not be created"))?,
  );
  let shared = runtime.block_on(
    aws_config::defaults(BehaviorVersion::latest())
      .region(Region::new(target.region.clone()))
      .use_fips(false)
      .use_dual_stack(false)
      .load(),
  );
  if target.endpoint.is_none() {
    reject_loaded_endpoint_overrides(&shared)?;
  }
  if !target.loopback_credentials {
    let credentials = shared
      .credentials_provider()
      .ok_or_else(|| RemoteStoreError::authentication("remote cache credentials are unavailable"))?;
    match block_on_timeout(&runtime, CREDENTIAL_TIMEOUT, credentials.as_ref().provide_credentials()) {
      Ok(Ok(_)) => {}
      Ok(Err(_)) => {
        return Err(RemoteStoreError::authentication(
          "remote cache credentials are unavailable",
        ));
      }
      Err(_) => {
        return Err(RemoteStoreError::unavailable(
          "remote cache credential resolution timed out",
        ));
      }
    }
  }
  let timeout = TimeoutConfig::builder()
    .connect_timeout(CONNECT_TIMEOUT)
    .read_timeout(READ_TIMEOUT)
    .operation_attempt_timeout(OPERATION_TIMEOUT)
    .operation_timeout(OPERATION_TIMEOUT)
    .build();
  let mut builder = aws_sdk_s3::config::Builder::from(&shared);
  if target.loopback_credentials {
    builder.set_credentials_provider(Some(aws_sdk_s3::config::SharedCredentialsProvider::new(
      Credentials::new(
        "cargo-rail-loopback",
        "cargo-rail-loopback",
        None,
        None,
        "cargo-rail-loopback",
      ),
    )));
  }
  builder
    .set_region(Some(Region::new(target.region.clone())))
    .set_endpoint_url(target.endpoint.clone())
    .set_use_fips(Some(false))
    .set_use_dual_stack(Some(false))
    .set_force_path_style(Some(target.force_path_style))
    .set_accelerate(Some(false))
    .set_use_arn_region(Some(false))
    .set_disable_multi_region_access_points(Some(true));
  builder.set_disable_s3_express_session_auth(Some(true));
  builder
    .set_timeout_config(Some(timeout))
    .set_retry_config(Some(RetryConfig::standard().with_max_attempts(3)));
  let counters = Arc::new(TransferCounters::default());
  builder.push_interceptor(aws_sdk_s3::config::SharedInterceptor::new(RequestMetricsInterceptor {
    counters: Arc::clone(&counters),
  }));
  let client = aws_sdk_s3::Client::from_conf(builder.build());
  Ok(S3Backend {
    runtime,
    client,
    target,
    counters,
  })
}

fn reject_official_endpoint_overrides() -> RemoteStoreResult<()> {
  for name in [
    "AWS_ENDPOINT_URL",
    "AWS_ENDPOINT_URL_S3",
    "AWS_ENDPOINT_URL_STS",
    "AWS_ENDPOINT_URL_SSO",
    "AWS_ENDPOINT_URL_SSO_OIDC",
  ] {
    if std::env::var_os(name).is_some_and(|value| !value.is_empty()) {
      return Err(RemoteStoreError::configuration(
        "official AWS cache credentials require official service endpoints",
      ));
    }
  }
  Ok(())
}

fn reject_loaded_endpoint_overrides(shared: &aws_config::SdkConfig) -> RemoteStoreResult<()> {
  if shared.endpoint_url().is_some() {
    return Err(RemoteStoreError::configuration(
      "official AWS cache credentials require official service endpoints",
    ));
  }
  if let Some(configuration) = shared.service_config() {
    for service_id in ["S3", "STS", "SSO", "SSO OIDC"] {
      let key = ServiceConfigKey::builder()
        .service_id(service_id)
        .env("AWS_ENDPOINT_URL")
        .profile("endpoint_url")
        .build()
        .map_err(|_| RemoteStoreError::configuration("remote credential endpoint policy is invalid"))?;
      if configuration.load_config(key).is_some() {
        return Err(RemoteStoreError::configuration(
          "official AWS cache credentials require official service endpoints",
        ));
      }
    }
  }
  Ok(())
}

impl S3Backend {
  pub(super) fn metrics(&self) -> TransferMetrics {
    self.counters.snapshot()
  }

  pub(super) fn take_metrics(&self) -> TransferMetrics {
    self.counters.take()
  }

  pub(super) fn get_marker(&self, key: &str) -> RemoteStoreResult<Option<Vec<u8>>> {
    self.runtime.block_on(get_marker(
      self.client.clone(),
      self.target.clone(),
      key.to_string(),
      Arc::clone(&self.counters),
    ))
  }

  pub(super) fn get_entry(&self, key: &str, base_action_key: &str) -> RemoteStoreResult<Option<StoredEntry>> {
    self.runtime.block_on(get_entry_request(
      self.client.clone(),
      self.target.clone(),
      key.to_string(),
      base_action_key.to_string(),
      Arc::clone(&self.counters),
      Arc::clone(&self.runtime),
    ))
  }

  pub(super) fn put_bytes(&self, key: &str, body: &[u8], condition: PutCondition) -> RemoteStoreResult<PutOutcome> {
    self.put_body(key, ByteStream::from(body.to_vec()), condition)
  }

  pub(super) fn put_file(
    &self,
    key: &str,
    mut body: File,
    bytes: u64,
    condition: PutCondition,
  ) -> RemoteStoreResult<PutOutcome> {
    if bytes > super::object::MAX_ENTRY_BYTES || body.metadata().map_err(io_unavailable)?.len() != bytes {
      return Err(RemoteStoreError::integrity(
        "remote entry body exceeds its byte authority",
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
    let body = body.map_err(|_| RemoteStoreError::unavailable("remote entry stream could not be opened"))?;
    self.put_body(key, body, condition)
  }

  fn put_body(&self, key: &str, body: ByteStream, condition: PutCondition) -> RemoteStoreResult<PutOutcome> {
    let mut request = self
      .client
      .put_object()
      .bucket(&self.target.bucket)
      .key(key)
      .set_expected_bucket_owner(self.target.expected_owner.clone())
      .body(body);
    request = match condition {
      PutCondition::Absent => request.if_none_match("*"),
      PutCondition::Match(etag) => request.if_match(etag),
    };
    match self.runtime.block_on(request.send()) {
      Ok(output) => {
        super::object::parse_etag(output.e_tag())?;
        Ok(PutOutcome::Written)
      }
      Err(error) => match classify_put_error(&error) {
        RequestFailure::Precondition => Ok(PutOutcome::PreconditionFailed),
        RequestFailure::Absent => Err(RemoteStoreError::integrity(
          "remote write returned an impossible absence",
        )),
        RequestFailure::Store(error) => Err(error),
      },
    }
  }
}

async fn get_marker(
  client: aws_sdk_s3::Client,
  target: S3Target,
  key: String,
  counters: Arc<TransferCounters>,
) -> RemoteStoreResult<Option<Vec<u8>>> {
  let result = client
    .get_object()
    .bucket(&target.bucket)
    .key(key)
    .set_expected_bucket_owner(target.expected_owner.clone())
    .send()
    .await;
  let mut output = match result {
    Ok(output) => output,
    Err(error) => {
      return match classify_get_error(&error, RequestKind::Marker) {
        RequestFailure::Absent => Ok(None),
        RequestFailure::Precondition => Err(RemoteStoreError::integrity("marker read returned a precondition")),
        RequestFailure::Store(error) => Err(error),
      };
    }
  };
  let declared = super::object::exact_length(
    output.content_length(),
    super::object::PROTOCOL_MARKER.len() as u64,
    "protocol marker",
  )?;
  let mut body = Vec::with_capacity(declared as usize);
  while let Some(chunk) = output.body.next().await {
    let chunk = chunk.map_err(|_| RemoteStoreError::unavailable("remote protocol marker stream failed"))?;
    if body.len().saturating_add(chunk.len()) > super::object::PROTOCOL_MARKER.len() {
      return Err(RemoteStoreError::integrity("remote protocol marker is oversized"));
    }
    counters
      .payload_bytes_read
      .fetch_add(chunk.len() as u64, Ordering::Relaxed);
    body.extend_from_slice(&chunk);
  }
  if body.len() as u64 != declared {
    return Err(RemoteStoreError::integrity(
      "remote protocol marker length changed while reading",
    ));
  }
  Ok(Some(body))
}

async fn get_entry_request(
  client: aws_sdk_s3::Client,
  target: S3Target,
  key: String,
  base_action_key: String,
  counters: Arc<TransferCounters>,
  runtime: Arc<tokio::runtime::Runtime>,
) -> RemoteStoreResult<Option<StoredEntry>> {
  let result = client
    .get_object()
    .bucket(&target.bucket)
    .key(key)
    .set_expected_bucket_owner(target.expected_owner.clone())
    .send()
    .await;
  let output = match result {
    Ok(output) => output,
    Err(error) => {
      return match classify_get_error(&error, RequestKind::CacheGet) {
        RequestFailure::Absent => Ok(None),
        RequestFailure::Precondition => Err(RemoteStoreError::integrity("cache read returned a precondition")),
        RequestFailure::Store(error) => Err(error),
      };
    }
  };
  let etag = super::object::parse_etag(output.e_tag())?;
  let declared = super::object::exact_length(output.content_length(), super::object::MAX_ENTRY_BYTES, "entry")?;
  counters.payload_bytes_read.fetch_add(declared, Ordering::Relaxed);
  let deadline = Instant::now()
    .checked_add(STREAM_TIMEOUT)
    .ok_or_else(|| RemoteStoreError::unavailable("remote entry stream deadline overflowed"))?;
  let mut stream: AsyncEntryStream = Box::pin(output.body.into_async_read());
  let (record, header_end) =
    tokio::time::timeout(STREAM_TIMEOUT, decode_entry_header_async(&mut stream, &base_action_key))
      .await
      .map_err(|_| RemoteStoreError::unavailable("remote entry stream timed out"))??;
  match &record.state {
    EntryState::Conflict { .. } => {
      if header_end != declared {
        return Err(RemoteStoreError::integrity("remote conflict entry contains a payload"));
      }
      Ok(Some(StoredEntry {
        record,
        body: None,
        etag,
      }))
    }
    EntryState::Unique {
      identity,
      payload_length,
    } => {
      if header_end.checked_add(*payload_length) != Some(declared) {
        return Err(RemoteStoreError::integrity(
          "remote unique entry length is inconsistent",
        ));
      }
      let pack = EntryBody::new(
        Box::new(RemotePayload {
          runtime,
          stream,
          remaining: *payload_length,
          deadline,
        }),
        *payload_length,
        identity.pack_length,
      );
      Ok(Some(StoredEntry {
        record,
        body: Some(pack),
        etag,
      }))
    }
  }
}

async fn decode_entry_header_async(
  reader: &mut AsyncEntryStream,
  base_action_key: &str,
) -> RemoteStoreResult<(EntryRecord, u64)> {
  let mut prelude = [0_u8; ENTRY_PRELUDE_BYTES as usize];
  reader
    .read_exact(&mut prelude)
    .await
    .map_err(|_| RemoteStoreError::integrity("remote entry prelude is truncated"))?;
  let header_length = super::object::decode_entry_prelude(&prelude)?;
  let mut header = vec![0_u8; header_length];
  reader
    .read_exact(&mut header)
    .await
    .map_err(|_| RemoteStoreError::integrity("remote entry header is truncated"))?;
  let record = super::object::decode_entry_record(&header, base_action_key)?;
  let header_end = ENTRY_PRELUDE_BYTES
    .checked_add(header_length as u64)
    .ok_or_else(|| RemoteStoreError::integrity("remote entry header length overflowed"))?;
  Ok((record, header_end))
}

fn classify_get_error(error: &SdkError<GetObjectError>, kind: RequestKind) -> RequestFailure {
  let typed_absence = matches!(error.as_service_error(), Some(GetObjectError::NoSuchKey(_)));
  classify_sdk_error(error, kind, typed_absence)
}

fn classify_put_error(error: &SdkError<PutObjectError>) -> RequestFailure {
  classify_sdk_error(error, RequestKind::ConditionalPut, false)
}

fn classify_sdk_error<E>(error: &SdkError<E>, kind: RequestKind, typed_absence: bool) -> RequestFailure {
  if typed_absence {
    return RequestFailure::Absent;
  }
  if let SdkError::ServiceError(context) = error {
    return match context.raw().status().as_u16() {
      404 => RequestFailure::Absent,
      403 if matches!(kind, RequestKind::Marker | RequestKind::CacheGet) => RequestFailure::Absent,
      409 | 412 if kind == RequestKind::ConditionalPut => RequestFailure::Precondition,
      401 | 403 => RequestFailure::Store(RemoteStoreError::authentication(
        "remote cache authentication was rejected",
      )),
      408 | 425 | 429 | 500..=599 => {
        RequestFailure::Store(RemoteStoreError::unavailable("remote cache service is unavailable"))
      }
      _ => RequestFailure::Store(RemoteStoreError::configuration(
        "remote cache request was rejected by its pinned service",
      )),
    };
  }
  match error {
    SdkError::ConstructionFailure(_) => RequestFailure::Store(RemoteStoreError::authentication(
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

fn io_unavailable(_error: std::io::Error) -> RemoteStoreError {
  RemoteStoreError::unavailable("remote cache local streaming failed")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn remote_compressed_body_drains_exactly_and_rejects_truncation() {
    use tokio::io::AsyncWriteExt as _;

    fn body(payload: &[u8], declared: u64) -> EntryBody {
      let runtime = Arc::new(
        tokio::runtime::Builder::new_current_thread()
          .enable_time()
          .build()
          .expect("runtime"),
      );
      let (mut sender, receiver) = tokio::io::duplex(payload.len().max(1));
      runtime.block_on(async {
        sender.write_all(payload).await.expect("write payload");
        sender.shutdown().await.expect("finish payload");
      });
      EntryBody::new(
        Box::new(RemotePayload {
          runtime,
          stream: Box::pin(receiver),
          remaining: declared,
          deadline: Instant::now() + STREAM_TIMEOUT,
        }),
        declared,
        1,
      )
    }

    let payload = (0..(STREAM_BUFFER_BYTES * 3 + 17))
      .map(|index| (index % 251) as u8)
      .collect::<Vec<_>>();
    let mut exact = body(&payload, payload.len() as u64);
    let mut retained = Vec::new();
    assert_eq!(
      exact.copy_compressed_to(&mut retained).expect("drain payload"),
      payload.len() as u64
    );
    assert_eq!(retained, payload);

    let mut truncated = body(&payload, payload.len() as u64 + 1);
    let error = truncated
      .copy_compressed_to(&mut std::io::sink())
      .expect_err("truncated payload must fail");
    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
  }

  #[test]
  fn request_statuses_preserve_conditional_and_fallback_classes() {
    assert!(matches!(
      classify_sdk_status_for_test(RequestKind::ConditionalPut, 412),
      RequestFailure::Precondition
    ));
    assert!(matches!(
      classify_sdk_status_for_test(RequestKind::CacheGet, 403),
      RequestFailure::Absent
    ));
    assert!(matches!(
      classify_sdk_status_for_test(RequestKind::Marker, 403),
      RequestFailure::Absent
    ));
  }

  fn classify_sdk_status_for_test(kind: RequestKind, status: u16) -> RequestFailure {
    match status {
      404 => RequestFailure::Absent,
      403 if matches!(kind, RequestKind::Marker | RequestKind::CacheGet) => RequestFailure::Absent,
      409 | 412 if kind == RequestKind::ConditionalPut => RequestFailure::Precondition,
      _ => RequestFailure::Store(RemoteStoreError::unavailable("test status")),
    }
  }
}
