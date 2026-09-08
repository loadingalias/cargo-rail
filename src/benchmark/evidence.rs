//! Validate action-bound output evidence from the production cache, without reading its private CAS formats.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::compiler::native_cache::{
    ACTION_KEY_PREFIX, BENCH_COVERAGE_VERSION, BenchmarkOutput, MAX_BENCH_COVERAGE_EVENT_BYTES, RESULT_KEY_PREFIX,
};
use crate::compiler::operation::CompilerOperation;
use crate::source::ContentDigest;
use crate::{RailError, RailResult};

type Admission = (String, String);

#[derive(Default, Serialize)]
pub(super) struct Evidence {
    pub(super) events: u64,
    pub(super) hits: u64,
    pub(super) misses: u64,
    pub(super) bypass_reasons: BTreeMap<String, u64>,
    pub(super) restored_files: usize,
    pub(super) unretained_restore_outputs: usize,
    pub(super) restored_bytes: u64,
    pub(super) restored_by_role: BTreeMap<String, u64>,
    #[serde(skip)]
    admitted: BTreeMap<Admission, Vec<BenchmarkOutput>>,
    #[serde(skip)]
    retained: BTreeMap<PathBuf, BenchmarkOutput>,
}

pub(super) fn read(directory: &Path, workspace: &Path, seed: Option<&Evidence>) -> RailResult<Evidence> {
    let mut evidence = Evidence::default();
    let mut invocations = BTreeSet::new();
    let mut restored = Vec::new();
    let mut candidates = BTreeMap::<PathBuf, Vec<BenchmarkOutput>>::new();
    let mut paths = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    for path in paths {
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file()
            || crate::utils::is_symlink_or_reparse(&metadata)
            || metadata.len() > MAX_BENCH_COVERAGE_EVENT_BYTES as u64 + 1
        {
            return Err(RailError::message(
                "compiler evidence is not a bounded regular event file",
            ));
        }
        let event: Value = serde_json::from_slice(&fs::read(&path)?)?;
        validate_event(&event)?;
        if text(&event, "recording_identity")?
            != format!(
                "sha256:{}",
                ContentDigest::sha256(directory.as_os_str().as_encoded_bytes())
            )
            || !invocations.insert(text(&event, "invocation_identity")?.to_string())
        {
            return Err(RailError::message(
                "compiler evidence is duplicated or belongs to another recording",
            ));
        }
        evidence.events += 1;
        let status = text(&event, "status")?;
        let reason = text(&event, "reason")?;
        match status {
            "hit" => evidence.hits += 1,
            "miss" => evidence.misses += 1,
            "bypassed" | "disabled" => *evidence.bypass_reasons.entry(reason.to_string()).or_default() += 1,
            _ => return Err(RailError::message("unknown compiler evidence outcome")),
        }
        if reason == "local_cache_unavailable"
            || crate::cache::installation::NativeCacheFailureReason::from_reason(reason).is_some()
        {
            return Err(RailError::message(format!(
                "Cargo-Rail evidence records an unavailable cache or incomplete observation: {reason}"
            )));
        }
        let outputs = event
            .get("outputs")
            .ok_or_else(|| RailError::message("compiler event omitted output evidence"))?;
        if outputs.is_null() {
            if status == "hit" || (status == "miss" && event.get("result_key").is_some()) {
                return Err(RailError::message(
                    "successful cache outcome omitted its output inventory",
                ));
            }
            continue;
        }
        if status != "hit" && status != "miss" {
            return Err(RailError::message("bypassed compiler action claimed admitted outputs"));
        }
        let output_values = outputs
            .as_array()
            .filter(|outputs| !outputs.is_empty())
            .ok_or_else(|| RailError::message("cache output inventory is empty"))?;
        if output_values
            .iter()
            .any(|output| output.get("symlink_target") != Some(&Value::Null))
        {
            return Err(RailError::message(
                "compiler output inventory omitted regular-file authority",
            ));
        }
        let outputs: Vec<BenchmarkOutput> = serde_json::from_value(outputs.clone())?;
        let key = (
            identity(&event, "action_key", ACTION_KEY_PREFIX)?,
            identity(&event, "result_key", RESULT_KEY_PREFIX)?,
        );
        let mut members = BTreeSet::new();
        for output in &outputs {
            if !members.insert(&output.path) {
                return Err(RailError::message("compiler output inventory repeats a destination"));
            }
            validate_descriptor(output, workspace)?;
            candidates.entry(output.path.clone()).or_default().push(output.clone());
        }
        if status == "miss" {
            if let Some(previous) = evidence.admitted.insert(key, outputs.clone())
                && previous != outputs
            {
                return Err(RailError::message(
                    "one admitted result has conflicting output inventories",
                ));
            }
        } else {
            let expected = seed
                .and_then(|seed| seed.admitted.get(&key))
                .ok_or_else(|| RailError::message("restored compiler result has no admission in this sample's seed"))?;
            if expected != &outputs {
                return Err(RailError::message(
                    "restored outputs differ from the exact admitted seed inventory",
                ));
            }
            restored.extend(outputs);
        }
    }
    for (path, outputs) in candidates {
        if !path.try_exists()? {
            continue;
        }
        let retained = outputs
            .into_iter()
            .find(|output| validate_output(output, workspace).is_ok())
            .ok_or_else(|| {
                RailError::message(format!(
                    "final compiler output does not match any recorded producer: {}",
                    path.display()
                ))
            })?;
        evidence.retained.insert(path, retained);
    }
    let mut verified = BTreeMap::new();
    for output in restored {
        if evidence
            .retained
            .get(&output.path)
            .is_some_and(|retained| same_materialized_output(retained, &output))
        {
            verified.insert(output.path.clone(), output);
        } else {
            if seed
                .and_then(|seed| seed.retained.get(&output.path))
                .is_some_and(|retained| same_materialized_output(retained, &output))
                && !evidence.retained.contains_key(&output.path)
            {
                return Err(RailError::message(format!(
                    "a restored output retained in the seed disappeared: {}",
                    output.path.display()
                )));
            }
            evidence.unretained_restore_outputs += 1;
        }
    }
    evidence.restored_files = verified.len();
    for output in verified.values() {
        evidence.restored_bytes = evidence
            .restored_bytes
            .checked_add(output.bytes)
            .ok_or_else(|| RailError::message("restored output byte count overflow"))?;
        *evidence.restored_by_role.entry(output.role.clone()).or_default() += 1;
    }
    Ok(evidence)
}

fn validate_event(event: &Value) -> RailResult<()> {
    if event["schema_version"] != BENCH_COVERAGE_VERSION || event["lane"] != "cargo-rail" {
        return Err(RailError::message("incompatible compiler evidence event"));
    }
    let compiler = text(event, "compiler")?;
    let arguments: Vec<String> = serde_json::from_value(event["arguments"].clone())?;
    let operation = CompilerOperation::capture(compiler, &arguments)?;
    if event["action"] != serde_json::to_value(&operation)? || text(event, "action_id")? != operation.identity()? {
        return Err(RailError::message(
            "compiler evidence identity does not match its invocation",
        ));
    }
    for field in [
        "remote_request_attempts",
        "remote_coordinator_requests",
        "remote_payload_bytes_read",
        "remote_payload_bytes_written",
    ] {
        if event[field].as_u64() != Some(0) {
            return Err(RailError::message(
                "local compiler event used remote authority or omitted transfer evidence",
            ));
        }
    }
    if event.get("distributed_timing").is_some()
        || event.get("remote_base_action_key").is_some()
        || event.get("remote_error").is_some()
    {
        return Err(RailError::message(
            "local compiler event contains remote or distributed evidence",
        ));
    }
    Ok(())
}

fn same_materialized_output(left: &BenchmarkOutput, right: &BenchmarkOutput) -> bool {
    left.path == right.path
        && left.role == right.role
        && left.sha256 == right.sha256
        && left.bytes == right.bytes
        && left.mode == right.mode
        && left.symlink_target == right.symlink_target
}

fn validate_descriptor(output: &BenchmarkOutput, workspace: &Path) -> RailResult<()> {
    if !output.path.starts_with(workspace.join("target"))
        || crate::utils::canonicalize_allow_missing(&output.path)? != output.path
    {
        return Err(RailError::message("compiler output escaped the benchmark target"));
    }
    for digest in [&output.sha256, &output.stored_sha256] {
        if !digest.strip_prefix("sha256:").is_some_and(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }) {
            return Err(RailError::message("output digest is not canonical SHA-256"));
        }
    }
    if output.symlink_target.is_some() || output.mode > 0o777 || output.role.is_empty() {
        return Err(RailError::message(
            "compiler output descriptor has an unsupported role, mode or file kind",
        ));
    }
    Ok(())
}

fn validate_output(output: &BenchmarkOutput, workspace: &Path) -> RailResult<()> {
    validate_descriptor(output, workspace)?;
    let metadata = fs::symlink_metadata(&output.path)?;
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o777
    };
    #[cfg(not(unix))]
    let mode = if metadata.permissions().readonly() {
        0o444
    } else {
        0o644
    };
    if !metadata.is_file()
        || crate::utils::is_symlink_or_reparse(&metadata)
        || output.symlink_target.is_some()
        || metadata.len() != output.bytes
        || mode != output.mode
        || format!("sha256:{}", ContentDigest::sha256(&fs::read(&output.path)?)) != output.sha256
    {
        return Err(RailError::message(format!(
            "compiler output no longer matches its admitted bytes or mode: {}",
            output.path.display()
        )));
    }
    Ok(())
}

fn text<'a>(value: &'a Value, field: &str) -> RailResult<&'a str> {
    value[field]
        .as_str()
        .ok_or_else(|| RailError::message(format!("compiler event omitted {field}")))
}

fn identity(value: &Value, field: &str, prefix: &str) -> RailResult<String> {
    let value = text(value, field)?;
    let suffix = value
        .strip_prefix(prefix)
        .ok_or_else(|| RailError::message("compiler event has the wrong identity domain"))?;
    if suffix.len() != 64
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(RailError::message("compiler event identity is not canonical SHA-256"));
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn output_validation_rejects_same_size_corruption_and_escaped_paths() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let workspace = crate::utils::canonicalize_existing(directory.path())?;
        fs::create_dir(workspace.join("target"))?;
        let file = workspace.join("target/output");
        fs::write(&file, b"abc")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&file, fs::Permissions::from_mode(0o644))?;
        }
        let digest = "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_string();
        let output = BenchmarkOutput {
            role: "rlib".to_string(),
            path: file.clone(),
            sha256: digest.clone(),
            stored_sha256: digest,
            bytes: 3,
            mode: 0o644,
            symlink_target: None,
        };
        validate_output(&output, &workspace)?;
        fs::write(&file, b"xyz")?;
        anyhow::ensure!(
            validate_output(&output, &workspace).is_err(),
            "same-size corruption was accepted"
        );
        fs::write(&file, b"abc")?;
        let mut escaped = output;
        escaped.path = workspace.join("outside");
        fs::write(&escaped.path, b"abc")?;
        anyhow::ensure!(
            validate_output(&escaped, &workspace).is_err(),
            "output outside target was accepted"
        );
        Ok(())
    }

    #[test]
    fn local_events_reject_changed_operation_identity_and_remote_activity() -> anyhow::Result<()> {
        let arguments = vec!["--version".to_string()];
        let action = CompilerOperation::capture("rustc", &arguments)?;
        let event = json!({
            "schema_version": 10, "lane": "cargo-rail", "compiler": "rustc", "arguments": arguments,
            "action": action, "action_id": action.identity()?, "remote_request_attempts": 0,
            "remote_coordinator_requests": 0, "remote_payload_bytes_read": 0, "remote_payload_bytes_written": 0,
        });
        validate_event(&event)?;
        let mut modified = event.clone();
        modified["action_id"] = json!("unbound");
        anyhow::ensure!(validate_event(&modified).is_err());
        let mut remote = event.clone();
        remote["remote_request_attempts"] = json!(1);
        anyhow::ensure!(validate_event(&remote).is_err());
        let mut unknown = event;
        unknown["schema_version"] = json!(11);
        anyhow::ensure!(validate_event(&unknown).is_err());
        Ok(())
    }
    fn write_receipt(directory: &Path, status: &str, outputs: &[BenchmarkOutput]) -> RailResult<()> {
        fs::create_dir(directory)?;
        let arguments = vec![
            "--crate-name".to_string(),
            "probe".to_string(),
            "--crate-type=rlib".to_string(),
            "--emit=dep-info,metadata".to_string(),
            "input.rs".to_string(),
        ];
        let action = CompilerOperation::capture("rustc", &arguments)?;
        let event = json!({
            "schema_version": 10, "lane": "cargo-rail", "compiler": "rustc", "arguments": arguments,
            "action": action, "action_id": action.identity()?, "remote_request_attempts": 0,
            "remote_coordinator_requests": 0, "remote_payload_bytes_read": 0, "remote_payload_bytes_written": 0,
            "recording_identity": format!("sha256:{}", ContentDigest::sha256(directory.as_os_str().as_encoded_bytes())),
            "invocation_identity": "one-invocation", "status": status, "reason": "verified_local_result",
            "action_key": format!("{ACTION_KEY_PREFIX}{}", "a".repeat(64)),
            "result_key": format!("{RESULT_KEY_PREFIX}{}", "b".repeat(64)), "outputs": outputs,
        });
        fs::write(directory.join("event.json"), serde_json::to_vec(&event)?)?;
        Ok(())
    }

    #[test]
    fn retained_counts_exclude_deleted_probes_but_reject_missing_seed_artifacts() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let workspace = crate::utils::canonicalize_existing(directory.path())?;
        fs::create_dir(workspace.join("target"))?;
        let kept = workspace.join("target/probe.rmeta");
        fs::write(&kept, b"abc")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&kept, fs::Permissions::from_mode(0o644))?;
        }
        let digest = "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_string();
        let output = BenchmarkOutput {
            role: "metadata".to_string(),
            path: kept.clone(),
            sha256: digest.clone(),
            stored_sha256: digest,
            bytes: 3,
            mode: 0o644,
            symlink_target: None,
        };
        let mut consumed = output.clone();
        consumed.role = "dep_info".to_string();
        consumed.path = workspace.join("target/probe.d");
        let outputs = [output, consumed];
        let seed_directory = workspace.join("seed");
        write_receipt(&seed_directory, "miss", &outputs)?;
        let seed = read(&seed_directory, &workspace, None)?;
        let measured = workspace.join("measured");
        write_receipt(&measured, "hit", &outputs)?;
        let result = read(&measured, &workspace, Some(&seed))?;
        anyhow::ensure!(result.hits == 1 && result.restored_files == 1 && result.restored_bytes == 3);
        anyhow::ensure!(result.unretained_restore_outputs == 1);
        anyhow::ensure!(result.restored_by_role == BTreeMap::from([("metadata".to_string(), 1)]));
        fs::remove_file(&kept)?;
        let error = read(&measured, &workspace, Some(&seed))
            .err()
            .ok_or_else(|| anyhow::anyhow!("missing retained artifact accepted"))?;
        anyhow::ensure!(error.to_string().contains("retained in the seed disappeared"));
        Ok(())
    }

    #[test]
    fn recording_rejects_replayed_and_truncated_events() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let workspace = crate::utils::canonicalize_existing(directory.path())?;
        let source = workspace.join("source");
        write_receipt(&source, "bypassed", &[])?;
        let path = source.join("event.json");
        let mut event: Value = serde_json::from_slice(&fs::read(&path)?)?;
        event["outputs"] = Value::Null;
        event.as_object_mut().expect("event object").remove("action_key");
        event.as_object_mut().expect("event object").remove("result_key");
        fs::write(&path, serde_json::to_vec(&event)?)?;
        anyhow::ensure!(read(&source, &workspace, None)?.events == 1);
        let foreign = workspace.join("foreign");
        fs::create_dir(&foreign)?;
        fs::copy(&path, foreign.join("event.json"))?;
        anyhow::ensure!(read(&foreign, &workspace, None).is_err(), "foreign recording accepted");
        fs::copy(&path, source.join("duplicate.json"))?;
        anyhow::ensure!(
            read(&source, &workspace, None).is_err(),
            "duplicate invocation accepted"
        );
        fs::remove_file(source.join("duplicate.json"))?;
        fs::write(path, b"{")?;
        anyhow::ensure!(read(&source, &workspace, None).is_err(), "truncated event accepted");
        Ok(())
    }
}
