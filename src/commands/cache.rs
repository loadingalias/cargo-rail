//! Read-only cache status and explicitly scoped cache reclamation.

use super::TextJsonOutputFormat;
use super::cli::CacheScope;
use crate::cache::CacheStatus;
use crate::error::{RailError, RailResult};
use std::path::Path;

/// Validate and normalize one machine-owned remote authority without contacting it.
pub(crate) fn run_normalize(
    remote_url: &str,
    mode: Option<&str>,
    environment: Vec<String>,
    format: TextJsonOutputFormat,
) -> RailResult<()> {
    if format.is_json() {
        crate::output::set_json_mode(true);
    }
    let selection = crate::remote_cache::RemoteCacheSelection::parse(remote_url, mode, &environment)
        .map_err(|error| RailError::message(format!("remote cache URL is invalid: {error}")))?;
    let status = crate::remote_cache::RemoteCacheConfigurationStatus::from_selection(&selection);
    if format.is_json() {
        let output = crate::output::machine_json_envelope(
            "cache",
            "normalize",
            "success",
            0,
            serde_json::json!({
              "normalized_url": selection.normalized_url(),
              "remote": status,
            }),
        );
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("remote compiler cache authority");
        println!("  normalized URL: {}", selection.normalized_url());
        println!("  provider: {}", status.provider);
        println!("  protocol: {}", status.protocol);
        println!("  authority: {}", status.authority);
        println!("  mode: {}", status.mode);
        println!(
            "  shared compiler environment names: {}",
            status.shared_environment_names
        );
    }
    Ok(())
}

/// Preview or apply one exact transparent compiler-cache installation.
pub(crate) fn run_setup(
    current_dir: &Path,
    request: crate::cache::installation::SetupRequest,
    check: bool,
    format: TextJsonOutputFormat,
) -> RailResult<()> {
    if format.is_json() {
        crate::output::set_json_mode(true);
    }
    let plan = crate::cache::installation::plan_setup(current_dir, &request)?;
    let pending = plan.pending();
    let receipt_path = plan.receipt_path()?;
    let remote = plan
        .remote_selection()?
        .as_ref()
        .map(crate::remote_cache::RemoteCacheConfigurationStatus::from_selection);
    let details = serde_json::json!({
      "changed": pending,
      "config_path": plan.config_path(),
      "config_field": "build.rustc-wrapper",
      "config_action": plan.config_action(),
      "wrapper_path": plan.wrapper_path(),
      "receipt_path": receipt_path,
      "private_state_action": if pending { "install_or_repair" } else { "verify" },
      "cache_base": plan.cache_base(),
      "max_bytes": plan.max_bytes(),
      "remote": remote,
      "distributed": plan.distributed_mode(),
      "distributed_policy": plan.distributed_policy(),
    });
    if check {
        render_installation_operation("setup_check", pending, &details, format)?;
        return if pending {
            Err(RailError::CheckHasPendingChanges)
        } else {
            Ok(())
        };
    }
    crate::cache::installation::apply_setup(plan)?;
    render_installation_operation("setup", false, &details, format)
}

/// Preview or apply removal of the exact receipt-owned installation.
pub(crate) fn run_remove(current_dir: &Path, check: bool, format: TextJsonOutputFormat) -> RailResult<()> {
    if format.is_json() {
        crate::output::set_json_mode(true);
    }
    let plan = crate::cache::installation::plan_removal(current_dir)?;
    let pending = plan.pending();
    let details = serde_json::json!({
      "changed": pending,
      "config_path": plan.config_path(),
      "config_field": "build.rustc-wrapper",
      "config_action": plan.config_action(),
      "wrapper_path": plan.wrapper_path(),
      "receipt_path": plan.receipt_path(),
      "private_state_action": if pending { "remove_receipt_owned_installation" } else { "none" },
      "cache_preserved": true,
    });
    if check {
        render_installation_operation("remove_check", pending, &details, format)?;
        return if pending {
            Err(RailError::CheckHasPendingChanges)
        } else {
            Ok(())
        };
    }
    crate::cache::installation::apply_removal(plan)?;
    render_installation_operation("remove", false, &details, format)
}

fn render_installation_operation(
    operation: &str,
    pending: bool,
    details: &serde_json::Value,
    format: TextJsonOutputFormat,
) -> RailResult<()> {
    if format.is_json() {
        let mut payload = details.clone();
        payload["pending"] = serde_json::Value::Bool(pending);
        let output = crate::output::machine_json_envelope(
            "cache",
            operation,
            if pending { "pending_changes" } else { "success" },
            if pending { 1 } else { 0 },
            payload,
        );
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("transparent compiler cache {operation}");
        println!(
            "  Cargo config: {}",
            details["config_path"].as_str().unwrap_or("unknown")
        );
        println!(
            "  Cargo field: {} ({})",
            details["config_field"].as_str().unwrap_or("build.rustc-wrapper"),
            details["config_action"].as_str().unwrap_or("unknown")
        );
        if let Some(wrapper) = details["wrapper_path"].as_str() {
            println!("  wrapper: {wrapper}");
        }
        if let Some(cache) = details["cache_base"].as_str() {
            println!("  local cache base: {cache}");
        }
        if let Some(distributed) = details["distributed"].as_str() {
            println!("  distributed mode: {distributed}");
        }
        if let Some(policy) = details["distributed_policy"].as_str() {
            println!("  distributed placement: {policy}");
        }
        if let Some(receipt) = details["receipt_path"].as_str() {
            println!("  setup receipt: {receipt}");
        }
        if let Some(action) = details["private_state_action"].as_str() {
            println!("  private state: {action}");
        }
        if pending {
            println!("  changes pending");
        } else {
            println!("  no pending changes");
        }
    }
    Ok(())
}

/// Report selected cache scopes without creating workspace context or cache state.
pub(crate) fn run_status(workspace_root: &Path, scope: CacheScope, format: TextJsonOutputFormat) -> RailResult<()> {
    if format.is_json() {
        crate::output::set_json_mode(true);
    }
    let status = crate::cache::status(workspace_root, scope.includes_workspace(), scope.includes_local())?;
    if format.is_json() {
        let output = crate::output::machine_json_envelope(
            "cache",
            "status",
            "success",
            0,
            serde_json::json!({ "scope": scope.as_str(), "status": status }),
        );
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        render_status(&status);
    }
    Ok(())
}

/// Preview or apply byte-preserving recovery of one selected markerless CAS.
pub(crate) fn run_recover(workspace_root: &Path, check: bool, format: TextJsonOutputFormat) -> RailResult<()> {
    if format.is_json() {
        crate::output::set_json_mode(true);
    }
    let plan = if check {
        crate::cache::installation::plan_local_cache_recovery(workspace_root)?
    } else {
        crate::cache::installation::recover_local_cache(workspace_root)?
    };
    let pending = plan.is_some();
    if format.is_json() {
        let output = crate::output::machine_json_envelope(
            "cache",
            if check { "recover_check" } else { "recover" },
            if check && pending { "pending_changes" } else { "success" },
            if check && pending { 1 } else { 0 },
            serde_json::json!({
              "pending": pending,
              "recovery": plan,
            }),
        );
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if let Some(plan) = &plan {
        if check {
            println!("local CAS recovery pending");
        } else {
            println!("local CAS recovered");
        }
        println!("  selected root: {}", plan.selected_root);
        println!("  quarantine: {}", plan.quarantine_root);
        println!("  retained bytes: {}", plan.bytes);
        println!("  receipt: {}", plan.receipt_path);
    } else {
        println!("local CAS recovery not required");
    }
    if check && pending {
        Err(RailError::CheckHasPendingChanges)
    } else {
        Ok(())
    }
}

/// Preview or apply explicitly scoped cache reclamation.
pub(crate) fn run_clean(
    workspace_root: &Path,
    scope: CacheScope,
    check: bool,
    format: TextJsonOutputFormat,
) -> RailResult<()> {
    if format.is_json() {
        crate::output::set_json_mode(true);
    }
    if check {
        let status = crate::cache::status(workspace_root, scope.includes_workspace(), scope.includes_local())?;
        let pending = has_state(&status);
        if format.is_json() {
            let output = crate::output::machine_json_envelope(
                "cache",
                "clean_check",
                if pending { "pending_changes" } else { "success" },
                if pending { 1 } else { 0 },
                serde_json::json!({
                  "scope": scope.as_str(),
                  "would_reclaim_bytes": total_bytes(&status),
                  "status": status,
                }),
            );
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            render_status(&status);
            if pending {
                println!("\ncache state would be reclaimed; run without --check to apply");
            } else {
                println!("\nnothing to reclaim");
            }
        }
        return if pending {
            Err(RailError::CheckHasPendingChanges)
        } else {
            Ok(())
        };
    }

    // For a combined cleanup, validate the complete workspace scope before
    // deleting the cross-workspace CAS. Each removal then measures and mutates
    // under its own lifecycle authority.
    if scope.includes_workspace() && scope.includes_local() {
        crate::cache::status(workspace_root, true, false)?;
    }
    let mut removal = crate::cache::CacheRemoval::default();
    if scope.includes_local() {
        removal.extend(crate::cache::remove_local(workspace_root)?)?;
    }
    if scope.includes_workspace() {
        removal.extend(crate::cache::remove_workspace(workspace_root)?)?;
    }
    if format.is_json() {
        let output = crate::output::machine_json_envelope(
            "cache",
            "clean",
            "success",
            0,
            serde_json::json!({
              "scope": scope.as_str(),
              "reclaimed_bytes": removal.bytes,
              "removed": removal.paths,
            }),
        );
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if removal.paths.is_empty() {
        println!("nothing to reclaim");
    } else {
        println!(
            "reclaimed {} bytes from {} cache path(s)",
            removal.bytes,
            removal.paths.len()
        );
    }
    Ok(())
}

fn has_state(status: &CacheStatus) -> bool {
    status.workspace.as_ref().is_some_and(|workspace| {
        workspace
            .artifacts
            .iter()
            .any(|artifact| artifact.kind != "workspace_cache_lock")
    }) || status.local.as_ref().is_some_and(|local| local.present)
}

fn total_bytes(status: &CacheStatus) -> u64 {
    status
        .workspace
        .as_ref()
        .map_or(0, |workspace| workspace.bytes)
        .saturating_add(
            status
                .local
                .as_ref()
                .and_then(|local| local.cache.as_ref())
                .map_or(0, |local| local.bytes),
        )
}

fn render_status(status: &CacheStatus) {
    println!("cache status");
    println!("  transparent compiler reuse");
    println!("    state: {}", status.installation.state);
    println!("    healthy: {}", status.installation.healthy);
    println!("    Cargo home: {}", status.installation.cargo_home);
    println!("    Cargo config: {}", status.installation.config_path);
    if let Some(wrapper) = &status.installation.wrapper_path {
        println!("    wrapper: {wrapper}");
    }
    if let Some(cache) = &status.installation.cache_base {
        println!("    local cache base: {cache}");
    }
    if let Some(distributed) = status.installation.distributed {
        println!("    distributed mode: {distributed}");
    }
    if let Some(policy) = status.installation.distributed_policy {
        println!("    distributed placement: {policy}");
    }
    if let Some(history) = &status.installation.distributed_placement_history {
        println!(
            "    distributed history: {} classes, {} local / {} remote observations, {} active backoffs",
            history.classes, history.local_observations, history.remote_observations, history.active_backoffs
        );
    }
    println!("    Cargo L0: {}", status.installation.cargo_l0);
    println!(
        "    observed L1: {} hits / {} misses / {} bypasses / {} failures",
        status.installation.usage.hits,
        status.installation.usage.misses,
        status.installation.usage.bypasses,
        status.installation.usage.failures
    );
    println!(
        "    usage ledger: {} events (full: {})",
        status.installation.usage.recorded_events, status.installation.usage.ledger_full
    );
    println!(
        "    early bypass ledger: {} events (full: {}; incomplete tail: {})",
        status.installation.usage.early_bypasses,
        status.installation.usage.early_bypass_ledger_full,
        status.installation.usage.early_bypass_incomplete_tail
    );
    for (reason, count) in &status.installation.usage.early_bypass_reasons {
        println!("      {reason}: {count}");
    }
    for issue in &status.installation.issues {
        println!("    issue: {issue}");
    }
    if let Some(workspace) = &status.workspace {
        println!("  workspace");
        println!("    root: {}", workspace.root);
        println!("    bytes: {}", workspace.bytes);
        println!("    files: {}", workspace.files);
        println!("    directories: {}", workspace.directories);
        println!("    fully bounded: {}", workspace.fully_bounded);
        for artifact in &workspace.artifacts {
            let bound = artifact
                .max_bytes
                .map_or_else(|| "unbounded".to_string(), |bytes| bytes.to_string());
            println!(
                "    {}: {} bytes (bound: {}, path: {})",
                artifact.kind, artifact.bytes, bound, artifact.path
            );
        }
    }
    if let Some(local) = &status.local {
        println!("  local (shared across workspaces)");
        if let Some(cache) = &local.cache {
            println!("    root: {}", cache.root);
            println!("    trust domain: {}", cache.trust_domain);
            println!("    bytes: {} / {}", cache.bytes, cache.max_bytes);
            println!("    committed result bytes: {}", cache.committed_result_bytes);
            println!("    results: {}", cache.results);
            println!("    objects: {}", cache.objects);
            println!("    pins: {}", cache.pins);
            println!(
                "    native actions: {} ({} unique, {} conflicted, {} quarantined)",
                cache.native_actions, cache.native_unique, cache.native_conflicted, cache.native_quarantined
            );
            println!(
                "    native origins: {} local / {} remote",
                cache.native_local_origins, cache.native_remote_origins
            );
            println!(
                "    native terminal ledger: {} / {} bytes (disabled: {})",
                cache.native_ledger_bytes, cache.native_ledger_max_bytes, cache.native_ledger_disabled
            );
            println!("    active leases: {}", cache.active_leases);
            println!("    stale leases: {}", cache.stale_leases);
            println!(
                "    staging: {} entries / {} bytes",
                cache.staging_entries, cache.staging_bytes
            );
            println!("    reclaimable bytes: {}", cache.reclaimable_bytes);
            if let Some(oldest) = cache.oldest_used_unix_ms {
                println!("    oldest use (unix ms): {oldest}");
            }
            if let Some(newest) = cache.newest_used_unix_ms {
                println!("    newest use (unix ms): {newest}");
            }
        } else {
            println!("    current authority root: absent");
        }
    }
    if let Some(remote) = &status.remote {
        println!("  remote (machine-owned)");
        println!("    activation: {}", remote.activation);
        println!("    provider: {}", remote.provider);
        println!("    protocol: {}", remote.protocol);
        println!("    authority: {}", remote.authority);
        println!("    mode: {}", remote.mode);
        println!(
            "    shared compiler environment names: {}",
            remote.shared_environment_names
        );
    }
}
