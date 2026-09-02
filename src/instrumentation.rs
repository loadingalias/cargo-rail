//! Explicit, out-of-band diagnostic counters for performance workloads.

use std::fs::File;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::error::{RailError, RailResult};

const SCHEMA_VERSION: u32 = 15;

static COUNTERS: OnceLock<Counters> = OnceLock::new();

struct Counters {
    snapshot_id: OnceLock<String>,
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
    compiler_acquisition: CompilerAcquisitionCounters,
}

impl Counters {
    const fn new() -> Self {
        Self {
            snapshot_id: OnceLock::new(),
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
            compiler_acquisition: CompilerAcquisitionCounters::new(),
        }
    }

    fn snapshot(&self) -> CounterSnapshot {
        CounterSnapshot {
            schema_version: SCHEMA_VERSION,
            snapshot_id: self.snapshot_id.get().cloned(),
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
            compiler_acquisition: self.compiler_acquisition.snapshot(),
        }
    }
}

struct CompilerAcquisitionCounters {
    plan_identity: OnceLock<String>,
    plans: AtomicU64,
    plan_build_elapsed_ns: AtomicU64,
    packages: AtomicU64,
    targets: AtomicU64,
    features: AtomicU64,
    candidates: AtomicU64,
    views: AtomicU64,
    cargo_views: AtomicU64,
    cargo_elapsed_ns: AtomicU64,
    configured_process_slots: AtomicU64,
    configured_work_permits: AtomicU64,
    live_cargo_processes: AtomicU64,
    max_live_cargo_processes: AtomicU64,
    max_nonwaiting_cargo_views: AtomicU64,
    work_permit_start_waits: AtomicU64,
    work_permit_yields: AtomicU64,
    work_permit_resumes: AtomicU64,
    compiler_actions: AtomicU64,
    cargo_messages_read: AtomicU64,
    stdout_bytes_read: AtomicU64,
    stderr_bytes_read: AtomicU64,
    stdout_bytes_retained: AtomicU64,
    stderr_bytes_retained: AtomicU64,
    output_retention_high_water_bytes: AtomicU64,
    process_tree_terminations: AtomicU64,
    process_tree_forced_terminations: AtomicU64,
    process_tree_termination_elapsed_ns: AtomicU64,
    sandboxes_created: AtomicU64,
    sandboxes_reused: AtomicU64,
    sandboxes_poisoned: AtomicU64,
    sandboxes_deleted: AtomicU64,
    artifact_tree_walks: AtomicU64,
    artifact_tree_walk_elapsed_ns: AtomicU64,
    evidence_cache_lookups: AtomicU64,
    evidence_cache_hits: AtomicU64,
    evidence_cache_writes: AtomicU64,
    evidence_cache_elapsed_ns: AtomicU64,
    journal_writes: AtomicU64,
    journal_bytes_written: AtomicU64,
    journal_flushes: AtomicU64,
    journal_syncs: AtomicU64,
    journal_elapsed_ns: AtomicU64,
}

impl CompilerAcquisitionCounters {
    const fn new() -> Self {
        Self {
            plan_identity: OnceLock::new(),
            plans: AtomicU64::new(0),
            plan_build_elapsed_ns: AtomicU64::new(0),
            packages: AtomicU64::new(0),
            targets: AtomicU64::new(0),
            features: AtomicU64::new(0),
            candidates: AtomicU64::new(0),
            views: AtomicU64::new(0),
            cargo_views: AtomicU64::new(0),
            cargo_elapsed_ns: AtomicU64::new(0),
            configured_process_slots: AtomicU64::new(0),
            configured_work_permits: AtomicU64::new(0),
            live_cargo_processes: AtomicU64::new(0),
            max_live_cargo_processes: AtomicU64::new(0),
            max_nonwaiting_cargo_views: AtomicU64::new(0),
            work_permit_start_waits: AtomicU64::new(0),
            work_permit_yields: AtomicU64::new(0),
            work_permit_resumes: AtomicU64::new(0),
            compiler_actions: AtomicU64::new(0),
            cargo_messages_read: AtomicU64::new(0),
            stdout_bytes_read: AtomicU64::new(0),
            stderr_bytes_read: AtomicU64::new(0),
            stdout_bytes_retained: AtomicU64::new(0),
            stderr_bytes_retained: AtomicU64::new(0),
            output_retention_high_water_bytes: AtomicU64::new(0),
            process_tree_terminations: AtomicU64::new(0),
            process_tree_forced_terminations: AtomicU64::new(0),
            process_tree_termination_elapsed_ns: AtomicU64::new(0),
            sandboxes_created: AtomicU64::new(0),
            sandboxes_reused: AtomicU64::new(0),
            sandboxes_poisoned: AtomicU64::new(0),
            sandboxes_deleted: AtomicU64::new(0),
            artifact_tree_walks: AtomicU64::new(0),
            artifact_tree_walk_elapsed_ns: AtomicU64::new(0),
            evidence_cache_lookups: AtomicU64::new(0),
            evidence_cache_hits: AtomicU64::new(0),
            evidence_cache_writes: AtomicU64::new(0),
            evidence_cache_elapsed_ns: AtomicU64::new(0),
            journal_writes: AtomicU64::new(0),
            journal_bytes_written: AtomicU64::new(0),
            journal_flushes: AtomicU64::new(0),
            journal_syncs: AtomicU64::new(0),
            journal_elapsed_ns: AtomicU64::new(0),
        }
    }

    fn snapshot(&self) -> CompilerAcquisitionSnapshot {
        CompilerAcquisitionSnapshot {
            plan_identity: self.plan_identity.get().cloned(),
            plans: self.plans.load(Ordering::Relaxed),
            plan_build_elapsed_ns: self.plan_build_elapsed_ns.load(Ordering::Relaxed),
            packages: self.packages.load(Ordering::Relaxed),
            targets: self.targets.load(Ordering::Relaxed),
            features: self.features.load(Ordering::Relaxed),
            candidates: self.candidates.load(Ordering::Relaxed),
            views: self.views.load(Ordering::Relaxed),
            cargo_views: self.cargo_views.load(Ordering::Relaxed),
            cargo_elapsed_ns: self.cargo_elapsed_ns.load(Ordering::Relaxed),
            configured_process_slots: self.configured_process_slots.load(Ordering::Relaxed),
            configured_work_permits: self.configured_work_permits.load(Ordering::Relaxed),
            live_cargo_processes: self.live_cargo_processes.load(Ordering::Relaxed),
            max_live_cargo_processes: self.max_live_cargo_processes.load(Ordering::Relaxed),
            max_nonwaiting_cargo_views: self.max_nonwaiting_cargo_views.load(Ordering::Relaxed),
            work_permit_start_waits: self.work_permit_start_waits.load(Ordering::Relaxed),
            work_permit_yields: self.work_permit_yields.load(Ordering::Relaxed),
            work_permit_resumes: self.work_permit_resumes.load(Ordering::Relaxed),
            compiler_actions: self.compiler_actions.load(Ordering::Relaxed),
            cargo_messages_read: self.cargo_messages_read.load(Ordering::Relaxed),
            stdout_bytes_read: self.stdout_bytes_read.load(Ordering::Relaxed),
            stderr_bytes_read: self.stderr_bytes_read.load(Ordering::Relaxed),
            stdout_bytes_retained: self.stdout_bytes_retained.load(Ordering::Relaxed),
            stderr_bytes_retained: self.stderr_bytes_retained.load(Ordering::Relaxed),
            output_retention_high_water_bytes: self.output_retention_high_water_bytes.load(Ordering::Relaxed),
            process_tree_terminations: self.process_tree_terminations.load(Ordering::Relaxed),
            process_tree_forced_terminations: self.process_tree_forced_terminations.load(Ordering::Relaxed),
            process_tree_termination_elapsed_ns: self.process_tree_termination_elapsed_ns.load(Ordering::Relaxed),
            sandboxes_created: self.sandboxes_created.load(Ordering::Relaxed),
            sandboxes_reused: self.sandboxes_reused.load(Ordering::Relaxed),
            sandboxes_poisoned: self.sandboxes_poisoned.load(Ordering::Relaxed),
            sandboxes_deleted: self.sandboxes_deleted.load(Ordering::Relaxed),
            artifact_tree_walks: self.artifact_tree_walks.load(Ordering::Relaxed),
            artifact_tree_walk_elapsed_ns: self.artifact_tree_walk_elapsed_ns.load(Ordering::Relaxed),
            evidence_cache_lookups: self.evidence_cache_lookups.load(Ordering::Relaxed),
            evidence_cache_hits: self.evidence_cache_hits.load(Ordering::Relaxed),
            evidence_cache_writes: self.evidence_cache_writes.load(Ordering::Relaxed),
            evidence_cache_elapsed_ns: self.evidence_cache_elapsed_ns.load(Ordering::Relaxed),
            journal_writes: self.journal_writes.load(Ordering::Relaxed),
            journal_bytes_written: self.journal_bytes_written.load(Ordering::Relaxed),
            journal_flushes: self.journal_flushes.load(Ordering::Relaxed),
            journal_syncs: self.journal_syncs.load(Ordering::Relaxed),
            journal_elapsed_ns: self.journal_elapsed_ns.load(Ordering::Relaxed),
        }
    }
}

struct PhaseCounters {
    cli_pre_context_preparation: PhaseCounter,
    workspace_capture_cargo_metadata: PhaseCounter,
    sysroot_fingerprinting: PhaseCounter,
}

impl PhaseCounters {
    const fn new() -> Self {
        Self {
            cli_pre_context_preparation: PhaseCounter::new(),
            workspace_capture_cargo_metadata: PhaseCounter::new(),
            sysroot_fingerprinting: PhaseCounter::new(),
        }
    }

    fn snapshot(&self) -> PhaseSnapshots {
        PhaseSnapshots {
            cli_pre_context_preparation: self.cli_pre_context_preparation.snapshot(),
            workspace_capture_cargo_metadata: self.workspace_capture_cargo_metadata.snapshot(),
            sysroot_fingerprinting: self.sysroot_fingerprinting.snapshot(),
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
    sysroot_fingerprinting: PhaseSnapshot,
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
    compiler_acquisition: CompilerAcquisitionSnapshot,
}

#[derive(Serialize)]
struct CompilerAcquisitionSnapshot {
    plan_identity: Option<String>,
    plans: u64,
    plan_build_elapsed_ns: u64,
    packages: u64,
    targets: u64,
    features: u64,
    candidates: u64,
    views: u64,
    cargo_views: u64,
    cargo_elapsed_ns: u64,
    configured_process_slots: u64,
    configured_work_permits: u64,
    live_cargo_processes: u64,
    max_live_cargo_processes: u64,
    max_nonwaiting_cargo_views: u64,
    work_permit_start_waits: u64,
    work_permit_yields: u64,
    work_permit_resumes: u64,
    compiler_actions: u64,
    cargo_messages_read: u64,
    stdout_bytes_read: u64,
    stderr_bytes_read: u64,
    stdout_bytes_retained: u64,
    stderr_bytes_retained: u64,
    output_retention_high_water_bytes: u64,
    process_tree_terminations: u64,
    process_tree_forced_terminations: u64,
    process_tree_termination_elapsed_ns: u64,
    sandboxes_created: u64,
    sandboxes_reused: u64,
    sandboxes_poisoned: u64,
    sandboxes_deleted: u64,
    artifact_tree_walks: u64,
    artifact_tree_walk_elapsed_ns: u64,
    evidence_cache_lookups: u64,
    evidence_cache_hits: u64,
    evidence_cache_writes: u64,
    evidence_cache_elapsed_ns: u64,
    journal_writes: u64,
    journal_bytes_written: u64,
    journal_flushes: u64,
    journal_syncs: u64,
    journal_elapsed_ns: u64,
}

#[derive(Clone, Copy)]
enum DiagnosticPhase {
    SysrootFingerprinting,
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
            DiagnosticPhase::SysrootFingerprinting => &counters.phases.sysroot_fingerprinting,
        };
        counter.record(started);
    }
}

/// Active diagnostic session for one cargo-rail process.
#[derive(Debug)]
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
    duration_value_ns(started.elapsed())
}

fn duration_value_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn optional_duration_ns(started: Option<Instant>) -> u64 {
    started.map_or(0, duration_ns)
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

pub(crate) fn sysroot_fingerprinting_phase() -> DiagnosticPhaseGuard {
    DiagnosticPhaseGuard::start(DiagnosticPhase::SysrootFingerprinting)
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
        counters.snapshot_id.get_or_init(|| snapshot_id);
    }
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

pub(crate) fn compiler_acquisition_timer() -> Option<Instant> {
    COUNTERS.get().map(|_| Instant::now())
}

pub(crate) fn record_compiler_acquisition_execution_policy(process_slots: usize, work_permits: usize) {
    if let Some(counters) = COUNTERS.get() {
        let acquisition = &counters.compiler_acquisition;
        acquisition
            .configured_process_slots
            .fetch_max(amount(process_slots), Ordering::Relaxed);
        acquisition
            .configured_work_permits
            .fetch_max(amount(work_permits), Ordering::Relaxed);
    }
}

/// RAII ownership for one live Cargo process measured at the spawn boundary.
pub(crate) struct CompilerAcquisitionProcessGuard {
    measured: bool,
}

pub(crate) fn compiler_acquisition_process_started(
    counts_as_nonwaiting_without_broker: bool,
) -> CompilerAcquisitionProcessGuard {
    let measured = if let Some(counters) = COUNTERS.get() {
        let acquisition = &counters.compiler_acquisition;
        let live = acquisition.live_cargo_processes.fetch_add(1, Ordering::Relaxed) + 1;
        acquisition.max_live_cargo_processes.fetch_max(live, Ordering::Relaxed);
        if counts_as_nonwaiting_without_broker {
            acquisition
                .max_nonwaiting_cargo_views
                .fetch_max(live, Ordering::Relaxed);
        }
        true
    } else {
        false
    };
    CompilerAcquisitionProcessGuard { measured }
}

impl Drop for CompilerAcquisitionProcessGuard {
    fn drop(&mut self) {
        if self.measured
            && let Some(counters) = COUNTERS.get()
        {
            let previous = counters
                .compiler_acquisition
                .live_cargo_processes
                .fetch_sub(1, Ordering::Relaxed);
            debug_assert!(previous > 0, "compiler acquisition live-process counter underflowed");
        }
    }
}

pub(crate) fn record_compiler_acquisition_nonwaiting_views(views: usize) {
    if let Some(counters) = COUNTERS.get() {
        counters
            .compiler_acquisition
            .max_nonwaiting_cargo_views
            .fetch_max(amount(views), Ordering::Relaxed);
    }
}

pub(crate) fn record_compiler_acquisition_work_permit_wait() {
    add(|counters| &counters.compiler_acquisition.work_permit_start_waits, 1);
}

pub(crate) fn record_compiler_acquisition_work_permit_yield() {
    add(|counters| &counters.compiler_acquisition.work_permit_yields, 1);
}

pub(crate) fn record_compiler_acquisition_work_permit_resume() {
    add(|counters| &counters.compiler_acquisition.work_permit_resumes, 1);
}

pub(crate) fn record_compiler_acquisition_plan(
    started: Option<Instant>,
    identity: &str,
    packages: usize,
    targets: usize,
    features: usize,
    candidates: usize,
    views: usize,
) {
    let Some(counters) = COUNTERS.get() else {
        return;
    };
    let acquisition = &counters.compiler_acquisition;
    acquisition.plan_identity.get_or_init(|| identity.to_string());
    acquisition.plans.fetch_add(1, Ordering::Relaxed);
    acquisition
        .plan_build_elapsed_ns
        .fetch_add(optional_duration_ns(started), Ordering::Relaxed);
    acquisition.packages.fetch_add(amount(packages), Ordering::Relaxed);
    acquisition.targets.fetch_add(amount(targets), Ordering::Relaxed);
    acquisition.features.fetch_add(amount(features), Ordering::Relaxed);
    acquisition.candidates.fetch_add(amount(candidates), Ordering::Relaxed);
    acquisition.views.fetch_add(amount(views), Ordering::Relaxed);
}

pub(crate) fn record_compiler_acquisition_cargo_view(
    started: Option<Instant>,
    cargo_messages: usize,
    stdout_bytes_read: u64,
    stderr_bytes_read: u64,
    stdout_bytes_retained: usize,
    stderr_bytes_retained: usize,
) {
    let Some(counters) = COUNTERS.get() else {
        return;
    };
    let acquisition = &counters.compiler_acquisition;
    acquisition.cargo_views.fetch_add(1, Ordering::Relaxed);
    acquisition
        .cargo_elapsed_ns
        .fetch_add(optional_duration_ns(started), Ordering::Relaxed);
    acquisition
        .cargo_messages_read
        .fetch_add(amount(cargo_messages), Ordering::Relaxed);
    acquisition
        .stdout_bytes_read
        .fetch_add(stdout_bytes_read, Ordering::Relaxed);
    acquisition
        .stderr_bytes_read
        .fetch_add(stderr_bytes_read, Ordering::Relaxed);
    acquisition
        .stdout_bytes_retained
        .fetch_add(amount(stdout_bytes_retained), Ordering::Relaxed);
    acquisition
        .stderr_bytes_retained
        .fetch_add(amount(stderr_bytes_retained), Ordering::Relaxed);
    acquisition.output_retention_high_water_bytes.fetch_max(
        amount(stdout_bytes_retained.saturating_add(stderr_bytes_retained)),
        Ordering::Relaxed,
    );
}

pub(crate) fn record_compiler_acquisition_process_termination(forced: bool, elapsed: Duration) {
    if let Some(counters) = COUNTERS.get() {
        let acquisition = &counters.compiler_acquisition;
        acquisition.process_tree_terminations.fetch_add(1, Ordering::Relaxed);
        if forced {
            acquisition
                .process_tree_forced_terminations
                .fetch_add(1, Ordering::Relaxed);
        }
        acquisition
            .process_tree_termination_elapsed_ns
            .fetch_add(duration_value_ns(elapsed), Ordering::Relaxed);
    }
}

pub(crate) fn record_compiler_acquisition_sandbox_create() {
    add(|counters| &counters.compiler_acquisition.sandboxes_created, 1);
}

pub(crate) fn record_compiler_acquisition_sandbox_reuse() {
    add(|counters| &counters.compiler_acquisition.sandboxes_reused, 1);
}

pub(crate) fn record_compiler_acquisition_sandbox_poison() {
    add(|counters| &counters.compiler_acquisition.sandboxes_poisoned, 1);
}

pub(crate) fn record_compiler_acquisition_sandbox_delete() {
    add(|counters| &counters.compiler_acquisition.sandboxes_deleted, 1);
}

pub(crate) fn record_compiler_acquisition_actions(actions: usize) {
    if let Some(counters) = COUNTERS.get() {
        counters
            .compiler_acquisition
            .compiler_actions
            .fetch_add(amount(actions), Ordering::Relaxed);
    }
}

pub(crate) fn record_compiler_acquisition_artifact_tree_walk(started: Option<Instant>) {
    if let Some(counters) = COUNTERS.get() {
        let acquisition = &counters.compiler_acquisition;
        acquisition.artifact_tree_walks.fetch_add(1, Ordering::Relaxed);
        acquisition
            .artifact_tree_walk_elapsed_ns
            .fetch_add(optional_duration_ns(started), Ordering::Relaxed);
    }
}

pub(crate) fn record_compiler_acquisition_cache_lookup(started: Option<Instant>, hit: bool) {
    if let Some(counters) = COUNTERS.get() {
        let acquisition = &counters.compiler_acquisition;
        acquisition.evidence_cache_lookups.fetch_add(1, Ordering::Relaxed);
        if hit {
            acquisition.evidence_cache_hits.fetch_add(1, Ordering::Relaxed);
        }
        acquisition
            .evidence_cache_elapsed_ns
            .fetch_add(optional_duration_ns(started), Ordering::Relaxed);
    }
}

pub(crate) fn record_compiler_acquisition_cache_write(started: Option<Instant>) {
    if let Some(counters) = COUNTERS.get() {
        let acquisition = &counters.compiler_acquisition;
        acquisition.evidence_cache_writes.fetch_add(1, Ordering::Relaxed);
        acquisition
            .evidence_cache_elapsed_ns
            .fetch_add(optional_duration_ns(started), Ordering::Relaxed);
    }
}

pub(crate) fn record_compiler_acquisition_journal_write(
    started: Option<Instant>,
    bytes: usize,
    flushed: bool,
    synced: bool,
) {
    if let Some(counters) = COUNTERS.get() {
        let acquisition = &counters.compiler_acquisition;
        acquisition.journal_writes.fetch_add(1, Ordering::Relaxed);
        acquisition
            .journal_bytes_written
            .fetch_add(amount(bytes), Ordering::Relaxed);
        if flushed {
            acquisition.journal_flushes.fetch_add(1, Ordering::Relaxed);
        }
        if synced {
            acquisition.journal_syncs.fetch_add(1, Ordering::Relaxed);
        }
        acquisition
            .journal_elapsed_ns
            .fetch_add(optional_duration_ns(started), Ordering::Relaxed);
    }
}

#[cfg(any(unix, windows, test))]
pub(crate) fn record_cas_restore(bytes: u64) {
    add(|counters| &counters.cas_bytes_restored, bytes);
}
