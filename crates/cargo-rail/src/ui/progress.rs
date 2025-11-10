//! Progress indicators for long-running operations
//!
//! Uses `linya` for allocation-free, concurrency-optimized progress bars
//! Perfect for monorepo operations with multiple concurrent tasks

use linya::{Bar, Progress};

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
#[allow(dead_code)]
pub struct FileProgress {
  progress: Progress,
  bar: Bar,
}

#[allow(dead_code)]
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
  pub fn set(&mut self, pos: usize) {
    self.progress.set_and_draw(&self.bar, pos);
  }

  /// Get the bar handle
  pub fn bar(&self) -> &Bar {
    &self.bar
  }
}

/// Multi-bar progress for parallel operations
#[allow(dead_code)]
pub struct MultiProgress {
  progress: Progress,
}

#[allow(dead_code)]
impl MultiProgress {
  /// Create a new multi-progress container
  pub fn new() -> Self {
    Self {
      progress: Progress::new(),
    }
  }

  /// Add a new bar with a label and total
  pub fn add_bar(&mut self, total: usize, label: impl Into<String>) -> Bar {
    self.progress.bar(total, label.into())
  }

  /// Increment a bar
  pub fn inc(&mut self, bar: &Bar) {
    self.progress.inc_and_draw(bar, 1);
  }

  /// Set a bar to a specific value
  pub fn set(&mut self, bar: &Bar, pos: usize) {
    self.progress.set_and_draw(bar, pos);
  }
}

impl Default for MultiProgress {
  fn default() -> Self {
    Self::new()
  }
}
