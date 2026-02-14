//! `cargo rail hash` and `cargo rail diff-hash` introspection commands.

use crate::commands::common::OutputFormat;
use crate::commands::plan::{PlanOptions, build_plan_output};
use crate::error::{RailError, RailResult};
use crate::workspace::WorkspaceContext;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Options for `cargo rail hash`.
pub struct HashOptions {
  /// Git ref to compare against.
  pub since: Option<String>,
  /// Start of SHA range.
  pub from: Option<String>,
  /// End of SHA range.
  pub to: Option<String>,
  /// Use merge-base with default branch.
  pub merge_base: bool,
  /// Planner confidence profile override.
  pub confidence_profile: Option<String>,
  /// Output format.
  pub format: OutputFormat,
}

/// Run the `hash` command.
pub fn run_hash(ctx: &WorkspaceContext, opts: HashOptions) -> RailResult<()> {
  if opts.format.is_json_like() {
    crate::output::set_json_mode(true);
  }

  let plan_opts = PlanOptions {
    since: opts.since,
    from: opts.from,
    to: opts.to,
    merge_base: opts.merge_base,
    format: crate::commands::common::PlanOutputFormat::Json,
    output: None,
    explain: false,
    confidence_profile: opts.confidence_profile,
  };

  let plan = build_plan_output(ctx, &plan_opts)?;
  let plan_json = serde_json::to_value(&plan)
    .map_err(|e| RailError::message(format!("failed to serialize plan for hashing: {}", e)))?;
  let canonical = canonical_json(&plan_json);
  let hash = format!("fnv1a64:{:016x}", fnv1a64(canonical.as_bytes()));

  if opts.format.is_json() {
    let out = serde_json::json!({
      "command": "hash",
      "algorithm": "fnv1a64",
      "hash": hash,
      "inputs_fingerprint": plan.inputs.config_fingerprint.clone(),
      "plan_contract_version": plan.plan_contract_version,
      "refs": plan.inputs.refs,
    });
    println!(
      "{}",
      serde_json::to_string_pretty(&out).map_err(|e| RailError::message(format!("failed to render JSON: {}", e)))?
    );
  } else {
    println!("{}", hash);
  }

  Ok(())
}

/// Run the `diff-hash` command.
pub fn run_diff_hash(a: PathBuf, b: PathBuf, format: OutputFormat) -> RailResult<()> {
  if format.is_json_like() {
    crate::output::set_json_mode(true);
  }

  let a_json = read_json_file(&a)?;
  let b_json = read_json_file(&b)?;

  let a_hash = format!("fnv1a64:{:016x}", fnv1a64(canonical_json(&a_json).as_bytes()));
  let b_hash = format!("fnv1a64:{:016x}", fnv1a64(canonical_json(&b_json).as_bytes()));

  let mut changes = Vec::new();
  collect_diffs(&a_json, &b_json, "$", &mut changes, 64);
  let equal = a_hash == b_hash;

  if format.is_json() {
    let out = serde_json::json!({
      "command": "diff-hash",
      "a": a.display().to_string(),
      "b": b.display().to_string(),
      "hash_a": a_hash,
      "hash_b": b_hash,
      "equal": equal,
      "changes": changes,
    });
    println!(
      "{}",
      serde_json::to_string_pretty(&out).map_err(|e| RailError::message(format!("failed to render JSON: {}", e)))?
    );
  } else if equal {
    println!("hashes match");
    println!("  {}", a_hash);
  } else {
    println!("hash mismatch");
    println!("  a: {}", a_hash);
    println!("  b: {}", b_hash);
    if changes.is_empty() {
      println!("  no structural diff paths found");
    } else {
      println!("  changed paths:");
      for change in &changes {
        println!("    {}", change);
      }
    }
  }

  Ok(())
}

fn read_json_file(path: &Path) -> RailResult<Value> {
  let content = std::fs::read_to_string(path)
    .map_err(|e| RailError::message(format!("failed to read '{}': {}", path.display(), e)))?;
  serde_json::from_str(&content).map_err(|e| RailError::message(format!("invalid JSON '{}': {}", path.display(), e)))
}

fn canonical_json(value: &Value) -> String {
  let canonical = canonicalize_value(value);
  serde_json::to_string(&canonical).unwrap_or_else(|_| "null".to_string())
}

fn canonicalize_value(value: &Value) -> Value {
  match value {
    Value::Object(map) => {
      let mut keys: Vec<&String> = map.keys().collect();
      keys.sort_unstable();
      let mut out = serde_json::Map::new();
      for key in keys {
        if let Some(v) = map.get(key) {
          out.insert(key.clone(), canonicalize_value(v));
        }
      }
      Value::Object(out)
    }
    Value::Array(arr) => Value::Array(arr.iter().map(canonicalize_value).collect()),
    _ => value.clone(),
  }
}

fn collect_diffs(a: &Value, b: &Value, path: &str, out: &mut Vec<String>, limit: usize) {
  if out.len() >= limit {
    return;
  }

  match (a, b) {
    (Value::Object(a_map), Value::Object(b_map)) => {
      let mut keys = BTreeSet::new();
      keys.extend(a_map.keys().cloned());
      keys.extend(b_map.keys().cloned());
      for key in keys {
        let child_path = format!("{}.{}", path, key);
        match (a_map.get(&key), b_map.get(&key)) {
          (Some(va), Some(vb)) => collect_diffs(va, vb, &child_path, out, limit),
          (Some(_), None) => out.push(format!("{} removed", child_path)),
          (None, Some(_)) => out.push(format!("{} added", child_path)),
          (None, None) => {}
        }
        if out.len() >= limit {
          return;
        }
      }
    }
    (Value::Array(a_arr), Value::Array(b_arr)) => {
      if a_arr.len() != b_arr.len() {
        out.push(format!("{} length {} -> {}", path, a_arr.len(), b_arr.len()));
      }
      let max = a_arr.len().min(b_arr.len());
      for idx in 0..max {
        collect_diffs(&a_arr[idx], &b_arr[idx], &format!("{}[{}]", path, idx), out, limit);
        if out.len() >= limit {
          return;
        }
      }
    }
    _ => {
      if a != b {
        out.push(path.to_string());
      }
    }
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
