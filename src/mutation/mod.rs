//! Deterministic mutation plan/apply framework shared by mutating commands.
//!
//! This module provides:
//! - A shared mutation plan schema
//! - Pre-apply drift checks
//! - Immutable execution receipts

use crate::config::RailConfig;
use crate::error::{RailError, RailResult};
use crate::workspace::WorkspaceContext;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Version for the mutation contract emitted by plan/apply flows.
pub const MUTATION_CONTRACT_VERSION: u32 = 1;

/// Shared deterministic mutation plan schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationPlan {
  /// Schema version for this plan contract.
  pub contract_version: u32,
  /// Stable operation identifier for this specific plan.
  pub operation_id: String,
  /// Deterministic fingerprint of operation inputs.
  pub inputs_fingerprint: String,
  /// Resolved git refs used to build and validate the plan.
  pub resolved_refs: MutationResolvedRefs,
  /// Ordered action list the operation intends to perform.
  pub actions: Vec<MutationAction>,
  /// Identified risks for this operation.
  pub risks: Vec<MutationRisk>,
  /// Explainability trace entries for planning and execution.
  pub trace: Vec<MutationTrace>,
  /// Snapshot used for pre-apply drift validation.
  pub pre_apply: MutationPreApplyChecks,
}

/// Resolved refs for a plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationResolvedRefs {
  /// HEAD SHA when the plan was built.
  pub git_head: String,
  /// Current branch name (or `HEAD` when detached).
  pub git_branch: String,
}

/// Canonical action entry for mutation plans.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationAction {
  /// Stable action code.
  pub code: String,
  /// Human-readable action target.
  pub target: String,
  /// Optional details.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub detail: Option<String>,
}

impl MutationAction {
  /// Construct an action.
  pub fn new(code: impl Into<String>, target: impl Into<String>, detail: Option<String>) -> Self {
    Self {
      code: code.into(),
      target: target.into(),
      detail,
    }
  }
}

/// Risk entry for mutation plans.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationRisk {
  /// Stable risk code.
  pub code: String,
  /// Severity (`low`, `medium`, `high`).
  pub severity: String,
  /// Human-readable message.
  pub message: String,
}

impl MutationRisk {
  /// Construct a risk.
  pub fn new(code: impl Into<String>, severity: impl Into<String>, message: impl Into<String>) -> Self {
    Self {
      code: code.into(),
      severity: severity.into(),
      message: message.into(),
    }
  }
}

/// Trace entry for plan/apply explainability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationTrace {
  /// Stable trace code.
  pub code: String,
  /// Human-readable trace message.
  pub message: String,
}

impl MutationTrace {
  /// Construct a trace entry.
  pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
    Self {
      code: code.into(),
      message: message.into(),
    }
  }
}

/// Snapshot of preconditions that must match before applying mutations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MutationPreApplyChecks {
  /// HEAD SHA.
  pub git_head: String,
  /// `rail.toml` fingerprint.
  pub config_fingerprint: String,
  /// `rust-toolchain*` fingerprint.
  pub toolchain_fingerprint: String,
  /// `Cargo.lock` fingerprint.
  pub lock_fingerprint: String,
  /// Cargo metadata cache fingerprint.
  pub metadata_fingerprint: String,
}

/// Immutable receipt for plan/apply execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationReceipt {
  /// Schema version for this receipt.
  pub contract_version: u32,
  /// Operation ID tied to the source plan.
  pub operation_id: String,
  /// Operation name (for humans).
  pub operation: String,
  /// Phase (`plan` or `apply`).
  pub phase: String,
  /// Status (`planned`, `applied`, `failed`).
  pub status: String,
  /// RFC3339 timestamp in UTC.
  pub timestamp_utc: String,
  /// Plan payload.
  pub plan: MutationPlan,
  /// Execution trace.
  pub trace: Vec<MutationTrace>,
}

/// Build a deterministic mutation plan for an operation.
pub fn build_plan(
  ctx: &WorkspaceContext,
  operation: &str,
  actions: Vec<MutationAction>,
  risks: Vec<MutationRisk>,
  trace: Vec<MutationTrace>,
) -> RailResult<MutationPlan> {
  let resolved_refs = MutationResolvedRefs {
    git_head: ctx.git.git().head_commit()?,
    git_branch: ctx.git.current_branch()?,
  };
  let pre_apply = capture_pre_apply_checks(ctx)?;

  let fingerprint_bytes = serde_json::to_vec(&serde_json::json!({
    "operation": operation,
    "resolved_refs": &resolved_refs,
    "pre_apply": &pre_apply,
    "actions": &actions,
    "risks": &risks,
  }))
  .map_err(|e| RailError::message(format!("failed to serialize mutation inputs: {}", e)))?;

  let inputs_fingerprint = format!("fnv1a64:{:016x}", fnv1a64(&fingerprint_bytes));
  let operation_id = build_operation_id(operation, &inputs_fingerprint);

  Ok(MutationPlan {
    contract_version: MUTATION_CONTRACT_VERSION,
    operation_id,
    inputs_fingerprint,
    resolved_refs,
    actions,
    risks,
    trace,
    pre_apply,
  })
}

/// Fail-closed validation before apply.
pub fn validate_pre_apply(ctx: &WorkspaceContext, plan: &MutationPlan) -> RailResult<()> {
  let current = capture_pre_apply_checks(ctx)?;
  if current == plan.pre_apply {
    return Ok(());
  }

  let mut reasons = Vec::new();
  if current.git_head != plan.pre_apply.git_head {
    reasons.push(format!(
      "git_head changed (planned {}, current {})",
      plan.pre_apply.git_head, current.git_head
    ));
  }
  if current.config_fingerprint != plan.pre_apply.config_fingerprint {
    reasons.push(format!(
      "config fingerprint changed (planned {}, current {})",
      plan.pre_apply.config_fingerprint, current.config_fingerprint
    ));
  }
  if current.toolchain_fingerprint != plan.pre_apply.toolchain_fingerprint {
    reasons.push(format!(
      "toolchain fingerprint changed (planned {}, current {})",
      plan.pre_apply.toolchain_fingerprint, current.toolchain_fingerprint
    ));
  }
  if current.lock_fingerprint != plan.pre_apply.lock_fingerprint {
    reasons.push(format!(
      "lock fingerprint changed (planned {}, current {})",
      plan.pre_apply.lock_fingerprint, current.lock_fingerprint
    ));
  }
  if current.metadata_fingerprint != plan.pre_apply.metadata_fingerprint {
    reasons.push(format!(
      "metadata fingerprint changed (planned {}, current {})",
      plan.pre_apply.metadata_fingerprint, current.metadata_fingerprint
    ));
  }

  Err(RailError::with_help(
    format!(
      "mutation drift detected for operation '{}': {}",
      plan.operation_id,
      reasons.join("; ")
    ),
    "regenerate the mutation plan and re-run apply".to_string(),
  ))
}

/// Write an immutable receipt file to `target/cargo-rail/receipts/`.
pub fn write_receipt(
  workspace_root: &Path,
  operation: &str,
  phase: &str,
  status: &str,
  plan: MutationPlan,
  trace: Vec<MutationTrace>,
) -> RailResult<PathBuf> {
  let receipt = MutationReceipt {
    contract_version: MUTATION_CONTRACT_VERSION,
    operation_id: plan.operation_id.clone(),
    operation: operation.to_string(),
    phase: phase.to_string(),
    status: status.to_string(),
    timestamp_utc: Utc::now().to_rfc3339(),
    plan,
    trace,
  };

  let dir = workspace_root.join("target").join("cargo-rail").join("receipts");
  fs::create_dir_all(&dir)?;

  let nonce = Utc::now().timestamp_nanos_opt().unwrap_or_default();
  let path = dir.join(format!(
    "{}-{}-{}-{}.json",
    sanitize_for_filename(operation),
    receipt.operation_id,
    sanitize_for_filename(phase),
    nonce
  ));
  let mut file = fs::OpenOptions::new()
    .create_new(true)
    .write(true)
    .open(&path)
    .map_err(|e| RailError::message(format!("failed to create receipt '{}': {}", path.display(), e)))?;

  let bytes = serde_json::to_vec_pretty(&receipt)
    .map_err(|e| RailError::message(format!("failed to serialize receipt: {}", e)))?;
  file
    .write_all(&bytes)
    .map_err(|e| RailError::message(format!("failed to write receipt '{}': {}", path.display(), e)))?;
  file
    .write_all(b"\n")
    .map_err(|e| RailError::message(format!("failed to finalize receipt '{}': {}", path.display(), e)))?;

  Ok(path)
}

/// Read a mutation plan from a JSON file.
///
/// Accepts either:
/// - a raw `MutationPlan` JSON object
/// - an envelope with `{ "mutation_plan": { ... } }`
pub fn read_plan_file(path: &Path) -> RailResult<MutationPlan> {
  let content =
    fs::read_to_string(path).map_err(|e| RailError::message(format!("failed to read '{}': {}", path.display(), e)))?;

  let value: serde_json::Value = serde_json::from_str(&content)
    .map_err(|e| RailError::message(format!("invalid mutation plan JSON '{}': {}", path.display(), e)))?;

  if let Some(inner) = value.get("mutation_plan") {
    return serde_json::from_value(inner.clone())
      .map_err(|e| RailError::message(format!("invalid mutation_plan in '{}': {}", path.display(), e)));
  }

  serde_json::from_value(value)
    .map_err(|e| RailError::message(format!("invalid mutation plan in '{}': {}", path.display(), e)))
}

fn build_operation_id(operation: &str, inputs_fingerprint: &str) -> String {
  let digest = inputs_fingerprint.rsplit(':').next().unwrap_or("unknown");
  let short = if digest.len() >= 12 { &digest[..12] } else { digest };
  format!("{}-{}", sanitize_for_filename(operation), short)
}

fn sanitize_for_filename(input: &str) -> String {
  input
    .chars()
    .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
    .collect::<String>()
    .trim_matches('-')
    .to_lowercase()
}

fn capture_pre_apply_checks(ctx: &WorkspaceContext) -> RailResult<MutationPreApplyChecks> {
  let workspace_root = ctx.workspace_root();
  Ok(MutationPreApplyChecks {
    git_head: ctx.git.git().head_commit()?,
    config_fingerprint: config_fingerprint(workspace_root),
    toolchain_fingerprint: toolchain_fingerprint(workspace_root),
    lock_fingerprint: file_fingerprint_or_none(&workspace_root.join("Cargo.lock")),
    metadata_fingerprint: file_fingerprint_or_none(&workspace_root.join("target/cargo-rail/metadata.json")),
  })
}

fn config_fingerprint(workspace_root: &Path) -> String {
  if let Some(config_path) = RailConfig::find_config_path(workspace_root) {
    return file_fingerprint_or_none(&config_path);
  }
  "none".to_string()
}

fn toolchain_fingerprint(workspace_root: &Path) -> String {
  let candidates = [
    workspace_root.join("rust-toolchain.toml"),
    workspace_root.join("rust-toolchain"),
  ];

  for candidate in candidates {
    if candidate.exists() {
      return file_fingerprint_or_none(&candidate);
    }
  }

  "none".to_string()
}

fn file_fingerprint_or_none(path: &Path) -> String {
  match fs::read(path) {
    Ok(bytes) => format!("fnv1a64:{:016x}", fnv1a64(&bytes)),
    Err(_) => "none".to_string(),
  }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
  const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
  const FNV_PRIME: u64 = 0x100000001b3;

  let mut hash = FNV_OFFSET_BASIS;
  for byte in bytes {
    hash ^= *byte as u64;
    hash = hash.wrapping_mul(FNV_PRIME);
  }
  hash
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_sanitize_for_filename_replaces_non_alnum() {
    assert_eq!(sanitize_for_filename("release/run v1"), "release-run-v1");
  }

  #[test]
  fn test_operation_id_is_stable() {
    let op = build_operation_id("unify apply", "fnv1a64:0123456789abcdef");
    assert_eq!(op, "unify-apply-0123456789ab");
  }
}
