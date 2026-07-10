//! Deterministic mutation plan/apply framework shared by mutating commands.
//!
//! This module provides:
//! - A shared mutation plan schema
//! - Pre-apply drift checks
//! - Immutable execution receipts

use crate::error::{RailError, RailResult};
use crate::git::SystemGit;
use crate::utils::{config_fingerprint, file_fingerprint, toolchain_fingerprint};
use crate::workspace::WorkspaceContext;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Version for the mutation contract emitted by plan/apply flows.
pub const MUTATION_CONTRACT_VERSION: u32 = 2;

/// Shared deterministic mutation plan schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationPlan {
  /// Schema version for this plan contract.
  pub contract_version: u32,
  /// Stable operation name such as `release`, `split`, `sync`, or `unify`.
  #[serde(default)]
  pub operation: String,
  /// Stable operation identifier for this specific plan.
  pub operation_id: String,
  /// Deterministic fingerprint of operation inputs.
  pub inputs_fingerprint: String,
  /// Resolved git refs used to build and validate the plan.
  pub resolved_refs: MutationResolvedRefs,
  /// Ordered action list the operation intends to perform.
  pub actions: Vec<MutationAction>,
  /// Explicit non-mutated filesystem inputs consumed by the operation.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub declared_inputs: Vec<MutationInput>,
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MutationAction {
  /// Stable action code.
  pub code: String,
  /// Human-readable action target.
  pub target: String,
  /// Optional details.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub detail: Option<String>,
  /// Exact machine-readable inputs for the action.
  #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
  pub payload: serde_json::Value,
  /// Files this action is allowed to create, update, or delete.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub expected_mutations: Vec<ExpectedMutation>,
}

impl MutationAction {
  /// Construct an action.
  pub fn new(code: impl Into<String>, target: impl Into<String>, detail: Option<String>) -> Self {
    Self {
      code: code.into(),
      target: target.into(),
      detail,
      payload: serde_json::Value::Null,
      expected_mutations: Vec::new(),
    }
  }

  /// Attach the complete structured action payload.
  pub fn with_payload(mut self, payload: serde_json::Value) -> Self {
    self.payload = payload;
    self
  }

  /// Declare the exact file mutations authorized for this action.
  pub fn with_mutations(mut self, mutations: Vec<ExpectedMutation>) -> Self {
    self.expected_mutations = mutations;
    self
  }
}

/// Expected effect of one planned filesystem mutation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MutationEffect {
  /// Create or replace file content.
  Write,
  /// Remove an existing file.
  Delete,
}

/// One path explicitly authorized by a mutation action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExpectedMutation {
  /// Git-worktree-relative path.
  pub path: PathBuf,
  /// Planned effect.
  pub effect: MutationEffect,
  /// Content fingerprint before apply, or `none` when absent.
  pub before_fingerprint: String,
}

/// One explicit filesystem input consumed without being staged or mutated.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MutationInput {
  /// Git-worktree-relative path.
  pub path: PathBuf,
  /// Content fingerprint at plan time.
  pub fingerprint: String,
}

impl MutationInput {
  /// Capture an input file relative to the Git worktree.
  pub fn capture(git: &SystemGit, worktree_root: &Path, path: PathBuf) -> RailResult<Self> {
    let absolute = if path.is_absolute() {
      path.clone()
    } else {
      worktree_root.join(&path)
    };
    Ok(Self {
      path,
      fingerprint: git_path_fingerprint(git, &absolute)?,
    })
  }
}

impl ExpectedMutation {
  /// Capture the current state of an expected workspace mutation.
  pub fn capture(workspace_root: &Path, path: PathBuf, effect: MutationEffect) -> Self {
    let absolute = if path.is_absolute() {
      path.clone()
    } else {
      workspace_root.join(&path)
    };
    Self {
      path,
      effect,
      before_fingerprint: file_fingerprint(&absolute),
    }
  }
}

/// Risk entry for mutation plans.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
  /// Fingerprint of every tracked or untracked worktree change.
  #[serde(default)]
  pub worktree_fingerprint: String,
  /// Exact changed path set used to produce `worktree_fingerprint`.
  #[serde(default)]
  pub changed_paths: Vec<PathBuf>,
}

/// Git object produced while applying a mutation plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MutationObject {
  /// Object kind such as `commit` or `tag`.
  pub kind: String,
  /// Human-readable object name.
  pub name: String,
  /// Full Git object ID.
  pub oid: String,
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
  /// Input snapshot verified immediately before apply.
  pub verified_inputs: MutationPreApplyChecks,
  /// Exact actions applied from the approved plan.
  pub applied_actions: Vec<MutationAction>,
  /// Git objects resulting from apply.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub resulting_objects: Vec<MutationObject>,
}

/// Build a deterministic mutation plan for an operation.
pub fn build_plan(
  ctx: &WorkspaceContext,
  operation: &str,
  actions: Vec<MutationAction>,
  risks: Vec<MutationRisk>,
  trace: Vec<MutationTrace>,
) -> RailResult<MutationPlan> {
  build_plan_with_inputs(ctx, operation, actions, Vec::new(), risks, trace)
}

/// Build a deterministic mutation plan with explicit non-mutated file inputs.
pub fn build_plan_with_inputs(
  ctx: &WorkspaceContext,
  operation: &str,
  actions: Vec<MutationAction>,
  declared_inputs: Vec<MutationInput>,
  risks: Vec<MutationRisk>,
  trace: Vec<MutationTrace>,
) -> RailResult<MutationPlan> {
  let resolved_refs = MutationResolvedRefs {
    git_head: ctx.git()?.git().head_commit()?,
    git_branch: ctx.git()?.current_branch()?,
  };
  let pre_apply = capture_pre_apply_checks(ctx)?;

  let inputs_fingerprint = mutation_fingerprint(
    ctx.git()?.git(),
    operation,
    &resolved_refs,
    &pre_apply,
    &actions,
    &declared_inputs,
    &risks,
  )?;
  let operation_id = build_operation_id(operation, &inputs_fingerprint);

  Ok(MutationPlan {
    contract_version: MUTATION_CONTRACT_VERSION,
    operation: operation.to_string(),
    operation_id,
    inputs_fingerprint,
    resolved_refs,
    actions,
    declared_inputs,
    risks,
    trace,
    pre_apply,
  })
}

/// Fail-closed validation before apply.
pub fn validate_pre_apply(ctx: &WorkspaceContext, plan: &MutationPlan) -> RailResult<()> {
  validate_pre_apply_with_allowed_paths(ctx, plan, &[])
}

/// Validate pre-apply state while excluding declared control files such as the plan itself.
pub fn validate_pre_apply_with_allowed_paths(
  ctx: &WorkspaceContext,
  plan: &MutationPlan,
  allowed_paths: &[PathBuf],
) -> RailResult<()> {
  if plan.contract_version != MUTATION_CONTRACT_VERSION {
    return Err(RailError::with_help(
      format!(
        "unsupported mutation contract version {} (expected {})",
        plan.contract_version, MUTATION_CONTRACT_VERSION
      ),
      "regenerate the mutation plan with this cargo-rail version",
    ));
  }
  let expected_fingerprint = mutation_fingerprint(
    ctx.git()?.git(),
    &plan.operation,
    &plan.resolved_refs,
    &plan.pre_apply,
    &plan.actions,
    &plan.declared_inputs,
    &plan.risks,
  )?;
  let expected_operation_id = build_operation_id(&plan.operation, &expected_fingerprint);
  if plan.inputs_fingerprint != expected_fingerprint || plan.operation_id != expected_operation_id {
    return Err(RailError::with_help(
      "mutation plan integrity check failed",
      "use the plan exactly as emitted; regenerate it if the file was edited",
    ));
  }
  let mut current = capture_pre_apply_checks(ctx)?;
  if !allowed_paths.is_empty() {
    let git_root = fs::canonicalize(&ctx.git()?.git().worktree_root)?;
    let mut allowed = BTreeSet::new();
    for path in allowed_paths {
      if path.is_absolute() {
        let canonical = fs::canonicalize(path)?;
        if let Ok(relative) = canonical.strip_prefix(&git_root) {
          allowed.insert(relative.to_path_buf());
        }
      } else {
        allowed.insert(path.clone());
      }
    }
    current.changed_paths.retain(|path| !allowed.contains(path));
    current.worktree_fingerprint = fingerprint_changed_paths(ctx.git()?.git(), &git_root, &current.changed_paths)?;
  }
  let git = ctx.git()?.git();
  let mut changed_inputs = Vec::new();
  for input in &plan.declared_inputs {
    let current_fingerprint = git_path_fingerprint(git, &git.worktree_root.join(&input.path))?;
    if current_fingerprint != input.fingerprint {
      changed_inputs.push(format!(
        "{} changed (planned {}, current {})",
        input.path.display(),
        input.fingerprint,
        current_fingerprint
      ));
    }
  }
  if current == plan.pre_apply && changed_inputs.is_empty() {
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
  if current.worktree_fingerprint != plan.pre_apply.worktree_fingerprint {
    reasons.push(format!(
      "worktree changed (planned {}, current {}; paths: {})",
      plan.pre_apply.worktree_fingerprint,
      current.worktree_fingerprint,
      display_paths(&current.changed_paths)
    ));
  }
  reasons.extend(changed_inputs);

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
  write_receipt_with_objects(workspace_root, operation, phase, status, plan, trace, Vec::new())
}

/// Write an immutable receipt including resulting Git objects.
pub fn write_receipt_with_objects(
  workspace_root: &Path,
  operation: &str,
  phase: &str,
  status: &str,
  plan: MutationPlan,
  trace: Vec<MutationTrace>,
  resulting_objects: Vec<MutationObject>,
) -> RailResult<PathBuf> {
  let verified_inputs = plan.pre_apply.clone();
  let applied_actions = if phase == "apply" {
    plan.actions.clone()
  } else {
    Vec::new()
  };
  let receipt = MutationReceipt {
    contract_version: MUTATION_CONTRACT_VERSION,
    operation_id: plan.operation_id.clone(),
    operation: operation.to_string(),
    phase: phase.to_string(),
    status: status.to_string(),
    timestamp_utc: Utc::now().to_rfc3339(),
    plan,
    trace,
    verified_inputs,
    applied_actions,
    resulting_objects,
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

fn mutation_fingerprint(
  git: &SystemGit,
  operation: &str,
  resolved_refs: &MutationResolvedRefs,
  pre_apply: &MutationPreApplyChecks,
  actions: &[MutationAction],
  declared_inputs: &[MutationInput],
  risks: &[MutationRisk],
) -> RailResult<String> {
  let bytes = serde_json::to_vec(&serde_json::json!({
    "operation": operation,
    "resolved_refs": resolved_refs,
    "pre_apply": pre_apply,
    "actions": actions,
    "declared_inputs": declared_inputs,
    "risks": risks,
  }))
  .map_err(|error| RailError::message(format!("failed to serialize mutation inputs: {}", error)))?;
  Ok(format!("git-object:{}", git.hash_bytes(&bytes)?))
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
  let git = ctx.git()?.git();
  let changed_paths = git.changed_paths()?;
  Ok(MutationPreApplyChecks {
    git_head: ctx.git()?.git().head_commit()?,
    config_fingerprint: config_fingerprint(workspace_root),
    toolchain_fingerprint: toolchain_fingerprint(workspace_root),
    lock_fingerprint: file_fingerprint(&workspace_root.join("Cargo.lock")),
    metadata_fingerprint: file_fingerprint(&workspace_root.join("target/cargo-rail/metadata.json")),
    worktree_fingerprint: fingerprint_changed_paths(git, &git.worktree_root, &changed_paths)?,
    changed_paths,
  })
}

/// Return the deduplicated workspace paths authorized by a plan.
pub fn expected_paths(plan: &MutationPlan) -> Vec<PathBuf> {
  plan
    .actions
    .iter()
    .flat_map(|action| action.expected_mutations.iter().map(|mutation| mutation.path.clone()))
    .collect::<BTreeSet<_>>()
    .into_iter()
    .collect()
}

/// Return explicit non-mutated input paths bound by a plan.
pub fn declared_input_paths(plan: &MutationPlan) -> Vec<PathBuf> {
  plan.declared_inputs.iter().map(|input| input.path.clone()).collect()
}

/// Verify that an approved plan authorizes the currently requested action payloads.
pub fn validate_requested_operation(approved: &MutationPlan, expected: &MutationPlan) -> RailResult<()> {
  if approved.contract_version == expected.contract_version
    && approved.actions == expected.actions
    && approved.declared_inputs == expected.declared_inputs
    && approved.risks == expected.risks
  {
    return Ok(());
  }
  Err(RailError::with_help(
    "provided mutation plan does not match current requested operation",
    "regenerate the plan with the exact command options and retry",
  ))
}

/// Reject any current worktree change outside a plan's explicit path allowlist.
pub fn validate_changed_paths(ctx: &WorkspaceContext, plan: &MutationPlan) -> RailResult<()> {
  validate_changed_paths_with_allowed_paths(ctx, plan, &[])
}

/// Reject worktree changes outside the plan and declared control inputs.
pub fn validate_changed_paths_with_allowed_paths(
  ctx: &WorkspaceContext,
  plan: &MutationPlan,
  allowed_paths: &[PathBuf],
) -> RailResult<()> {
  let git = ctx.git()?.git();
  let canonical_git_root = fs::canonicalize(&git.worktree_root)?;
  let mut allowed: BTreeSet<_> = expected_paths(plan)
    .into_iter()
    .chain(declared_input_paths(plan))
    .collect();
  for path in allowed_paths {
    let relative = if path.is_absolute() {
      let canonical = fs::canonicalize(path)?;
      let Ok(relative) = canonical.strip_prefix(&canonical_git_root) else {
        continue;
      };
      relative.to_path_buf()
    } else {
      path.clone()
    };
    allowed.insert(relative);
  }
  let changed = git.changed_paths()?;
  let unexpected: Vec<_> = changed.into_iter().filter(|path| !allowed.contains(path)).collect();
  if unexpected.is_empty() {
    return Ok(());
  }
  Err(RailError::with_help(
    format!(
      "mutation produced unplanned worktree changes: {}",
      display_paths(&unexpected)
    ),
    "restore the unexpected paths, then regenerate and re-run the mutation plan",
  ))
}

fn fingerprint_changed_paths(git: &SystemGit, workspace_root: &Path, paths: &[PathBuf]) -> RailResult<String> {
  let mut bytes = Vec::new();
  for path in paths {
    let absolute = workspace_root.join(path);
    append_fingerprint_frame(&mut bytes, b"path", path.to_string_lossy().as_bytes());
    match fs::symlink_metadata(&absolute) {
      Ok(metadata) if metadata.file_type().is_symlink() => {
        let target = fs::read_link(&absolute)?;
        append_fingerprint_frame(&mut bytes, b"symlink", target.to_string_lossy().as_bytes());
      }
      Ok(metadata) if metadata.is_file() => {
        append_fingerprint_frame(&mut bytes, b"file", &fs::read(&absolute)?);
      }
      Ok(_) => append_fingerprint_frame(&mut bytes, b"non-file", b""),
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
        append_fingerprint_frame(&mut bytes, b"missing", b"");
      }
      Err(error) => return Err(error.into()),
    }
  }
  Ok(format!("git-object:{}", git.hash_bytes(&bytes)?))
}

fn append_fingerprint_frame(output: &mut Vec<u8>, kind: &[u8], value: &[u8]) {
  output.extend_from_slice(&(kind.len() as u64).to_be_bytes());
  output.extend_from_slice(kind);
  output.extend_from_slice(&(value.len() as u64).to_be_bytes());
  output.extend_from_slice(value);
}

fn git_path_fingerprint(git: &SystemGit, path: &Path) -> RailResult<String> {
  let bytes = match fs::symlink_metadata(path) {
    Ok(metadata) if metadata.file_type().is_symlink() => {
      let target = fs::read_link(path)?;
      let mut bytes = b"symlink\0".to_vec();
      bytes.extend_from_slice(target.to_string_lossy().as_bytes());
      bytes
    }
    Ok(metadata) if metadata.is_file() => fs::read(path)?,
    Ok(_) => b"non-file".to_vec(),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => b"missing".to_vec(),
    Err(error) => return Err(error.into()),
  };
  Ok(format!("git-object:{}", git.hash_bytes(&bytes)?))
}

fn display_paths(paths: &[PathBuf]) -> String {
  if paths.is_empty() {
    "none".to_string()
  } else {
    paths
      .iter()
      .map(|path| path.display().to_string())
      .collect::<Vec<_>>()
      .join(", ")
  }
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
    let op = build_operation_id("unify apply", "git-object:0123456789abcdef");
    assert_eq!(op, "unify-apply-0123456789ab");
  }
}
