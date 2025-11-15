//! Plan-based operations for idempotent, reviewable, and cacheable workflows
//!
//! This module provides a unified abstraction for all cargo-rail operations.
//!
//! Note: Some methods in this module are part of the public API for future features
//! (e.g., from_json for CI deserialization, len/is_empty for standard collection API).

#![allow(dead_code)]
//! Every mutating operation produces a `Plan` before execution, enabling:
//!
//! - **Dry-run mode**: Show what will happen without actually doing it
//! - **Idempotency**: Same input → same plan → same result
//! - **Auditability**: Plans are JSON-serializable for logging/review
//! - **Caching**: Plans can be hashed and cached
//! - **Rollback**: Plans can be reversed (future)
//!
//! # Architecture
//!
//! ```text
//! Command (split, sync, etc.)
//!   ↓
//! Plan (what to do)
//!   ↓
//! Executor (apply the plan)
//!   ↓
//! Result
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! // Create a plan
//! let plan = SplitPlan::new("my-crate", config)?;
//!
//! // Show the plan (dry-run)
//! println!("{}", plan.to_human_readable());
//!
//! // Execute the plan
//! if apply {
//!   plan.execute()?;
//! }
//! ```

use crate::core::error::RailResult;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt;

/// Plan identifier (SHA256 hash of plan contents)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanId(String);

impl PlanId {
  /// Create a plan ID from plan contents
  pub fn from_contents(contents: &[u8]) -> Self {
    let mut hasher = Sha256::new();
    hasher.update(contents);
    let result = hasher.finalize();
    Self(format!("{:x}", result))
  }

  /// Get the short ID (first 12 characters)
  pub fn short(&self) -> &str {
    &self.0[..12.min(self.0.len())]
  }
}

impl fmt::Display for PlanId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}", self.short())
  }
}

/// Operation type that can be performed
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Operation {
  /// Initialize a git repository
  InitRepo { path: String },

  /// Clone a repository
  Clone { url: String, path: String },

  /// Create a commit
  CreateCommit { message: String, files: Vec<String> },

  /// Push to remote
  Push {
    remote: String,
    branch: String,
    force: bool,
  },

  /// Pull from remote
  Pull { remote: String, branch: String },

  /// Transform a file
  Transform { path: String, transform_type: String },

  /// Copy file(s)
  Copy { from: String, to: String },

  /// Create branch
  CreateBranch { name: String, from: String },

  /// Checkout branch
  Checkout { branch: String },

  /// Merge branches
  Merge {
    from: String,
    into: String,
    strategy: String,
  },

  /// Update git-notes
  UpdateNotes {
    notes_ref: String,
    commit: String,
    note_content: String,
  },

  /// Create PR branch
  CreatePrBranch {
    name: String,
    base: String,
    message: String,
  },

  /// Execute a split workflow
  /// This is a high-level operation that encapsulates the full split process
  ExecuteSplit {
    crate_name: String,
    crate_paths: Vec<String>,
    mode: String,
    target_repo_path: String,
    branch: String,
    remote_url: Option<String>,
  },

  /// Execute a sync workflow
  /// This is a high-level operation that encapsulates the full sync process
  ExecuteSync {
    crate_name: String,
    crate_paths: Vec<String>,
    mode: String,
    target_repo_path: String,
    branch: String,
    remote_url: String,
    direction: String,
    conflict_strategy: String,
  },
}

/// Plan metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanMetadata {
  /// Plan ID (content hash)
  pub id: PlanId,

  /// What operation this plan represents
  pub operation_type: OperationType,

  /// Crate/package name (if applicable)
  pub crate_name: Option<String>,

  /// Estimated time to execute (in seconds)
  pub estimated_duration: Option<u64>,

  /// Whether this plan will make destructive changes
  pub is_destructive: bool,

  /// Git-notes trailers to add to commits
  pub commit_trailers: HashMap<String, String>,
}

/// Type of operation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationType {
  Split,
  Sync,
  Release,
  Init,
}

impl fmt::Display for OperationType {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      OperationType::Split => write!(f, "split"),
      OperationType::Sync => write!(f, "sync"),
      OperationType::Release => write!(f, "release"),
      OperationType::Init => write!(f, "init"),
    }
  }
}

/// A plan represents a sequence of operations to perform
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
  /// Plan metadata
  pub metadata: PlanMetadata,

  /// Operations to perform (in order)
  pub operations: Vec<Operation>,

  /// File checksums (for verification)
  #[serde(skip_serializing_if = "HashMap::is_empty", default)]
  pub checksums: HashMap<String, String>,

  /// Human-readable summary
  pub summary: String,
}

impl Plan {
  /// Create a new plan
  pub fn new(operation_type: OperationType, crate_name: Option<String>) -> Self {
    let operations = Vec::new();
    let summary = String::new();

    // Compute plan ID (will be empty hash initially)
    let id = PlanId::from_contents(&[]);

    Self {
      metadata: PlanMetadata {
        id,
        operation_type,
        crate_name,
        estimated_duration: None,
        is_destructive: false,
        commit_trailers: HashMap::new(),
      },
      operations,
      checksums: HashMap::new(),
      summary,
    }
  }

  /// Add an operation to the plan
  pub fn add_operation(&mut self, operation: Operation) {
    self.operations.push(operation);
    self.recompute_id();
  }

  /// Add multiple operations (for batch operations)
  pub fn add_operations(&mut self, operations: Vec<Operation>) {
    self.operations.extend(operations);
    self.recompute_id();
  }

  /// Set the summary
  pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
    self.summary = summary.into();
    self
  }

  /// Mark as destructive
  pub fn mark_destructive(mut self) -> Self {
    self.metadata.is_destructive = true;
    self
  }

  /// Add a commit trailer
  pub fn add_trailer(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
    self.metadata.commit_trailers.insert(key.into(), value.into());
    self
  }

  /// Recompute plan ID based on current contents
  fn recompute_id(&mut self) {
    // Serialize plan (without ID) to compute hash
    let json = serde_json::to_vec(&self.operations).unwrap_or_default();
    self.metadata.id = PlanId::from_contents(&json);
  }

  /// Serialize to JSON
  pub fn to_json(&self) -> RailResult<String> {
    Ok(serde_json::to_string_pretty(self)?)
  }

  /// Deserialize from JSON
  pub fn from_json(json: &str) -> RailResult<Self> {
    Ok(serde_json::from_str(json)?)
  }

  /// Get human-readable representation
  pub fn to_human_readable(&self) -> String {
    let mut output = String::new();

    output.push_str(&format!(
      "📋 Plan: {} ({})\n",
      self.metadata.operation_type, self.metadata.id
    ));

    if let Some(ref crate_name) = self.metadata.crate_name {
      output.push_str(&format!("   Crate: {}\n", crate_name));
    }

    if !self.summary.is_empty() {
      output.push_str(&format!("\n{}\n", self.summary));
    }

    output.push_str(&format!("\n   Operations ({}):\n", self.operations.len()));

    for (i, op) in self.operations.iter().enumerate() {
      output.push_str(&format!("   {}. {}\n", i + 1, operation_to_string(op)));
    }

    if self.metadata.is_destructive {
      output.push_str("\n⚠️  NOTE: This operation will modify the target repository\n");
      output.push_str("   (Pushes to remote - ensure target is empty or has been backed up)\n");
    }

    if let Some(duration) = self.metadata.estimated_duration {
      output.push_str(&format!("\n   Estimated duration: {}s\n", duration));
    }

    output
  }

  /// Get number of operations
  pub fn len(&self) -> usize {
    self.operations.len()
  }

  /// Check if plan is empty
  pub fn is_empty(&self) -> bool {
    self.operations.is_empty()
  }
}

/// Convert operation to human-readable string
fn operation_to_string(op: &Operation) -> String {
  match op {
    Operation::InitRepo { path } => format!("Initialize repository at {}", path),
    Operation::Clone { url, path } => format!("Clone {} to {}", url, path),
    Operation::CreateCommit { message, files } => {
      format!("Create commit: {} ({} files)", message, files.len())
    }
    Operation::Push { remote, branch, force } => {
      if *force {
        format!("Force push to {}/{}", remote, branch)
      } else {
        format!("Push to {}/{}", remote, branch)
      }
    }
    Operation::Pull { remote, branch } => format!("Pull from {}/{}", remote, branch),
    Operation::Transform { path, transform_type } => format!("Transform {} ({})", path, transform_type),
    Operation::Copy { from, to } => format!("Copy {} → {}", from, to),
    Operation::CreateBranch { name, from } => format!("Create branch {} from {}", name, from),
    Operation::Checkout { branch } => format!("Checkout branch {}", branch),
    Operation::Merge { from, into, strategy } => format!("Merge {} into {} (strategy: {})", from, into, strategy),
    Operation::UpdateNotes { notes_ref, commit, .. } => format!("Update git-notes {} for {}", notes_ref, commit),
    Operation::CreatePrBranch { name, base, .. } => format!("Create PR branch {} from {}", name, base),
    Operation::ExecuteSplit {
      crate_name,
      mode,
      target_repo_path,
      ..
    } => format!("Split crate '{}' (mode: {}) to {}", crate_name, mode, target_repo_path),
    Operation::ExecuteSync {
      crate_name, direction, ..
    } => format!("Sync crate '{}' (direction: {})", crate_name, direction),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_plan_id_generation() {
    let mut plan = Plan::new(OperationType::Split, Some("test-crate".to_string()));
    let id1 = plan.metadata.id.clone();

    plan.add_operation(Operation::InitRepo {
      path: "/tmp/test".to_string(),
    });
    let id2 = plan.metadata.id.clone();

    // ID should change when operations change
    assert_ne!(id1, id2);
  }

  #[test]
  fn test_plan_serialization() {
    let mut plan = Plan::new(OperationType::Split, Some("test-crate".to_string()));
    plan.add_operation(Operation::InitRepo {
      path: "/tmp/test".to_string(),
    });

    let json = plan.to_json().unwrap();

    // Just verify we can serialize and deserialize
    let _deserialized = Plan::from_json(&json).unwrap();
  }

  #[test]
  fn test_human_readable_output() {
    let mut plan = Plan::new(OperationType::Sync, Some("my-crate".to_string()));
    plan.add_operation(Operation::Pull {
      remote: "origin".to_string(),
      branch: "main".to_string(),
    });
    plan.add_operation(Operation::Push {
      remote: "origin".to_string(),
      branch: "main".to_string(),
      force: false,
    });

    let output = plan.to_human_readable();
    assert!(output.contains("sync"), "Output should contain 'sync': {}", output);
    assert!(output.contains("my-crate"));
    assert!(output.contains("Pull from origin/main"));
    assert!(output.contains("Push to origin/main"));
  }
}
