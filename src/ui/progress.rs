//! Progress indicators for long-running operations
//!
//! Uses `linya` for allocation-free, concurrency-optimized progress bars
//! Perfect for monorepo operations with multiple concurrent tasks

use linya::{Bar, Progress};
use std::sync::{Arc, Mutex};

/// Progress bar wrapper for commits processing
pub struct CommitProgress {
  progress: Progress,
  bar: Bar,
}

impl CommitProgress {
  /// Create a new progress bar for processing commits
  pub fn new(total: usize, label: impl Into<String>) -> Self {
    let mut progress = Progress::new();
    let bar = progress.bar(total, label.into());
    Self { progress, bar }
  }

  /// Increment progress by 1
  pub fn inc(&mut self) {
    self.progress.inc_and_draw(&self.bar, 1);
  }

  /// Set progress to a specific value
  #[allow(dead_code)]
  pub fn set(&mut self, pos: usize) {
    self.progress.set_and_draw(&self.bar, pos);
  }

  /// Get the bar handle
  #[allow(dead_code)]
  pub fn bar(&self) -> &Bar {
    &self.bar
  }
}

/// Progress bar wrapper for file operations
pub struct FileProgress {
  progress: Progress,
  bar: Bar,
}

impl FileProgress {
  /// Create a new progress bar for file transformations
  pub fn new(total: usize, label: impl Into<String>) -> Self {
    let mut progress = Progress::new();
    let bar = progress.bar(total, label.into());
    Self { progress, bar }
  }

  /// Increment progress by 1
  pub fn inc(&mut self) {
    self.progress.inc_and_draw(&self.bar, 1);
  }

  /// Set progress to a specific value
  #[allow(dead_code)]
  pub fn set(&mut self, pos: usize) {
    self.progress.set_and_draw(&self.bar, pos);
  }

  /// Get the bar handle
  #[allow(dead_code)]
  pub fn bar(&self) -> &Bar {
    &self.bar
  }
}

/// Multi-bar progress for parallel operations
/// Thread-safe wrapper for concurrent progress tracking
#[derive(Clone)]
pub struct MultiProgress {
  progress: Arc<Mutex<Progress>>,
}

impl MultiProgress {
  /// Create a new multi-progress container
  pub fn new() -> Self {
    Self {
      progress: Arc::new(Mutex::new(Progress::new())),
    }
  }

  /// Add a new bar with a label and total
  pub fn add_bar(&self, total: usize, label: impl Into<String>) -> Bar {
    let mut progress = self.progress.lock().unwrap();
    progress.bar(total, label.into())
  }

  /// Increment a bar (thread-safe)
  pub fn inc(&self, bar: &Bar) {
    let mut progress = self.progress.lock().unwrap();
    progress.inc_and_draw(bar, 1);
  }
}

impl Default for MultiProgress {
  fn default() -> Self {
    Self::new()
  }
}
