//! Direct bounded Azure Blob transport for the native compiler-result protocol.

use std::fs::File;
use std::io::{Read as _, Seek as _};
use std::num::NonZero;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use azure_core::credentials::{AccessToken, TokenCredential, TokenRequestOptions};
use azure_core::error::ErrorKind;
use azure_core::http::policies::{Policy, PolicyResult};
use azure_core::http::{ClientOptions, Context, Etag, ExponentialRetryOptions, Method, Request, RetryOptions, Url};
use azure_identity::{DeveloperToolsCredential, ManagedIdentityCredential, WorkloadIdentityCredential};
use azure_storage_blob::models::{BlobClientDownloadOptions, BlobClientUploadOptions};
use azure_storage_blob::stream::tokio::FileStream;
use azure_storage_blob::{BlobClient, BlobClientOptions};
use futures_util::TryStreamExt as _;

use super::object::{
  ENTRY_PRELUDE_BYTES, EntryBody, EntryState, MAX_ENTRY_BYTES, PutCondition, PutOutcome, STREAM_BUFFER_BYTES,
  StoredEntry, TransferMetrics,
};
use super::{RemoteCacheSelection, RemoteStoreError, RemoteStoreResult};

const OPERATION_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const SERVICE_TIMEOUT_SECONDS: i32 = 15 * 60;
const DOWNLOAD_PARTITION_BYTES: usize = 8 * 1024 * 1024;
const CREDENTIAL_UNSELECTED: usize = usize::MAX;

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
struct RequestMetricsPolicy {
  counters: Arc<TransferCounters>,
}

#[async_trait]
impl Policy for RequestMetricsPolicy {
  async fn send(&self, context: &Context, request: &mut Request, next: &[Arc<dyn Policy>]) -> PolicyResult {
    self.counters.request_attempts.fetch_add(1, Ordering::Relaxed);
    if request.method() == Method::Put
      && let Some(bytes) = request.body().len()
    {
      self.counters.payload_bytes_written.fetch_add(bytes, Ordering::Relaxed);
    }
    let next = next
      .split_first()
      .ok_or_else(|| azure_core::Error::with_message(ErrorKind::Other, "Azure pipeline has no transport policy"))?;
    next.0.send(context, request, next.1).await
  }
}

struct CredentialChain {
  sources: Vec<Arc<dyn TokenCredential>>,
  selected: AtomicUsize,
}

impl std::fmt::Debug for CredentialChain {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    formatter
      .debug_struct("CredentialChain")
      .field("sources", &self.sources.len())
      .finish_non_exhaustive()
  }
}

#[async_trait]
impl TokenCredential for CredentialChain {
  async fn get_token(
    &self,
    scopes: &[&str],
    options: Option<TokenRequestOptions<'_>>,
  ) -> azure_core::Result<AccessToken> {
    let selected = self.selected.load(Ordering::Acquire);
    if selected != CREDENTIAL_UNSELECTED {
      return self
        .sources
        .get(selected)
        .ok_or_else(|| azure_core::Error::with_message(ErrorKind::Credential, "Azure credential selection is invalid"))?
        .get_token(scopes, options)
        .await;
    }
    for (index, source) in self.sources.iter().enumerate() {
      if let Ok(token) = source.get_token(scopes, options.clone()).await {
        self.selected.store(index, Ordering::Release);
        return Ok(token);
      }
    }
    Err(azure_core::Error::with_message(
      ErrorKind::Credential,
      "no supported Azure credential supplied a token",
    ))
  }
}

struct AzurePayload {
  runtime: Arc<tokio::runtime::Runtime>,
  body: azure_core::http::AsyncResponseBody,
  buffered: Option<azure_core::Bytes>,
  buffered_offset: usize,
  remaining: u64,
  deadline: Instant,
  counters: Arc<TransferCounters>,
}

impl std::io::Read for AzurePayload {
  fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
    if output.is_empty() || self.remaining == 0 {
      return Ok(0);
    }
    loop {
      if let Some(buffered) = &self.buffered {
        let available = &buffered[self.buffered_offset..];
        let maximum = usize::try_from(self.remaining.min(output.len() as u64)).unwrap_or(output.len());
        let copied = maximum.min(available.len());
        output[..copied].copy_from_slice(&available[..copied]);
        self.buffered_offset += copied;
        self.remaining = self.remaining.saturating_sub(copied as u64);
        if self.buffered_offset == buffered.len() {
          self.buffered = None;
          self.buffered_offset = 0;
        }
        return Ok(copied);
      }
      let timeout = self
        .deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::TimedOut, "remote entry stream timed out"))?;
      let next = self
        .runtime
        .block_on(async { tokio::time::timeout(timeout, self.body.try_next()).await });
      let chunk = match next {
        Ok(Ok(Some(chunk))) if !chunk.is_empty() => chunk,
        Ok(Ok(Some(_))) => continue,
        Ok(Ok(None)) => {
          return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "remote entry stream ended before its declared length",
          ));
        }
        Ok(Err(_)) => return Err(std::io::Error::other("remote entry stream failed")),
        Err(_) => {
          return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "remote entry stream timed out",
          ));
        }
      };
      if chunk.len() as u64 > self.remaining {
        return Err(std::io::Error::new(
          std::io::ErrorKind::InvalidData,
          "remote entry stream exceeds its declared length",
        ));
      }
      self
        .counters
        .payload_bytes_read
        .fetch_add(chunk.len() as u64, Ordering::Relaxed);
      self.buffered = Some(chunk);
    }
  }
}

struct DownloadedBlob {
  body: azure_core::http::AsyncResponseBody,
  bytes: u64,
  etag: String,
}

/// One retained Azure Blob client configuration and executor.
pub(super) struct AzureBackend {
  runtime: Arc<tokio::runtime::Runtime>,
  endpoint: Url,
  container: String,
  credential: Arc<dyn TokenCredential>,
  options: BlobClientOptions,
  counters: Arc<TransferCounters>,
}

pub(super) fn connect(selection: &RemoteCacheSelection) -> RemoteStoreResult<AzureBackend> {
  let authority = &selection.authority;
  if !authority.is_azure_blob() {
    return Err(RemoteStoreError::configuration(
      "selected remote provider is not Azure Blob",
    ));
  }
  let runtime = Arc::new(
    tokio::runtime::Builder::new_multi_thread()
      .worker_threads(1)
      .thread_name("cargo-rail-azure")
      .enable_all()
      .build()
      .map_err(|_| RemoteStoreError::configuration("remote cache runtime could not be created"))?,
  );
  let credential = azure_credential()?;
  let counters = Arc::new(TransferCounters::default());
  let retry = RetryOptions::exponential(ExponentialRetryOptions {
    max_retries: 2,
    max_total_elapsed: azure_core::time::Duration::seconds(60),
    ..Default::default()
  });
  let transport = azure_core::http::Transport::new(azure_core::http::new_http_client(Some(
    azure_core::http::HttpClientOptions {
      automatic_decompression: false,
    },
  )));
  let options = BlobClientOptions {
    client_options: ClientOptions {
      per_try_policies: vec![Arc::new(RequestMetricsPolicy {
        counters: Arc::clone(&counters),
      })],
      retry,
      transport: Some(transport),
      ..Default::default()
    },
    ..Default::default()
  };
  let endpoint = Url::parse(authority.endpoint())
    .map_err(|_| RemoteStoreError::integrity("Azure Blob endpoint could not be constructed"))?;
  Ok(AzureBackend {
    runtime,
    endpoint,
    container: authority.bucket().to_string(),
    credential,
    options,
    counters,
  })
}

fn azure_credential() -> RemoteStoreResult<Arc<dyn TokenCredential>> {
  let mut sources = Vec::<Arc<dyn TokenCredential>>::new();
  if std::env::var_os("AZURE_FEDERATED_TOKEN_FILE").is_some_and(|value| !value.is_empty()) {
    sources.push(
      WorkloadIdentityCredential::new(None)
        .map_err(|_| RemoteStoreError::authentication("Azure workload identity is incomplete"))?,
    );
  }
  sources.push(
    DeveloperToolsCredential::new(None)
      .map_err(|_| RemoteStoreError::authentication("Azure developer credentials are unavailable"))?,
  );
  sources.push(
    ManagedIdentityCredential::new(None)
      .map_err(|_| RemoteStoreError::authentication("Azure managed identity is unavailable"))?,
  );
  Ok(Arc::new(CredentialChain {
    sources,
    selected: AtomicUsize::new(CREDENTIAL_UNSELECTED),
  }))
}

impl AzureBackend {
  pub(super) fn metrics(&self) -> TransferMetrics {
    self.counters.snapshot()
  }

  pub(super) fn take_metrics(&self) -> TransferMetrics {
    self.counters.take()
  }

  pub(super) fn get_marker(&self, key: &str) -> RemoteStoreResult<Option<Vec<u8>>> {
    let Some(download) = self.download(key, super::object::PROTOCOL_MARKER.len() as u64, RequestKind::Marker)? else {
      return Ok(None);
    };
    let mut payload = self.payload(download.body, download.bytes)?;
    let capacity = usize::try_from(download.bytes)
      .map_err(|_| RemoteStoreError::integrity("remote protocol marker length is invalid"))?;
    let mut bytes = Vec::with_capacity(capacity);
    payload
      .read_to_end(&mut bytes)
      .map_err(|_| RemoteStoreError::unavailable("remote protocol marker stream failed"))?;
    Ok(Some(bytes))
  }

  pub(super) fn get_entry(&self, key: &str, base_action_key: &str) -> RemoteStoreResult<Option<StoredEntry>> {
    let Some(download) = self.download(key, MAX_ENTRY_BYTES, RequestKind::CacheGet)? else {
      return Ok(None);
    };
    let declared = download.bytes;
    let mut payload = self.payload(download.body, declared)?;
    let mut prelude = [0_u8; ENTRY_PRELUDE_BYTES as usize];
    payload
      .read_exact(&mut prelude)
      .map_err(|_| RemoteStoreError::integrity("remote entry prelude is truncated"))?;
    let header_length = super::object::decode_entry_prelude(&prelude)?;
    let mut header = vec![0_u8; header_length];
    payload
      .read_exact(&mut header)
      .map_err(|_| RemoteStoreError::integrity("remote entry header is truncated"))?;
    let record = super::object::decode_entry_record(&header, base_action_key)?;
    let header_end = ENTRY_PRELUDE_BYTES
      .checked_add(header_length as u64)
      .ok_or_else(|| RemoteStoreError::integrity("remote entry header length overflowed"))?;
    match &record.state {
      EntryState::Conflict { .. } => {
        if header_end != declared {
          return Err(RemoteStoreError::integrity("remote conflict entry contains a payload"));
        }
        Ok(Some(StoredEntry {
          record,
          body: None,
          etag: download.etag,
        }))
      }
      EntryState::Unique {
        identity,
        payload_length,
      } => {
        if header_end.checked_add(*payload_length) != Some(declared) || payload.remaining != *payload_length {
          return Err(RemoteStoreError::integrity(
            "remote unique entry length is inconsistent",
          ));
        }
        let body = EntryBody::new(Box::new(payload), *payload_length, identity.pack_length);
        Ok(Some(StoredEntry {
          record,
          body: Some(body),
          etag: download.etag,
        }))
      }
    }
  }

  pub(super) fn put_bytes(&self, key: &str, body: &[u8], condition: PutCondition) -> RemoteStoreResult<PutOutcome> {
    let client = self.client(key)?;
    let options = upload_options(condition)?;
    let result = self.runtime.block_on(async {
      tokio::time::timeout(
        OPERATION_TIMEOUT,
        client.upload(azure_core::http::RequestContent::from(body.to_vec()), Some(options)),
      )
      .await
    });
    classify_upload_result(result)
  }

  pub(super) fn put_file(
    &self,
    key: &str,
    mut body: File,
    bytes: u64,
    condition: PutCondition,
  ) -> RemoteStoreResult<PutOutcome> {
    if bytes > MAX_ENTRY_BYTES || body.metadata().map_err(io_unavailable)?.len() != bytes {
      return Err(RemoteStoreError::integrity(
        "remote entry body exceeds its byte authority",
      ));
    }
    body.rewind().map_err(io_unavailable)?;
    let client = self.client(key)?;
    let options = upload_options(condition)?;
    let result = self.runtime.block_on(async {
      let stream = FileStream::builder(tokio::fs::File::from_std(body))
        .with_buffer_size(STREAM_BUFFER_BYTES)
        .build()
        .await;
      let stream = match stream {
        Ok(stream) => stream,
        Err(error) => return Ok(Err(error)),
      };
      let body: azure_core::http::Body = stream.into();
      tokio::time::timeout(OPERATION_TIMEOUT, client.upload(body.into(), Some(options))).await
    });
    classify_upload_result(result)
  }

  fn payload(&self, body: azure_core::http::AsyncResponseBody, bytes: u64) -> RemoteStoreResult<AzurePayload> {
    Ok(AzurePayload {
      runtime: Arc::clone(&self.runtime),
      body,
      buffered: None,
      buffered_offset: 0,
      remaining: bytes,
      deadline: Instant::now()
        .checked_add(OPERATION_TIMEOUT)
        .ok_or_else(|| RemoteStoreError::unavailable("remote entry stream deadline overflowed"))?,
      counters: Arc::clone(&self.counters),
    })
  }

  fn download(&self, key: &str, maximum: u64, kind: RequestKind) -> RemoteStoreResult<Option<DownloadedBlob>> {
    let client = self.client(key)?;
    let partition_size = NonZero::new(DOWNLOAD_PARTITION_BYTES)
      .ok_or_else(|| RemoteStoreError::configuration("Azure download partition is invalid"))?;
    let parallel =
      NonZero::new(1).ok_or_else(|| RemoteStoreError::configuration("Azure download concurrency is invalid"))?;
    let options = BlobClientDownloadOptions {
      parallel: Some(parallel),
      partition_size: Some(partition_size),
      timeout: Some(SERVICE_TIMEOUT_SECONDS),
      ..Default::default()
    };
    let result = self
      .runtime
      .block_on(async { tokio::time::timeout(OPERATION_TIMEOUT, client.download(Some(options))).await });
    let response = match result {
      Ok(Ok(response)) => response,
      Ok(Err(error)) => {
        return match classify_error(&error, kind) {
          RequestFailure::Absent => Ok(None),
          RequestFailure::Precondition => Err(RemoteStoreError::integrity("remote read returned a precondition")),
          RequestFailure::Store(error) => Err(error),
        };
      }
      Err(_) => return Err(RemoteStoreError::unavailable("remote cache request timed out")),
    };
    let bytes = response_length(&response)?;
    if bytes > maximum {
      return Err(RemoteStoreError::integrity("remote object exceeds its byte bound"));
    }
    let etag = super::object::parse_etag(response.properties.etag.as_ref().map(AsRef::as_ref))?;
    Ok(Some(DownloadedBlob {
      body: response.body,
      bytes,
      etag,
    }))
  }

  fn client(&self, key: &str) -> RemoteStoreResult<BlobClient> {
    let mut url = self.endpoint.clone();
    let mut segments = url
      .path_segments_mut()
      .map_err(|_| RemoteStoreError::integrity("Azure Blob endpoint cannot accept object paths"))?;
    segments.push(&self.container);
    for segment in key.split('/') {
      segments.push(segment);
    }
    drop(segments);
    BlobClient::new(url, Some(Arc::clone(&self.credential)), Some(self.options.clone()))
      .map_err(|_| RemoteStoreError::configuration("Azure Blob client could not be created"))
  }
}

fn upload_options(condition: PutCondition) -> RemoteStoreResult<BlobClientUploadOptions<'static>> {
  let parallel =
    NonZero::new(1).ok_or_else(|| RemoteStoreError::configuration("Azure upload concurrency is invalid"))?;
  let partition_size = NonZero::new(MAX_ENTRY_BYTES)
    .ok_or_else(|| RemoteStoreError::configuration("Azure upload partition is invalid"))?;
  let mut options = BlobClientUploadOptions {
    parallel: Some(parallel),
    partition_size: Some(partition_size),
    per_request_timeout: Some(SERVICE_TIMEOUT_SECONDS),
    ..Default::default()
  };
  match condition {
    PutCondition::Absent => options.if_none_match = Some(Etag::from("*")),
    PutCondition::Match(etag) => options.if_match = Some(Etag::from(etag)),
  }
  Ok(options)
}

fn classify_upload_result(
  result: Result<azure_core::Result<azure_storage_blob::models::BlobClientUploadResult>, tokio::time::error::Elapsed>,
) -> RemoteStoreResult<PutOutcome> {
  match result {
    Ok(Ok(output)) => {
      super::object::parse_etag(output.etag.as_ref().map(AsRef::as_ref))?;
      Ok(PutOutcome::Written)
    }
    Ok(Err(error)) => match classify_error(&error, RequestKind::ConditionalPut) {
      RequestFailure::Precondition => Ok(PutOutcome::PreconditionFailed),
      RequestFailure::Absent => Err(RemoteStoreError::integrity(
        "remote write returned an impossible absence",
      )),
      RequestFailure::Store(error) => Err(error),
    },
    Err(_) => Err(RemoteStoreError::unavailable("remote cache request timed out")),
  }
}

fn response_length(response: &azure_storage_blob::models::BlobClientDownloadResult) -> RemoteStoreResult<u64> {
  let content_range = response
    .headers
    .get_optional_str(&azure_core::http::headers::HeaderName::from_static("content-range"));
  if let Some(value) = content_range {
    return value
      .rsplit_once('/')
      .and_then(|(_, total)| total.parse::<u64>().ok())
      .ok_or_else(|| RemoteStoreError::integrity("Azure Blob content range is malformed"));
  }
  response
    .properties
    .content_length
    .ok_or_else(|| RemoteStoreError::integrity("remote object has no exact length"))
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

fn classify_error(error: &azure_core::Error, kind: RequestKind) -> RequestFailure {
  if let Some(status) = error.http_status() {
    return match u16::from(status) {
      404 if matches!(kind, RequestKind::Marker | RequestKind::CacheGet) => RequestFailure::Absent,
      409 | 412 if kind == RequestKind::ConditionalPut => RequestFailure::Precondition,
      401 | 403 => RequestFailure::Store(RemoteStoreError::authentication(
        "remote cache authentication was rejected",
      )),
      408 | 425 | 429 | 500..=599 => {
        RequestFailure::Store(RemoteStoreError::unavailable("remote cache service is unavailable"))
      }
      _ => RequestFailure::Store(RemoteStoreError::configuration(
        "remote cache request was rejected by Azure Blob",
      )),
    };
  }
  match error.kind() {
    ErrorKind::Credential => RequestFailure::Store(RemoteStoreError::authentication(
      "remote cache credentials are unavailable",
    )),
    ErrorKind::DataConversion => RequestFailure::Store(RemoteStoreError::integrity(
      "remote cache service returned an invalid response",
    )),
    ErrorKind::Connection | ErrorKind::Io | ErrorKind::Other => {
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
  fn response_statuses_preserve_absence_authentication_and_preconditions() {
    assert!(matches!(
      classify_status_for_test(RequestKind::CacheGet, 404),
      RequestFailure::Absent
    ));
    assert!(matches!(
      classify_status_for_test(RequestKind::ConditionalPut, 412),
      RequestFailure::Precondition
    ));
    assert!(matches!(
      classify_status_for_test(RequestKind::CacheGet, 403),
      RequestFailure::Store(_)
    ));
  }

  #[test]
  fn content_range_requires_an_exact_decimal_total() {
    assert_eq!(parse_content_range_for_test("bytes 0-7/25"), Some(25));
    assert_eq!(parse_content_range_for_test("bytes 0-7/*"), None);
    assert_eq!(parse_content_range_for_test("invalid"), None);
  }

  fn classify_status_for_test(kind: RequestKind, status: u16) -> RequestFailure {
    match status {
      404 if matches!(kind, RequestKind::Marker | RequestKind::CacheGet) => RequestFailure::Absent,
      409 | 412 if kind == RequestKind::ConditionalPut => RequestFailure::Precondition,
      _ => RequestFailure::Store(RemoteStoreError::unavailable("test status")),
    }
  }

  fn parse_content_range_for_test(value: &str) -> Option<u64> {
    value.rsplit_once('/').and_then(|(_, total)| total.parse().ok())
  }
}
