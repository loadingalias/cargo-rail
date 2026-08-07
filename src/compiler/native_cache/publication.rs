//! Command-owned asynchronous publication of verified native compiler results.

use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::{
  Arc, Mutex,
  atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{
  CompilerCacheWrapperMetadata, CompilerCacheWrapperStatus, NativeCacheMetrics, NativeCacheWrapperPhase,
  NativeCacheWrapperTrace, NativeCacheWrapperTraceSnapshot, NativeCacheWrapperWork, NativeCompilerSession,
  NativeCompilerValidation, NativePublicationProof, NativeSessionAuthority, PreparedNativeOrigin, PreparedNativeResult,
  PreparedNativeStaging, RawCompilerInvocation, cold_input_bytes, write_cache_event_at,
};
use crate::error::{RailError, RailResult};
use crate::hermetic::OutputManifest;
use crate::hermetic::cas::{LocalCas, StagedNativeResult, StoreStats};

const REQUEST_VERSION: u32 = 4;
const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;
// Cross-process wrappers publish queue entries with an atomic rename. A 1 ms
// scan cadence spent roughly a quarter of a CPU second reopening an empty
// directory during one seven-second cold build. Eight milliseconds keeps the
// final drain responsive while removing almost all idle filesystem polling.
const POLL_INTERVAL: Duration = Duration::from_millis(8);
const LOCAL_COMMIT_COHORT_RESULTS: usize = 4;

/// Private filesystem capability inherited by compiler wrappers for one Cargo command.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WrapperContext {
  version: u32,
  staging_root: String,
  discovery_only: bool,
}

/// Parent-owned publication worker and the staging lease that excludes GC.
pub(super) struct Coordinator {
  context: WrapperContext,
  stop: Arc<AtomicBool>,
  worker: Mutex<Option<PublicationWorker>>,
  metrics: Arc<CoordinatorMetrics>,
}

struct PublicationWorker {
  handle: JoinHandle<()>,
}

#[derive(Default)]
struct CoordinatorMetrics {
  rejected: AtomicU64,
  setup_bytes_hashed: AtomicU64,
  session_failed: AtomicBool,
  selector_diverged: AtomicBool,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct CoordinatorReport {
  pub(super) rejected: u64,
  pub(super) setup_bytes_hashed: u64,
  pub(super) session_failed: bool,
  pub(super) selector_diverged: bool,
}

/// State owned by the one command-scoped publication thread.
///
/// Keeping this state together makes the thread's authority explicit and
/// prevents its invariant inputs from being threaded independently through
/// every queue operation.
struct PublicationServer {
  cas: LocalCas,
  discovery_session: NativeCompilerSession,
  admission_session: Option<NativeCompilerSession>,
  discovery_only: bool,
  source_root: PathBuf,
  observations: PathBuf,
  staging_root: PathBuf,
  stop: Arc<AtomicBool>,
  metrics: Arc<CoordinatorMetrics>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicationRequest {
  version: u32,
  discovery_only: bool,
  staging_name: String,
  manifest: OutputManifest,
  validation: NativeCompilerValidation,
  observed_outputs: Vec<super::FileObservation>,
  proof: NativePublicationProof,
  reason: String,
  remote_publishable: bool,
  cache_bytes_read: u64,
  wrapper_trace: Option<NativeCacheWrapperTraceSnapshot>,
}

struct PendingDiscoveryAdmission {
  staged: StagedNativeResult,
  report: AdmissionReport,
  proof: NativePublicationProof,
}

struct AdmissionReport {
  observation: RawCompilerInvocation,
  reason: String,
  remote_publishable: bool,
  cache_bytes_read: u64,
  wrapper_trace: Option<NativeCacheWrapperTraceSnapshot>,
  environment_selector: (String, Vec<String>),
}

enum AdmissionOutcome<'a> {
  Stored(&'a NativeCompilerValidation, StoreStats),
  RevalidationFailed,
  StoreFailed,
}

impl Coordinator {
  pub(super) fn prepare(
    source_root: &Path,
    observation_directory: &Path,
    session_path: &Path,
    deferred_session: Option<JoinHandle<RailResult<(NativeCompilerSession, u64)>>>,
    discovery_only: bool,
  ) -> RailResult<Self> {
    let source_root = crate::utils::canonicalize_existing(source_root)?;
    let session = NativeCompilerSession::load(session_path, &source_root)?;
    if discovery_only != (session.authority == NativeSessionAuthority::Discovery)
      || discovery_only != deferred_session.is_some()
    {
      return Err(RailError::message(
        "native publication session authority is inconsistent",
      ));
    }
    let cas = LocalCas::open_initialized()?;
    let (staging, active) = cas.native_publication_staging()?;
    for name in ["incoming", "queue"] {
      fs::create_dir(staging.path().join(name))?;
    }
    let staging_root = staging
      .path()
      .to_str()
      .ok_or_else(|| RailError::message("native publication staging path is not UTF-8"))?
      .to_string();
    let context = WrapperContext {
      version: 2,
      staging_root,
      discovery_only,
    };
    let stop = Arc::new(AtomicBool::new(false));
    let metrics = Arc::new(CoordinatorMetrics::default());
    let worker_stop = Arc::clone(&stop);
    let worker_metrics = Arc::clone(&metrics);
    let worker_root = staging.path().to_path_buf();
    let worker_observations = observation_directory.to_path_buf();
    let handle = std::thread::Builder::new()
      .name("cargo-rail-local-publication".to_string())
      .spawn(move || {
        // The worker owns the staging capability and active-file lease until
        // every queued publication has reached a terminal outcome.
        let _staging = staging;
        let _active = active;
        let admission_session = match deferred_session {
          Some(worker) => worker.join().ok().and_then(Result::ok).map(|(session, bytes)| {
            worker_metrics.setup_bytes_hashed.store(bytes, Ordering::Relaxed);
            session
          }),
          None => Some(session.clone()),
        };
        if admission_session.is_none() {
          worker_metrics.session_failed.store(true, Ordering::Relaxed);
        }
        PublicationServer {
          cas,
          discovery_session: session,
          admission_session,
          discovery_only,
          source_root,
          observations: worker_observations,
          staging_root: worker_root,
          stop: worker_stop,
          metrics: worker_metrics,
        }
        .serve();
      })?;
    Ok(Self {
      context,
      stop,
      worker: Mutex::new(Some(PublicationWorker { handle })),
      metrics,
    })
  }

  pub(super) fn context(&self) -> WrapperContext {
    self.context.clone()
  }

  pub(super) fn drain(&self) -> CoordinatorReport {
    self.stop.store(true, Ordering::Release);
    if let Ok(mut worker) = self.worker.lock()
      && let Some(worker) = worker.take()
    {
      match finish_worker(worker) {
        WorkerDrain::Completed => {}
        WorkerDrain::Panicked => self.metrics.session_failed.store(true, Ordering::Relaxed),
      }
    }
    CoordinatorReport {
      rejected: self.metrics.rejected.load(Ordering::Relaxed),
      setup_bytes_hashed: self.metrics.setup_bytes_hashed.load(Ordering::Relaxed),
      session_failed: self.metrics.session_failed.load(Ordering::Relaxed),
      selector_diverged: self.metrics.selector_diverged.load(Ordering::Relaxed),
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerDrain {
  Completed,
  Panicked,
}

fn finish_worker(worker: PublicationWorker) -> WorkerDrain {
  if worker.handle.join().is_ok() {
    WorkerDrain::Completed
  } else {
    WorkerDrain::Panicked
  }
}

impl Drop for Coordinator {
  fn drop(&mut self) {
    let _ = self.drain();
  }
}

impl WrapperContext {
  fn root(&self) -> RailResult<PathBuf> {
    if self.version != 2 || self.staging_root.is_empty() || self.staging_root.as_bytes().contains(&0) {
      return Err(RailError::message("native publication context is invalid"));
    }
    let root = PathBuf::from(&self.staging_root);
    if !root.is_absolute() {
      return Err(RailError::message("native publication staging is not absolute"));
    }
    Ok(root)
  }
}

pub(super) fn staging(context: &WrapperContext, cas: &LocalCas) -> RailResult<super::pack::NativeResultStaging> {
  cas.native_command_result_staging(&context.root()?)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn enqueue(
  context: &WrapperContext,
  prepared: PreparedNativeResult,
  observed_outputs: Vec<super::FileObservation>,
  proof: NativePublicationProof,
  reason: String,
  remote_publishable: bool,
  cache_bytes_read: u64,
  trace: &mut NativeCacheWrapperTrace,
) -> RailResult<()> {
  let handoff_phase = trace.start(NativeCacheWrapperPhase::ColdResultHandoff);
  let root = context.root()?;
  let PreparedNativeResult {
    staging,
    staging_lock,
    manifest,
    validation,
    origin,
    move_preverified_blobs,
  } = prepared;
  let PreparedNativeStaging::Temporary(staging) = staging else {
    return Err(RailError::message("native publication staging was already handed off"));
  };
  if staging_lock.is_some() || origin != PreparedNativeOrigin::Local || !move_preverified_blobs {
    return Err(RailError::message(
      "native publication handoff requires command-scoped local staging",
    ));
  }
  let incoming = root.join("incoming");
  if staging.path().parent() != Some(incoming.as_path()) {
    return Err(RailError::message(
      "native publication result is outside its command staging capability",
    ));
  }
  let staging_name = staging
    .path()
    .file_name()
    .and_then(OsStr::to_str)
    .filter(|name| valid_staging_name(name))
    .ok_or_else(|| RailError::message("native publication result has an invalid staging name"))?
    .to_string();
  let mut request = PublicationRequest {
    version: REQUEST_VERSION,
    discovery_only: context.discovery_only,
    staging_name: staging_name.clone(),
    manifest,
    validation,
    observed_outputs,
    proof,
    reason,
    remote_publishable,
    cache_bytes_read,
    wrapper_trace: None,
  };
  request.validate()?;
  let persisted = staging.keep();
  if persisted != incoming.join(&staging_name) {
    return Err(RailError::message(
      "native publication result path changed during handoff",
    ));
  }
  trace.finish(handoff_phase, NativeCacheWrapperWork::default());
  request.wrapper_trace = trace.snapshot();
  let bytes = serde_json::to_vec(&request)?;
  if bytes.len() > MAX_REQUEST_BYTES {
    return Err(RailError::message("native publication request exceeds its byte bound"));
  }
  publish_request(&root.join("queue"), &staging_name, &bytes)
}

impl PublicationRequest {
  fn validate(&self) -> RailResult<()> {
    if self.version != REQUEST_VERSION
      || !valid_staging_name(&self.staging_name)
      || self.reason.is_empty()
      || self.reason.len() > 4 * 1024
      || self.reason.chars().any(char::is_control)
    {
      return Err(RailError::message("native publication request is invalid"));
    }
    self.validation.validate_object()?;
    if self.observed_outputs.len() != self.validation.observation.emitted_outputs.len()
      || self
        .observed_outputs
        .iter()
        .zip(&self.validation.observation.emitted_outputs)
        .any(|(observed, canonical)| {
          observed.path != canonical.path
            || observed.executable != canonical.executable
            || observed.symlink_target != canonical.symlink_target
            || super::validate_file_observation(observed).is_err()
        })
    {
      return Err(RailError::message(
        "native publication observed outputs do not match the canonical output contract",
      ));
    }
    self.proof.validate_object()
  }
}

fn publish_request(queue: &Path, staging_name: &str, bytes: &[u8]) -> RailResult<()> {
  let pending = queue.join(format!(".{staging_name}.{}.pending", std::process::id()));
  let ready = queue.join(format!("{staging_name}.json"));
  let mut file = OpenOptions::new().write(true).create_new(true).open(&pending)?;
  file.write_all(bytes)?;
  drop(file);
  fs::rename(pending, ready)?;
  Ok(())
}

impl PublicationServer {
  fn serve(self) {
    let queue = self.staging_root.join("queue");
    let mut pending = Vec::new();
    loop {
      let processed = self.process_ready_requests(&queue, &mut pending);
      if self.discovery_only
        && discovery_cohort_ready(&pending)
        && let Some(admission_session) = self.admission_session.as_ref()
      {
        self.commit_discovery_batch(admission_session, std::mem::take(&mut pending));
      }
      if self.stop.load(Ordering::Acquire) && !queue_has_ready_requests(&queue) {
        if self.discovery_only
          && let Some(admission_session) = self.admission_session.as_ref()
        {
          self.commit_discovery_batch(admission_session, std::mem::take(&mut pending));
        }
        break;
      }
      if !processed {
        std::thread::sleep(POLL_INTERVAL);
      }
    }
  }

  fn process_ready_requests(&self, queue: &Path, pending: &mut Vec<PendingDiscoveryAdmission>) -> bool {
    let Ok(entries) = fs::read_dir(queue) else {
      self.metrics.rejected.fetch_add(1, Ordering::Relaxed);
      return false;
    };
    let mut paths = entries
      .filter_map(Result::ok)
      .map(|entry| entry.path())
      .filter(|path| path.extension() == Some(OsStr::new("json")))
      .collect::<Vec<_>>();
    paths.sort_unstable();
    if paths.is_empty() {
      return false;
    }
    for path in paths {
      let result = read_request(&path).and_then(|request| {
        request.validate()?;
        if request.discovery_only != self.discovery_only {
          return Err(RailError::message(
            "native publication request changed session authority",
          ));
        }
        let admission_session = self
          .admission_session
          .as_ref()
          .ok_or_else(|| RailError::message("exact native publication session is unavailable"))?;
        if self.discovery_only {
          self
            .stage_discovery_admission(admission_session, request)
            .map(|admission| pending.push(admission))
        } else {
          self.admit(admission_session, request)
        }
      });
      if result.is_err() {
        self.metrics.rejected.fetch_add(1, Ordering::Relaxed);
      }
      let _ = fs::remove_file(path);
      if self.discovery_only && discovery_cohort_ready(pending) {
        break;
      }
    }
    true
  }

  fn stage_discovery_admission(
    &self,
    admission_session: &NativeCompilerSession,
    request: PublicationRequest,
  ) -> RailResult<PendingDiscoveryAdmission> {
    let staging = self.staging_root.join("incoming").join(&request.staging_name);
    validate_staging_path(&self.staging_root, &staging)?;
    let PublicationRequest {
      manifest,
      validation,
      observed_outputs,
      proof,
      reason,
      remote_publishable,
      cache_bytes_read,
      wrapper_trace,
      ..
    } = request;
    let validation =
      validation.rebind_discovery_session(&self.discovery_session, admission_session, &proof, &self.source_root)?;
    let environment_selector = (
      validation.publication_base_action_key(admission_session, &self.source_root, &proof)?,
      validation.compiler_environment_names.clone(),
    );
    let mut observation = validation.observation.clone();
    observation.emitted_outputs = observed_outputs;
    let prepared = PreparedNativeResult {
      staging: PreparedNativeStaging::CommandScoped(staging),
      staging_lock: None,
      manifest,
      validation,
      origin: PreparedNativeOrigin::Local,
      move_preverified_blobs: true,
    };
    Ok(PendingDiscoveryAdmission {
      staged: self.cas.stage_native(prepared)?,
      report: AdmissionReport {
        observation,
        reason,
        remote_publishable,
        cache_bytes_read,
        wrapper_trace,
        environment_selector,
      },
      proof,
    })
  }

  fn commit_discovery_batch(&self, admission_session: &NativeCompilerSession, pending: Vec<PendingDiscoveryAdmission>) {
    let mut staged = Vec::with_capacity(pending.len());
    let mut reports = Vec::with_capacity(pending.len());
    for admission in pending {
      let PendingDiscoveryAdmission {
        staged: result,
        report,
        proof,
      } = admission;
      match result
        .validation()
        .revalidate_publication(admission_session, &self.source_root, &proof)
      {
        Ok(bytes_hashed) => {
          if self.publish_environment_selector(&report.environment_selector).is_ok() {
            staged.push(result);
            reports.push((report, bytes_hashed));
          } else if record_admission(
            &self.observations,
            &self.source_root,
            report,
            bytes_hashed,
            AdmissionOutcome::StoreFailed,
          )
          .is_err()
          {
            self.metrics.rejected.fetch_add(1, Ordering::Relaxed);
          }
        }
        Err(_) => {
          if record_admission(
            &self.observations,
            &self.source_root,
            report,
            0,
            AdmissionOutcome::RevalidationFailed,
          )
          .is_err()
          {
            self.metrics.rejected.fetch_add(1, Ordering::Relaxed);
          }
        }
      }
    }
    match self.cas.commit_new_native_batch(staged) {
      Ok(admitted) if admitted.len() == reports.len() => {
        for ((report, bytes_hashed), (validation, stats)) in reports.into_iter().zip(admitted) {
          if record_admission(
            &self.observations,
            &self.source_root,
            report,
            bytes_hashed,
            AdmissionOutcome::Stored(&validation, stats),
          )
          .is_err()
          {
            self.metrics.rejected.fetch_add(1, Ordering::Relaxed);
          }
        }
      }
      _ => {
        for (report, bytes_hashed) in reports {
          if record_admission(
            &self.observations,
            &self.source_root,
            report,
            bytes_hashed,
            AdmissionOutcome::StoreFailed,
          )
          .is_err()
          {
            self.metrics.rejected.fetch_add(1, Ordering::Relaxed);
          }
        }
      }
    }
  }
}

fn discovery_cohort_ready(pending: &[PendingDiscoveryAdmission]) -> bool {
  pending.len() >= LOCAL_COMMIT_COHORT_RESULTS
}

fn read_request(path: &Path) -> RailResult<PublicationRequest> {
  let metadata = fs::symlink_metadata(path)?;
  if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) || metadata.len() > MAX_REQUEST_BYTES as u64
  {
    return Err(RailError::message("native publication queue entry is invalid"));
  }
  let bytes = super::read_bounded(path, MAX_REQUEST_BYTES)?;
  Ok(serde_json::from_slice(&bytes)?)
}

fn queue_has_ready_requests(queue: &Path) -> bool {
  fs::read_dir(queue).is_ok_and(|entries| {
    entries
      .filter_map(Result::ok)
      .any(|entry| entry.path().extension() == Some(OsStr::new("json")))
  })
}

impl PublicationServer {
  fn admit(&self, admission_session: &NativeCompilerSession, request: PublicationRequest) -> RailResult<()> {
    let staging = self.staging_root.join("incoming").join(&request.staging_name);
    validate_staging_path(&self.staging_root, &staging)?;
    let PublicationRequest {
      manifest,
      validation,
      observed_outputs,
      proof,
      reason,
      remote_publishable,
      cache_bytes_read,
      wrapper_trace,
      ..
    } = request;
    if self.discovery_session != *admission_session || admission_session.authority != NativeSessionAuthority::Exact {
      return Err(RailError::message(
        "native publication exact session changed before admission",
      ));
    }
    let mut observation = validation.observation.clone();
    observation.emitted_outputs = observed_outputs;
    let environment_selector = (
      validation.publication_base_action_key(admission_session, &self.source_root, &proof)?,
      validation.compiler_environment_names.clone(),
    );
    let prepared = PreparedNativeResult {
      staging: PreparedNativeStaging::CommandScoped(staging),
      staging_lock: None,
      manifest,
      validation,
      origin: PreparedNativeOrigin::Local,
      move_preverified_blobs: true,
    };
    let mut final_capture_bytes = 0;
    let mut revalidation_failed = false;
    let admitted = self.cas.store_native_revalidated(prepared, |validation| {
      match validation.revalidate_publication(admission_session, &self.source_root, &proof) {
        Ok(bytes) => {
          final_capture_bytes = bytes;
          self.publish_environment_selector(&environment_selector)
        }
        Err(error) => {
          revalidation_failed = true;
          Err(error)
        }
      }
    });
    let report = AdmissionReport {
      observation,
      reason,
      remote_publishable,
      cache_bytes_read,
      wrapper_trace,
      environment_selector,
    };
    match admitted {
      Ok((validation, stats)) => record_admission(
        &self.observations,
        &self.source_root,
        report,
        final_capture_bytes,
        AdmissionOutcome::Stored(&validation, stats),
      ),
      Err(_) if revalidation_failed => record_admission(
        &self.observations,
        &self.source_root,
        report,
        final_capture_bytes,
        AdmissionOutcome::RevalidationFailed,
      ),
      Err(_) => record_admission(
        &self.observations,
        &self.source_root,
        report,
        final_capture_bytes,
        AdmissionOutcome::StoreFailed,
      ),
    }
  }

  fn publish_environment_selector(&self, environment_selector: &(String, Vec<String>)) -> RailResult<()> {
    let (base_action_key, names) = environment_selector;
    record_environment_selector_publication(
      &self.metrics,
      self.cas.publish_native_environment_selector(base_action_key, names)?,
    )
  }
}

fn record_environment_selector_publication(
  metrics: &CoordinatorMetrics,
  publication: crate::hermetic::cas::NativeEnvironmentSelectorPublication,
) -> RailResult<()> {
  match publication {
    crate::hermetic::cas::NativeEnvironmentSelectorPublication::Created
    | crate::hermetic::cas::NativeEnvironmentSelectorPublication::Converged => Ok(()),
    crate::hermetic::cas::NativeEnvironmentSelectorPublication::Diverged => {
      metrics.selector_diverged.store(true, Ordering::Relaxed);
      Err(RailError::message("native environment selector diverged"))
    }
  }
}

fn record_admission(
  observations: &Path,
  source_root: &Path,
  report: AdmissionReport,
  final_capture_bytes: u64,
  outcome: AdmissionOutcome<'_>,
) -> RailResult<()> {
  let AdmissionReport {
    mut observation,
    reason,
    remote_publishable,
    cache_bytes_read,
    wrapper_trace,
    environment_selector: (base_action_key, _),
  } = report;
  let initial = observation.cache_wrapper.clone();
  let failure = match &outcome {
    AdmissionOutcome::Stored(_, _) => None,
    AdmissionOutcome::RevalidationFailed => Some("cold_inputs_changed_before_admission"),
    AdmissionOutcome::StoreFailed => Some("local_cache_store_failed"),
  };
  match outcome {
    AdmissionOutcome::Stored(validation, stats) => {
      let stored_reason = format!("{reason};stored_verified_result");
      let bytes_hashed = cold_input_bytes(&observation, source_root, final_capture_bytes);
      observation.cache_wrapper = Some(CompilerCacheWrapperMetadata::native(
        CompilerCacheWrapperStatus::Miss,
        &stored_reason,
        Some(validation.action_key.clone()),
        Some(validation.result_key.clone()),
        bytes_hashed,
        0,
      ));
      write_cache_event_at(
        &observations.join("native-cache-events"),
        CompilerCacheWrapperStatus::Miss,
        &stored_reason,
        Some(validation.action_key()),
        Some(validation.result_key()),
        remote_publishable.then_some(base_action_key.as_str()),
        NativeCacheMetrics {
          bytes_hashed,
          cache_bytes_read,
          cache_bytes_written: stats.bytes_written,
          bytes_restored: 0,
        },
        wrapper_trace,
      );
    }
    AdmissionOutcome::RevalidationFailed | AdmissionOutcome::StoreFailed => {
      let Some(failure) = failure else {
        return Err(RailError::message("native admission outcome lost its failure reason"));
      };
      let rejected_reason = format!("{reason};{failure}");
      let bytes_hashed = cold_input_bytes(&observation, source_root, final_capture_bytes);
      observation.cache_wrapper = Some(CompilerCacheWrapperMetadata::native(
        CompilerCacheWrapperStatus::Bypassed,
        &rejected_reason,
        initial
          .as_ref()
          .and_then(CompilerCacheWrapperMetadata::action_key)
          .map(str::to_string),
        None,
        bytes_hashed,
        0,
      ));
      write_cache_event_at(
        &observations.join("native-cache-events"),
        CompilerCacheWrapperStatus::Bypassed,
        &rejected_reason,
        initial.as_ref().and_then(CompilerCacheWrapperMetadata::action_key),
        None,
        None,
        NativeCacheMetrics {
          bytes_hashed,
          cache_bytes_read,
          ..NativeCacheMetrics::default()
        },
        wrapper_trace,
      );
    }
  }
  crate::compiler::observation::publish_raw(observations, &observation)
}

fn validate_staging_path(root: &Path, staging: &Path) -> RailResult<()> {
  if staging.parent() != Some(root.join("incoming").as_path()) {
    return Err(RailError::message(
      "native publication result escaped its command staging",
    ));
  }
  let metadata = fs::symlink_metadata(staging)?;
  if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
    return Err(RailError::message(
      "native publication result staging is not a real directory",
    ));
  }
  Ok(())
}

fn valid_staging_name(name: &str) -> bool {
  name.starts_with("native-unit-")
    && name.len() <= 255
    && !name.as_bytes().contains(&0)
    && matches!(
      Path::new(name).components().collect::<Vec<_>>().as_slice(),
      [Component::Normal(_)]
    )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn staging_names_are_single_bounded_components() {
    assert!(valid_staging_name("native-unit-abc123"));
    for invalid in [
      "native-result-abc123",
      "native-unit-../escape",
      "native-unit-a/b",
      "../native-unit-abc123",
      "",
    ] {
      assert!(!valid_staging_name(invalid), "{invalid}");
    }
  }

  #[test]
  fn worker_drain_waits_for_worker_owned_state() {
    let completed = Arc::new(AtomicBool::new(false));
    let worker_completed = Arc::clone(&completed);
    let handle = std::thread::spawn(move || {
      worker_completed.store(true, Ordering::Release);
    });

    assert_eq!(finish_worker(PublicationWorker { handle }), WorkerDrain::Completed);
    assert!(completed.load(Ordering::Acquire));
  }

  #[test]
  fn only_selector_divergence_sets_the_operational_report_bit() {
    for publication in [
      crate::hermetic::cas::NativeEnvironmentSelectorPublication::Created,
      crate::hermetic::cas::NativeEnvironmentSelectorPublication::Converged,
    ] {
      let metrics = CoordinatorMetrics::default();
      record_environment_selector_publication(&metrics, publication).expect("ordinary selector publication");
      assert!(!metrics.selector_diverged.load(Ordering::Relaxed));
    }

    let metrics = CoordinatorMetrics::default();
    record_environment_selector_publication(
      &metrics,
      crate::hermetic::cas::NativeEnvironmentSelectorPublication::Diverged,
    )
    .expect_err("divergent selector publication must fail");
    assert!(metrics.selector_diverged.load(Ordering::Relaxed));
  }
}
