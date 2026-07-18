//! Explicit, out-of-band diagnostic counters for performance workloads.

use std::fs::File;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use crate::error::{RailError, RailResult};

const SCHEMA_VERSION: u32 = 2;

static COUNTERS: OnceLock<Counters> = OnceLock::new();

struct Counters {
  snapshot_id: OnceLock<String>,
  cargo_metadata_loads: AtomicU64,
  cargo_metadata_cache_hits: AtomicU64,
  target_view_loads: AtomicU64,
  hash_operations: AtomicU64,
  hash_input_bytes: AtomicU64,
  hashed_file_bytes_read: AtomicU64,
  git_subprocesses: AtomicU64,
  graph_traversals: AtomicU64,
  graph_node_visits: AtomicU64,
  graph_edge_visits: AtomicU64,
}

impl Counters {
  const fn new() -> Self {
    Self {
      snapshot_id: OnceLock::new(),
      cargo_metadata_loads: AtomicU64::new(0),
      cargo_metadata_cache_hits: AtomicU64::new(0),
      target_view_loads: AtomicU64::new(0),
      hash_operations: AtomicU64::new(0),
      hash_input_bytes: AtomicU64::new(0),
      hashed_file_bytes_read: AtomicU64::new(0),
      git_subprocesses: AtomicU64::new(0),
      graph_traversals: AtomicU64::new(0),
      graph_node_visits: AtomicU64::new(0),
      graph_edge_visits: AtomicU64::new(0),
    }
  }

  fn snapshot(&self) -> CounterSnapshot {
    CounterSnapshot {
      schema_version: SCHEMA_VERSION,
      snapshot_id: self.snapshot_id.get().cloned(),
      cargo_metadata_loads: self.cargo_metadata_loads.load(Ordering::Relaxed),
      cargo_metadata_cache_hits: self.cargo_metadata_cache_hits.load(Ordering::Relaxed),
      target_view_loads: self.target_view_loads.load(Ordering::Relaxed),
      hash_operations: self.hash_operations.load(Ordering::Relaxed),
      hash_input_bytes: self.hash_input_bytes.load(Ordering::Relaxed),
      hashed_file_bytes_read: self.hashed_file_bytes_read.load(Ordering::Relaxed),
      git_subprocesses: self.git_subprocesses.load(Ordering::Relaxed),
      graph_traversals: self.graph_traversals.load(Ordering::Relaxed),
      graph_node_visits: self.graph_node_visits.load(Ordering::Relaxed),
      graph_edge_visits: self.graph_edge_visits.load(Ordering::Relaxed),
    }
  }
}

#[derive(Serialize)]
struct CounterSnapshot {
  schema_version: u32,
  snapshot_id: Option<String>,
  cargo_metadata_loads: u64,
  cargo_metadata_cache_hits: u64,
  target_view_loads: u64,
  hash_operations: u64,
  hash_input_bytes: u64,
  hashed_file_bytes_read: u64,
  git_subprocesses: u64,
  graph_traversals: u64,
  graph_node_visits: u64,
  graph_edge_visits: u64,
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

fn add(counter: fn(&Counters) -> &AtomicU64, value: u64) {
  if let Some(counters) = COUNTERS.get() {
    counter(counters).fetch_add(value, Ordering::Relaxed);
  }
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

pub(crate) fn record_graph_traversal(node_visits: usize, edge_visits: usize) {
  add(|counters| &counters.graph_traversals, 1);
  add(|counters| &counters.graph_node_visits, amount(node_visits));
  add(|counters| &counters.graph_edge_visits, amount(edge_visits));
}
