//! `cargo rail hash` and `cargo rail diff-hash` introspection commands.

use crate::commands::common::TextJsonOutputFormat;
use crate::commands::plan::{PlanOptions, build_plan_output};
use crate::error::{RailError, RailResult};
use crate::workspace::WorkspaceContext;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const PLAN_IDENTITY_CONTRACT_VERSION: u32 = 1;

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
    pub format: TextJsonOutputFormat,
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
    let portable = portable_plan_value(&plan_json)?;
    let identity = identity_for_portable_plan(&portable);

    if opts.format.is_json() {
        let payload = serde_json::json!({
          "command": "hash",
          "algorithm": "sha256",
          "identity": identity,
          "hash": identity,
          "identity_contract_version": PLAN_IDENTITY_CONTRACT_VERSION,
          "config_fingerprint": plan.inputs.config_fingerprint,
          "plan_contract_version": plan.plan_contract_version,
          "portable": true,
          "cache_key": false,
          "excluded_local_fields": ["inputs.workspace_root", "inputs.snapshot_id", "reproducibility"],
          "refs": plan.inputs.refs,
        });
        let out = crate::output::machine_json_envelope("hash", "inspect", "success", 0, payload);
        println!(
            "{}",
            serde_json::to_string_pretty(&out)
                .map_err(|e| RailError::message(format!("failed to render JSON: {}", e)))?
        );
    } else {
        println!("{}", identity);
    }

    Ok(())
}

/// Run the `diff-hash` command.
pub fn run_diff_hash(a: PathBuf, b: PathBuf, format: TextJsonOutputFormat) -> RailResult<()> {
    if format.is_json_like() {
        crate::output::set_json_mode(true);
    }

    let a_json = portable_plan_value(&read_json_file(&a)?)?;
    let b_json = portable_plan_value(&read_json_file(&b)?)?;

    let a_identity = identity_for_portable_plan(&a_json);
    let b_identity = identity_for_portable_plan(&b_json);

    let mut changes = Vec::new();
    collect_diffs(&a_json, &b_json, "$", &mut changes, 64);
    let equal = a_identity == b_identity;

    if format.is_json() {
        let payload = serde_json::json!({
          "command": "diff-hash",
          "a": a.display().to_string(),
          "b": b.display().to_string(),
          "identity_a": a_identity,
          "identity_b": b_identity,
          "hash_a": a_identity,
          "hash_b": b_identity,
          "identity_contract_version": PLAN_IDENTITY_CONTRACT_VERSION,
          "portable": true,
          "cache_key": false,
          "equal": equal,
          "changes": changes,
        });
        let out = crate::output::machine_json_envelope("diff-hash", "inspect", "success", 0, payload);
        println!(
            "{}",
            serde_json::to_string_pretty(&out)
                .map_err(|e| RailError::message(format!("failed to render JSON: {}", e)))?
        );
    } else if equal {
        println!("plan identities match");
        println!("  {}", a_identity);
    } else {
        println!("plan identity mismatch");
        println!("  a: {}", a_identity);
        println!("  b: {}", b_identity);
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

fn identity_for_portable_plan(value: &Value) -> String {
    let canonical = canonical_json(value);
    crate::instrumentation::record_hash(canonical.len());
    let digest = Sha256::digest(canonical.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        hex.push(char::from(HEX[usize::from(byte >> 4)]));
        hex.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    format!("plan-v{}:sha256:{}", PLAN_IDENTITY_CONTRACT_VERSION, hex)
}

fn portable_plan_value(value: &Value) -> RailResult<Value> {
    let plan = value
        .as_object()
        .ok_or_else(|| RailError::message("planner contract must be a JSON object"))?;

    let mut portable = serde_json::Map::new();
    for key in [
        "plan_contract_version",
        "resolution_universe",
        "files",
        "impact",
        "scope",
        "surfaces",
        "trace",
    ] {
        let field = plan
            .get(key)
            .ok_or_else(|| RailError::message(format!("planner contract is missing '{}'", key)))?;
        portable.insert(key.to_string(), field.clone());
    }

    let inputs = plan
        .get("inputs")
        .and_then(Value::as_object)
        .ok_or_else(|| RailError::message("planner contract is missing object 'inputs'"))?;
    let workspace_root = inputs
        .get("workspace_root")
        .and_then(Value::as_str)
        .ok_or_else(|| RailError::message("planner inputs are missing string 'workspace_root'"))?;
    let mut portable_inputs = serde_json::Map::new();
    for key in [
        "refs",
        "config_fingerprint",
        "toolchain_fingerprint",
        "confidence_profile",
        "confidence_profile_source",
    ] {
        let field = inputs
            .get(key)
            .ok_or_else(|| RailError::message(format!("planner inputs are missing '{}'", key)))?;
        portable_inputs.insert(key.to_string(), field.clone());
    }
    portable.insert("inputs".to_string(), Value::Object(portable_inputs));

    normalize_repository_paths(&mut portable, workspace_root)?;
    Ok(Value::Object(portable))
}

fn normalize_repository_paths(plan: &mut serde_json::Map<String, Value>, workspace_root: &str) -> RailResult<()> {
    if let Some(files) = plan.get_mut("files").and_then(Value::as_array_mut) {
        for file in files {
            normalize_path_field(file, "path")?;
        }
    }
    if let Some(trace) = plan.get_mut("trace").and_then(Value::as_array_mut) {
        for reason in trace {
            normalize_path_field(reason, "file")?;
            normalize_package_id_field(reason, "package_id", workspace_root)?;
            normalize_package_id_field(reason, "depends_on_package_id", workspace_root)?;
        }
    }
    Ok(())
}

fn normalize_package_id_field(value: &mut Value, field: &str, workspace_root: &str) -> RailResult<()> {
    let Some(package_id) = value.get_mut(field) else {
        return Ok(());
    };
    let Some(package_id_str) = package_id.as_str() else {
        return Err(RailError::message(format!(
            "planner field '{}' must be a string",
            field
        )));
    };
    let Some(path_identity) = package_id_str.strip_prefix("path+file://") else {
        return Ok(());
    };
    let Some(fragment_start) = path_identity.rfind('#') else {
        return Err(RailError::message(format!(
            "planner package identity '{}' has no Cargo fragment",
            package_id_str
        )));
    };
    let (path, fragment) = path_identity.split_at(fragment_start);
    let normalized_root = workspace_root.replace('\\', "/");
    let normalized_path = path.replace('\\', "/");
    let relative = if normalized_path == normalized_root {
        ""
    } else if let Some(relative) = normalized_path
        .strip_prefix(&normalized_root)
        .and_then(|path| path.strip_prefix('/'))
    {
        relative
    } else {
        *package_id = Value::String(format!("path+external:///{fragment}"));
        return Ok(());
    };
    *package_id = Value::String(format!("path+workspace:///{relative}{fragment}"));
    Ok(())
}

fn normalize_path_field(value: &mut Value, field: &str) -> RailResult<()> {
    let Some(path) = value.get_mut(field) else {
        return Ok(());
    };
    let Some(path_str) = path.as_str() else {
        return Err(RailError::message(format!(
            "planner field '{}' must be a string",
            field
        )));
    };
    let normalized = path_str.replace('\\', "/");
    let drive_absolute = normalized.as_bytes().get(1) == Some(&b':');
    if normalized.starts_with('/')
        || normalized.starts_with("//")
        || drive_absolute
        || normalized.split('/').any(|component| component == "..")
    {
        return Err(RailError::message(format!(
            "planner path '{}' is not repository-relative",
            path_str
        )));
    }
    *path = Value::String(normalized);
    Ok(())
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
