//! Explicit, out-of-band diagnostic counters for performance workloads.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{RailError, RailResult};

const SCHEMA_VERSION: u32 = 7;

static COUNTERS: OnceLock<Counters> = OnceLock::new();

struct Counters {
  snapshot_id: OnceLock<String>,
  native_cache_wrapper: Mutex<Option<NativeCacheWrapperDiagnostics>>,
  phases: PhaseCounters,
  cargo_metadata_loads: AtomicU64,
  cargo_metadata_cache_hits: AtomicU64,
  target_view_loads: AtomicU64,
  hash_operations: AtomicU64,
  hash_input_bytes: AtomicU64,
  hashed_file_bytes_read: AtomicU64,
  git_subprocesses: AtomicU64,
  git_object_reads: AtomicU64,
  git_object_read_batches: AtomicU64,
  git_path_change_reads: AtomicU64,
  git_path_change_batches: AtomicU64,
  graph_traversals: AtomicU64,
  graph_node_visits: AtomicU64,
  graph_edge_visits: AtomicU64,
  cas_objects_written: AtomicU64,
  cas_bytes_written: AtomicU64,
  cas_bytes_read: AtomicU64,
  cas_bytes_restored: AtomicU64,
  hermetic_cargo_executions: AtomicU64,
  hermetic_compiler_units: AtomicU64,
  hermetic_fetch_executions: AtomicU64,
  hermetic_cargo_probes: AtomicU64,
  hermetic_rustc_probes: AtomicU64,
  hermetic_rustdoc_probes: AtomicU64,
  hermetic_platform_probes: AtomicU64,
}

impl Counters {
  const fn new() -> Self {
    Self {
      snapshot_id: OnceLock::new(),
      native_cache_wrapper: Mutex::new(None),
      phases: PhaseCounters::new(),
      cargo_metadata_loads: AtomicU64::new(0),
      cargo_metadata_cache_hits: AtomicU64::new(0),
      target_view_loads: AtomicU64::new(0),
      hash_operations: AtomicU64::new(0),
      hash_input_bytes: AtomicU64::new(0),
      hashed_file_bytes_read: AtomicU64::new(0),
      git_subprocesses: AtomicU64::new(0),
      git_object_reads: AtomicU64::new(0),
      git_object_read_batches: AtomicU64::new(0),
      git_path_change_reads: AtomicU64::new(0),
      git_path_change_batches: AtomicU64::new(0),
      graph_traversals: AtomicU64::new(0),
      graph_node_visits: AtomicU64::new(0),
      graph_edge_visits: AtomicU64::new(0),
      cas_objects_written: AtomicU64::new(0),
      cas_bytes_written: AtomicU64::new(0),
      cas_bytes_read: AtomicU64::new(0),
      cas_bytes_restored: AtomicU64::new(0),
      hermetic_cargo_executions: AtomicU64::new(0),
      hermetic_compiler_units: AtomicU64::new(0),
      hermetic_fetch_executions: AtomicU64::new(0),
      hermetic_cargo_probes: AtomicU64::new(0),
      hermetic_rustc_probes: AtomicU64::new(0),
      hermetic_rustdoc_probes: AtomicU64::new(0),
      hermetic_platform_probes: AtomicU64::new(0),
    }
  }

  fn snapshot(&self) -> CounterSnapshot {
    CounterSnapshot {
      schema_version: SCHEMA_VERSION,
      snapshot_id: self.snapshot_id.get().cloned(),
      native_cache_wrapper: self
        .native_cache_wrapper
        .lock()
        .ok()
        .and_then(|diagnostics| diagnostics.clone()),
      phases: self.phases.snapshot(),
      cargo_metadata_loads: self.cargo_metadata_loads.load(Ordering::Relaxed),
      cargo_metadata_cache_hits: self.cargo_metadata_cache_hits.load(Ordering::Relaxed),
      target_view_loads: self.target_view_loads.load(Ordering::Relaxed),
      hash_operations: self.hash_operations.load(Ordering::Relaxed),
      hash_input_bytes: self.hash_input_bytes.load(Ordering::Relaxed),
      hashed_file_bytes_read: self.hashed_file_bytes_read.load(Ordering::Relaxed),
      git_subprocesses: self.git_subprocesses.load(Ordering::Relaxed),
      git_object_reads: self.git_object_reads.load(Ordering::Relaxed),
      git_object_read_batches: self.git_object_read_batches.load(Ordering::Relaxed),
      git_path_change_reads: self.git_path_change_reads.load(Ordering::Relaxed),
      git_path_change_batches: self.git_path_change_batches.load(Ordering::Relaxed),
      graph_traversals: self.graph_traversals.load(Ordering::Relaxed),
      graph_node_visits: self.graph_node_visits.load(Ordering::Relaxed),
      graph_edge_visits: self.graph_edge_visits.load(Ordering::Relaxed),
      cas_objects_written: self.cas_objects_written.load(Ordering::Relaxed),
      cas_bytes_written: self.cas_bytes_written.load(Ordering::Relaxed),
      cas_bytes_read: self.cas_bytes_read.load(Ordering::Relaxed),
      cas_bytes_restored: self.cas_bytes_restored.load(Ordering::Relaxed),
      hermetic_cargo_executions: self.hermetic_cargo_executions.load(Ordering::Relaxed),
      hermetic_compiler_units: self.hermetic_compiler_units.load(Ordering::Relaxed),
      hermetic_fetch_executions: self.hermetic_fetch_executions.load(Ordering::Relaxed),
      hermetic_cargo_probes: self.hermetic_cargo_probes.load(Ordering::Relaxed),
      hermetic_rustc_probes: self.hermetic_rustc_probes.load(Ordering::Relaxed),
      hermetic_rustdoc_probes: self.hermetic_rustdoc_probes.load(Ordering::Relaxed),
      hermetic_platform_probes: self.hermetic_platform_probes.load(Ordering::Relaxed),
    }
  }
}

struct PhaseCounters {
  cli_pre_context_preparation: PhaseCounter,
  workspace_capture_cargo_metadata: PhaseCounter,
  action_expansion_key_construction: PhaseCounter,
  native_cache_setup: PhaseCounter,
  sysroot_fingerprinting: PhaseCounter,
  cargo_child_execution: PhaseCounter,
  cache_report_collection: PhaseCounter,
}

impl PhaseCounters {
  const fn new() -> Self {
    Self {
      cli_pre_context_preparation: PhaseCounter::new(),
      workspace_capture_cargo_metadata: PhaseCounter::new(),
      action_expansion_key_construction: PhaseCounter::new(),
      native_cache_setup: PhaseCounter::new(),
      sysroot_fingerprinting: PhaseCounter::new(),
      cargo_child_execution: PhaseCounter::new(),
      cache_report_collection: PhaseCounter::new(),
    }
  }

  fn snapshot(&self) -> PhaseSnapshots {
    PhaseSnapshots {
      cli_pre_context_preparation: self.cli_pre_context_preparation.snapshot(),
      workspace_capture_cargo_metadata: self.workspace_capture_cargo_metadata.snapshot(),
      action_expansion_key_construction: self.action_expansion_key_construction.snapshot(),
      native_cache_setup: self.native_cache_setup.snapshot(),
      sysroot_fingerprinting: self.sysroot_fingerprinting.snapshot(),
      cargo_child_execution: self.cargo_child_execution.snapshot(),
      cache_report_collection: self.cache_report_collection.snapshot(),
    }
  }
}

struct PhaseCounter {
  invocations: AtomicU64,
  elapsed_ns: AtomicU64,
}

impl PhaseCounter {
  const fn new() -> Self {
    Self {
      invocations: AtomicU64::new(0),
      elapsed_ns: AtomicU64::new(0),
    }
  }

  fn record(&self, started: Instant) {
    self.invocations.fetch_add(1, Ordering::Relaxed);
    self.elapsed_ns.fetch_add(duration_ns(started), Ordering::Relaxed);
  }

  fn snapshot(&self) -> PhaseSnapshot {
    PhaseSnapshot {
      invocations: self.invocations.load(Ordering::Relaxed),
      elapsed_ns: self.elapsed_ns.load(Ordering::Relaxed),
    }
  }
}

#[derive(Serialize)]
struct PhaseSnapshots {
  cli_pre_context_preparation: PhaseSnapshot,
  workspace_capture_cargo_metadata: PhaseSnapshot,
  action_expansion_key_construction: PhaseSnapshot,
  native_cache_setup: PhaseSnapshot,
  sysroot_fingerprinting: PhaseSnapshot,
  cargo_child_execution: PhaseSnapshot,
  cache_report_collection: PhaseSnapshot,
}

#[derive(Serialize)]
struct PhaseSnapshot {
  invocations: u64,
  elapsed_ns: u64,
}

#[derive(Serialize)]
struct CounterSnapshot {
  schema_version: u32,
  snapshot_id: Option<String>,
  native_cache_wrapper: Option<NativeCacheWrapperDiagnostics>,
  phases: PhaseSnapshots,
  cargo_metadata_loads: u64,
  cargo_metadata_cache_hits: u64,
  target_view_loads: u64,
  hash_operations: u64,
  hash_input_bytes: u64,
  hashed_file_bytes_read: u64,
  git_subprocesses: u64,
  git_object_reads: u64,
  git_object_read_batches: u64,
  git_path_change_reads: u64,
  git_path_change_batches: u64,
  graph_traversals: u64,
  graph_node_visits: u64,
  graph_edge_visits: u64,
  cas_objects_written: u64,
  cas_bytes_written: u64,
  cas_bytes_read: u64,
  cas_bytes_restored: u64,
  hermetic_cargo_executions: u64,
  hermetic_compiler_units: u64,
  hermetic_fetch_executions: u64,
  hermetic_cargo_probes: u64,
  hermetic_rustc_probes: u64,
  hermetic_rustdoc_probes: u64,
  hermetic_platform_probes: u64,
}

/// Fixed diagnostic phases inside one native-cache compiler wrapper.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NativeCacheWrapperPhase {
  ContextLoad,
  SessionLoad,
  ArgumentNormalizationInputCapture,
  BypassClassification,
  CandidateKeyConstruction,
  CasOpen,
  CandidateLookup,
  InputRevalidationActionKey,
  ResultRestoreMaterialization,
  CargoOutputPublication,
}

impl NativeCacheWrapperPhase {
  const ALL: [Self; 10] = [
    Self::ContextLoad,
    Self::SessionLoad,
    Self::ArgumentNormalizationInputCapture,
    Self::BypassClassification,
    Self::CandidateKeyConstruction,
    Self::CasOpen,
    Self::CandidateLookup,
    Self::InputRevalidationActionKey,
    Self::ResultRestoreMaterialization,
    Self::CargoOutputPublication,
  ];

  fn name(self) -> &'static str {
    match self {
      Self::ContextLoad => "context_load",
      Self::SessionLoad => "session_load",
      Self::ArgumentNormalizationInputCapture => "argument_normalization_input_capture",
      Self::BypassClassification => "bypass_classification",
      Self::CandidateKeyConstruction => "candidate_key_construction",
      Self::CasOpen => "cas_open",
      Self::CandidateLookup => "candidate_lookup",
      Self::InputRevalidationActionKey => "input_revalidation_action_key",
      Self::ResultRestoreMaterialization => "result_restore_materialization",
      Self::CargoOutputPublication => "cargo_output_publication",
    }
  }
}

/// Bytes attributable to one diagnostic wrapper phase.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeCacheWrapperWork {
  pub(crate) bytes_hashed: u64,
  pub(crate) cache_bytes_read: u64,
  pub(crate) cache_bytes_written: u64,
  pub(crate) bytes_restored: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeCacheWrapperPhaseTrace {
  phase: NativeCacheWrapperPhase,
  start_unix_ns: u64,
  elapsed_ns: u64,
  work: NativeCacheWrapperWork,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeCacheWrapperTraceSnapshot {
  process_start_unix_ns: u64,
  process_elapsed_ns: u64,
  phases: Vec<NativeCacheWrapperPhaseTrace>,
}

/// Diagnostic identity and phase evidence from one compiler-wrapper process.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct NativeCacheWrapperEventDiagnostics {
  unit_identity: Option<String>,
  outcome: String,
  reason: String,
  trace: NativeCacheWrapperTraceSnapshot,
}

impl NativeCacheWrapperEventDiagnostics {
  pub(crate) fn new(
    unit_identity: Option<String>,
    outcome: &str,
    reason: String,
    trace: NativeCacheWrapperTraceSnapshot,
  ) -> Self {
    Self {
      unit_identity,
      outcome: outcome.to_string(),
      reason,
      trace,
    }
  }
}

/// Concurrency-aware native-cache wrapper diagnostics for one Cargo process.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct NativeCacheWrapperDiagnostics {
  process: NativeCacheWrapperPhaseSummary,
  phases: BTreeMap<&'static str, NativeCacheWrapperPhaseSummary>,
  events: Vec<NativeCacheWrapperEventDiagnostics>,
}

#[derive(Clone, Debug, Default, Serialize)]
struct NativeCacheWrapperPhaseSummary {
  invocations: u64,
  aggregate_elapsed_ns: u64,
  wall_occupied_ns: u64,
  bytes_hashed: u64,
  cache_bytes_read: u64,
  cache_bytes_written: u64,
  bytes_restored: u64,
}

impl NativeCacheWrapperDiagnostics {
  pub(crate) fn from_events(mut events: Vec<NativeCacheWrapperEventDiagnostics>) -> Option<Self> {
    if events.is_empty() {
      return None;
    }
    events.sort_by(|left, right| {
      (
        left.trace.process_start_unix_ns,
        &left.unit_identity,
        &left.outcome,
        &left.reason,
      )
        .cmp(&(
          right.trace.process_start_unix_ns,
          &right.unit_identity,
          &right.outcome,
          &right.reason,
        ))
    });

    let mut phases = NativeCacheWrapperPhase::ALL
      .into_iter()
      .map(|phase| (phase.name(), NativeCacheWrapperPhaseSummary::default()))
      .collect::<BTreeMap<_, _>>();
    let mut intervals = NativeCacheWrapperPhase::ALL
      .into_iter()
      .map(|phase| (phase, Vec::new()))
      .collect::<BTreeMap<_, _>>();
    let mut process_intervals = Vec::with_capacity(events.len());
    let mut process = NativeCacheWrapperPhaseSummary::default();

    for event in &events {
      process.invocations = process.invocations.saturating_add(1);
      process.aggregate_elapsed_ns = process
        .aggregate_elapsed_ns
        .saturating_add(event.trace.process_elapsed_ns);
      process_intervals.push((
        event.trace.process_start_unix_ns,
        event
          .trace
          .process_start_unix_ns
          .saturating_add(event.trace.process_elapsed_ns),
      ));
      for phase in &event.trace.phases {
        let Some(summary) = phases.get_mut(phase.phase.name()) else {
          continue;
        };
        summary.invocations = summary.invocations.saturating_add(1);
        summary.aggregate_elapsed_ns = summary.aggregate_elapsed_ns.saturating_add(phase.elapsed_ns);
        summary.bytes_hashed = summary.bytes_hashed.saturating_add(phase.work.bytes_hashed);
        summary.cache_bytes_read = summary.cache_bytes_read.saturating_add(phase.work.cache_bytes_read);
        summary.cache_bytes_written = summary
          .cache_bytes_written
          .saturating_add(phase.work.cache_bytes_written);
        summary.bytes_restored = summary.bytes_restored.saturating_add(phase.work.bytes_restored);
        if let Some(intervals) = intervals.get_mut(&phase.phase) {
          intervals.push((
            phase.start_unix_ns,
            phase.start_unix_ns.saturating_add(phase.elapsed_ns),
          ));
        }
      }
    }
    process.wall_occupied_ns = occupied_ns(&mut process_intervals);
    for (phase, phase_intervals) in &mut intervals {
      if let Some(summary) = phases.get_mut(phase.name()) {
        summary.wall_occupied_ns = occupied_ns(phase_intervals);
      }
    }
    Some(Self {
      process,
      phases,
      events,
    })
  }

  fn merge(&mut self, other: Self) {
    let mut events = std::mem::take(&mut self.events);
    events.extend(other.events);
    if let Some(merged) = Self::from_events(events) {
      *self = merged;
    }
  }
}

fn occupied_ns(intervals: &mut [(u64, u64)]) -> u64 {
  intervals.sort_unstable();
  let mut occupied = 0u64;
  let mut current: Option<(u64, u64)> = None;
  for &(start, end) in intervals.iter() {
    current = match current {
      Some((current_start, current_end)) if start <= current_end => Some((current_start, current_end.max(end))),
      Some((current_start, current_end)) => {
        occupied = occupied.saturating_add(current_end.saturating_sub(current_start));
        Some((start, end))
      }
      None => Some((start, end)),
    };
  }
  if let Some((start, end)) = current {
    occupied = occupied.saturating_add(end.saturating_sub(start));
  }
  occupied
}

/// Clock anchor captured before the private direct-wrapper context is loaded.
pub(crate) struct NativeCacheWrapperProcessStart {
  monotonic: Instant,
  unix_ns: u64,
}

impl NativeCacheWrapperProcessStart {
  pub(crate) fn capture() -> Self {
    Self {
      monotonic: Instant::now(),
      unix_ns: system_time_ns(),
    }
  }

  pub(crate) fn finish_context_load(self, enabled: bool) -> NativeCacheWrapperTrace {
    let elapsed_ns = duration_ns(self.monotonic);
    let phases = enabled.then(|| {
      vec![NativeCacheWrapperPhaseTrace {
        phase: NativeCacheWrapperPhase::ContextLoad,
        start_unix_ns: self.unix_ns,
        elapsed_ns,
        work: NativeCacheWrapperWork::default(),
      }]
    });
    NativeCacheWrapperTrace {
      process_started: self.monotonic,
      process_start_unix_ns: self.unix_ns,
      phases,
    }
  }
}

/// Diagnostic-only timing state for one compiler-wrapper process.
pub(crate) struct NativeCacheWrapperTrace {
  process_started: Instant,
  process_start_unix_ns: u64,
  phases: Option<Vec<NativeCacheWrapperPhaseTrace>>,
}

pub(crate) struct NativeCacheWrapperPhaseStart {
  phase: NativeCacheWrapperPhase,
  started: Instant,
  start_unix_ns: u64,
}

impl NativeCacheWrapperTrace {
  pub(crate) fn disabled() -> Self {
    NativeCacheWrapperProcessStart::capture().finish_context_load(false)
  }

  pub(crate) fn start(&self, phase: NativeCacheWrapperPhase) -> Option<NativeCacheWrapperPhaseStart> {
    self.phases.as_ref()?;
    let started = Instant::now();
    Some(NativeCacheWrapperPhaseStart {
      phase,
      started,
      start_unix_ns: self
        .process_start_unix_ns
        .saturating_add(instant_duration_ns(self.process_started, started)),
    })
  }

  pub(crate) fn finish(&mut self, started: Option<NativeCacheWrapperPhaseStart>, work: NativeCacheWrapperWork) {
    let (Some(phases), Some(started)) = (&mut self.phases, started) else {
      return;
    };
    phases.push(NativeCacheWrapperPhaseTrace {
      phase: started.phase,
      start_unix_ns: started.start_unix_ns,
      elapsed_ns: duration_ns(started.started),
      work,
    });
  }

  pub(crate) fn snapshot(&self) -> Option<NativeCacheWrapperTraceSnapshot> {
    Some(NativeCacheWrapperTraceSnapshot {
      process_start_unix_ns: self.process_start_unix_ns,
      process_elapsed_ns: duration_ns(self.process_started),
      phases: self.phases.clone()?,
    })
  }
}

#[derive(Clone, Copy)]
enum DiagnosticPhase {
  ActionExpansionKeyConstruction,
  NativeCacheSetup,
  SysrootFingerprinting,
  CargoChildExecution,
  CacheReportCollection,
}

/// Active timer for one fixed diagnostic phase.
pub(crate) struct DiagnosticPhaseGuard {
  phase: DiagnosticPhase,
  started: Option<Instant>,
}

impl DiagnosticPhaseGuard {
  fn start(phase: DiagnosticPhase) -> Self {
    Self {
      phase,
      started: COUNTERS.get().map(|_| Instant::now()),
    }
  }
}

impl Drop for DiagnosticPhaseGuard {
  fn drop(&mut self) {
    let (Some(counters), Some(started)) = (COUNTERS.get(), self.started) else {
      return;
    };
    let counter = match self.phase {
      DiagnosticPhase::ActionExpansionKeyConstruction => &counters.phases.action_expansion_key_construction,
      DiagnosticPhase::NativeCacheSetup => &counters.phases.native_cache_setup,
      DiagnosticPhase::SysrootFingerprinting => &counters.phases.sysroot_fingerprinting,
      DiagnosticPhase::CargoChildExecution => &counters.phases.cargo_child_execution,
      DiagnosticPhase::CacheReportCollection => &counters.phases.cache_report_collection,
    };
    counter.record(started);
  }
}

/// Active diagnostic session for one cargo-rail process.
#[doc(hidden)]
pub struct DiagnosticSession {
  output: Option<(PathBuf, File)>,
}

impl DiagnosticSession {
  /// Enable counters when an explicit output path was supplied.
  #[doc(hidden)]
  pub fn start(output: Option<PathBuf>) -> RailResult<Self> {
    let output = match output {
      Some(path) => {
        let file = File::options()
          .write(true)
          .create_new(true)
          .open(&path)
          .map_err(|error| {
            RailError::message(format!(
              "failed to reserve diagnostic counter file '{}': {error}",
              path.display()
            ))
          })?;
        let _ = COUNTERS.get_or_init(Counters::new);
        Some((path, file))
      }
      None => None,
    };
    Ok(Self { output })
  }

  /// Write the final counter snapshot without touching normal stdout or stderr.
  #[doc(hidden)]
  pub fn finish(self) -> RailResult<()> {
    let Some((output, mut file)) = self.output else {
      return Ok(());
    };
    let Some(counters) = COUNTERS.get() else {
      return Ok(());
    };

    let mut encoded = serde_json::to_vec_pretty(&counters.snapshot())
      .map_err(|error| RailError::message(format!("failed to serialize diagnostic counters: {error}")))?;
    encoded.push(b'\n');
    file.write_all(&encoded).map_err(|error| {
      RailError::message(format!(
        "failed to write diagnostic counters to '{}': {error}",
        output.display()
      ))
    })?;
    file.sync_all().map_err(|error| {
      RailError::message(format!(
        "failed to sync diagnostic counters to '{}': {error}",
        output.display()
      ))
    })
  }
}

fn amount(value: usize) -> u64 {
  u64::try_from(value).unwrap_or(u64::MAX)
}

fn duration_ns(started: Instant) -> u64 {
  u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn instant_duration_ns(started: Instant, finished: Instant) -> u64 {
  u64::try_from(finished.duration_since(started).as_nanos()).unwrap_or(u64::MAX)
}

fn system_time_ns() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .ok()
    .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
    .unwrap_or_default()
}

fn add(counter: fn(&Counters) -> &AtomicU64, value: u64) {
  if let Some(counters) = COUNTERS.get() {
    counter(counters).fetch_add(value, Ordering::Relaxed);
  }
}

/// Finish the fixed CLI and pre-context preparation phase.
#[doc(hidden)]
pub fn record_cli_pre_context_preparation(started: Instant) {
  if let Some(counters) = COUNTERS.get() {
    counters.phases.cli_pre_context_preparation.record(started);
  }
}

/// Finish the fixed workspace-capture and Cargo-metadata phase.
#[doc(hidden)]
pub fn record_workspace_capture_cargo_metadata(started: Instant) {
  if let Some(counters) = COUNTERS.get() {
    counters.phases.workspace_capture_cargo_metadata.record(started);
  }
}

pub(crate) fn action_expansion_key_construction_phase() -> DiagnosticPhaseGuard {
  DiagnosticPhaseGuard::start(DiagnosticPhase::ActionExpansionKeyConstruction)
}

pub(crate) fn native_cache_setup_phase() -> DiagnosticPhaseGuard {
  DiagnosticPhaseGuard::start(DiagnosticPhase::NativeCacheSetup)
}

pub(crate) fn sysroot_fingerprinting_phase() -> DiagnosticPhaseGuard {
  DiagnosticPhaseGuard::start(DiagnosticPhase::SysrootFingerprinting)
}

pub(crate) fn cargo_child_execution_phase() -> DiagnosticPhaseGuard {
  DiagnosticPhaseGuard::start(DiagnosticPhase::CargoChildExecution)
}

pub(crate) fn cache_report_collection_phase() -> DiagnosticPhaseGuard {
  DiagnosticPhaseGuard::start(DiagnosticPhase::CacheReportCollection)
}

pub(crate) fn record_cargo_metadata_load(target_view: bool) {
  add(|counters| &counters.cargo_metadata_loads, 1);
  if target_view {
    add(|counters| &counters.target_view_loads, 1);
  }
}

/// Record the authoritative workspace identity for diagnostic-only inspection.
#[doc(hidden)]
pub fn record_snapshot_id(snapshot_id: String) {
  if let Some(counters) = COUNTERS.get() {
    let _ = counters.snapshot_id.set(snapshot_id);
  }
}

pub(crate) fn enabled() -> bool {
  COUNTERS.get().is_some()
}

pub(crate) fn record_native_cache_wrapper_diagnostics(diagnostics: NativeCacheWrapperDiagnostics) {
  if let Some(counters) = COUNTERS.get()
    && let Ok(mut current) = counters.native_cache_wrapper.lock()
  {
    if let Some(current) = current.as_mut() {
      current.merge(diagnostics);
    } else {
      *current = Some(diagnostics);
    }
  }
}

pub(crate) fn record_cargo_metadata_cache_hit() {
  add(|counters| &counters.cargo_metadata_cache_hits, 1);
}

pub(crate) fn record_hash(input_bytes: usize) {
  record_hash_operation();
  record_hash_input_bytes(input_bytes);
}

pub(crate) fn record_hash_operation() {
  add(|counters| &counters.hash_operations, 1);
}

pub(crate) fn record_hash_input_bytes(input_bytes: usize) {
  add(|counters| &counters.hash_input_bytes, amount(input_bytes));
}

pub(crate) fn record_hashed_file_bytes_read(bytes: usize) {
  add(|counters| &counters.hashed_file_bytes_read, amount(bytes));
}

pub(crate) fn record_git_subprocess() {
  add(|counters| &counters.git_subprocesses, 1);
}

pub(crate) fn record_git_object_read_batch(objects: usize) {
  add(|counters| &counters.git_object_reads, amount(objects));
  add(|counters| &counters.git_object_read_batches, 1);
}

pub(crate) fn record_git_path_change_batch(commits: usize) {
  add(|counters| &counters.git_path_change_reads, amount(commits));
  add(|counters| &counters.git_path_change_batches, 1);
}

pub(crate) fn record_graph_traversal(node_visits: usize, edge_visits: usize) {
  add(|counters| &counters.graph_traversals, 1);
  add(|counters| &counters.graph_node_visits, amount(node_visits));
  add(|counters| &counters.graph_edge_visits, amount(edge_visits));
}

pub(crate) fn record_cas_write(bytes: u64, objects: u64) {
  add(|counters| &counters.cas_bytes_written, bytes);
  add(|counters| &counters.cas_objects_written, objects);
}

pub(crate) fn record_cas_read(bytes: u64) {
  add(|counters| &counters.cas_bytes_read, bytes);
}

#[cfg(any(unix, windows, test))]
pub(crate) fn record_cas_restore(bytes: u64) {
  add(|counters| &counters.cas_bytes_restored, bytes);
}

pub(crate) fn record_hermetic_cargo_execution() {
  add(|counters| &counters.hermetic_cargo_executions, 1);
}

pub(crate) fn record_hermetic_compiler_units(units: usize) {
  add(|counters| &counters.hermetic_compiler_units, amount(units));
}

pub(crate) fn record_hermetic_fetch_execution() {
  add(|counters| &counters.hermetic_fetch_executions, 1);
}

pub(crate) fn record_hermetic_toolchain_probe(program: &std::ffi::OsStr) {
  let name = std::path::Path::new(program)
    .file_stem()
    .and_then(std::ffi::OsStr::to_str)
    .unwrap_or_default();
  match name {
    "cargo" => add(|counters| &counters.hermetic_cargo_probes, 1),
    "rustc" => add(|counters| &counters.hermetic_rustc_probes, 1),
    "rustdoc" => add(|counters| &counters.hermetic_rustdoc_probes, 1),
    _ => {}
  }
}

#[cfg(target_os = "macos")]
pub(crate) fn record_hermetic_platform_probe() {
  add(|counters| &counters.hermetic_platform_probes, 1);
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn native_cache_wrapper_diagnostics_separate_work_from_wall_occupancy() {
    let event = |process_start_unix_ns, phase_start_unix_ns| {
      NativeCacheWrapperEventDiagnostics::new(
        Some(format!("unit-{process_start_unix_ns}")),
        "hit",
        "verified_local_result".to_string(),
        NativeCacheWrapperTraceSnapshot {
          process_start_unix_ns,
          process_elapsed_ns: 100,
          phases: vec![NativeCacheWrapperPhaseTrace {
            phase: NativeCacheWrapperPhase::CasOpen,
            start_unix_ns: phase_start_unix_ns,
            elapsed_ns: 40,
            work: NativeCacheWrapperWork {
              cache_bytes_read: 10,
              ..NativeCacheWrapperWork::default()
            },
          }],
        },
      )
    };
    let mut diagnostics =
      NativeCacheWrapperDiagnostics::from_events(vec![event(100, 110), event(150, 130)]).expect("wrapper diagnostics");
    diagnostics.merge(NativeCacheWrapperDiagnostics::from_events(vec![event(300, 310)]).expect("later action"));
    let encoded = serde_json::to_value(diagnostics).expect("diagnostics JSON");

    assert_eq!(encoded["process"]["invocations"], 3);
    assert_eq!(encoded["process"]["aggregate_elapsed_ns"], 300);
    assert_eq!(encoded["process"]["wall_occupied_ns"], 250);
    assert_eq!(encoded["phases"].as_object().map(serde_json::Map::len), Some(10));
    assert_eq!(encoded["phases"]["cas_open"]["invocations"], 3);
    assert_eq!(encoded["phases"]["cas_open"]["aggregate_elapsed_ns"], 120);
    assert_eq!(encoded["phases"]["cas_open"]["wall_occupied_ns"], 100);
    assert_eq!(encoded["phases"]["cas_open"]["cache_bytes_read"], 30);
  }
}
