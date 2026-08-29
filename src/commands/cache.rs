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
        println!("Normalized remote cache URL: {}", selection.normalized_url());
        if crate::output::is_verbose() {
            println!(
                "Provider: {}; protocol: {}; mode: {}",
                status.provider, status.protocol, status.mode
            );
            println!("Shared compiler environments: {}", status.shared_environment_names);
        }
    }
    Ok(())
}

/// Authenticate the selected object store and validate its protocol marker.
pub(crate) fn run_probe(current_dir: &Path, format: TextJsonOutputFormat) -> RailResult<()> {
    match crate::remote_cache::probe(current_dir) {
        Ok(probe) => {
            if format.is_json() {
                let output = crate::output::machine_json_envelope(
                    "cache",
                    "probe",
                    "ready",
                    0,
                    serde_json::json!({
                      "ready": true,
                      "remote": probe.remote,
                      "protocol_marker": probe.protocol_marker,
                    }),
                );
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                println!("Remote cache ready.");
                if crate::output::is_verbose() {
                    println!(
                        "Provider: {}; protocol: {}; mode: {}",
                        probe.remote.provider, probe.remote.protocol, probe.remote.mode
                    );
                    println!("Protocol marker: {}", probe.protocol_marker.as_str());
                }
            }
            Ok(())
        }
        Err(error) => {
            let failure = error.probe_failure();
            if format.is_json() {
                let output = crate::output::machine_json_envelope(
                    "cache",
                    "probe",
                    "probe_failed",
                    2,
                    serde_json::json!({
                      "ready": false,
                      "failure": {
                        "kind": failure,
                        "message": error.to_string(),
                      },
                    }),
                );
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                println!("Remote cache probe failed ({failure}): {error}");
            }
            Err(RailError::ExitWithCode { code: 2 })
        }
    }
}

/// Preview or apply one exact transparent compiler-cache installation.
pub(crate) fn run_setup(
    current_dir: &Path,
    request: crate::cache::installation::SetupRequest,
    check: bool,
    format: TextJsonOutputFormat,
) -> RailResult<()> {
    if check && request == crate::cache::installation::SetupRequest::default() {
        let status = crate::cache::installation::status(current_dir)?;
        if status.healthy && status.state == "installed" {
            let details = serde_json::json!({
              "changed": false,
              "config_path": status.config_path,
              "config_field": "build.rustc-wrapper",
              "config_action": "unchanged",
              "wrapper_path": status.wrapper_path,
              "receipt_path": null,
              "private_state_action": "verify",
              "cache_base": status.cache_base,
            });
            return render_installation_operation("setup_check", false, &details, format);
        }
    }
    let plan = crate::cache::installation::plan_setup(current_dir, &request)
        .map_err(|error| cache_setup_source_error(current_dir, error))?;
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
      "root_portability": plan.root_portability(),
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

fn cache_setup_source_error(current_dir: &Path, error: RailError) -> RailError {
    if !error
        .to_string()
        .contains("compiler cache worker executable is unavailable")
    {
        return error;
    }
    let source_checkout = std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok())
        .zip(current_dir.canonicalize().ok())
        .is_some_and(|(executable, root)| executable.starts_with(root.join("target")));
    if source_checkout {
        error.context("run `just build-all`, then rerun `cargo rail cache setup --check`")
    } else {
        error.context("reinstall the complete cargo-rail component set, then rerun `cargo rail cache setup --check`")
    }
}

/// Preview or apply removal of the exact receipt-owned installation.
pub(crate) fn run_remove(current_dir: &Path, check: bool, format: TextJsonOutputFormat) -> RailResult<()> {
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
    let changed = details["changed"].as_bool().unwrap_or(false);
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
        match (operation, pending, changed) {
            ("setup_check", true, _) => {
                println!("Cache setup pending.");
                println!("Next: cargo rail cache setup");
            }
            ("setup_check", false, _) | ("setup", false, false) => println!("Cache already configured."),
            ("setup", false, true) => println!("Cache repaired."),
            ("remove_check", true, _) => {
                println!("Cache removal pending.");
                println!("Next: cargo rail cache remove");
            }
            ("remove_check", false, _) | ("remove", false, false) => println!("Cache already removed."),
            ("remove", false, true) => println!("Cache removed."),
            _ => println!("Cache operation complete."),
        }
        if crate::output::is_verbose() {
            println!("Cargo config: {}", details["config_path"].as_str().unwrap_or("unknown"));
            if let Some(wrapper) = details["wrapper_path"].as_str() {
                println!("Wrapper: {wrapper}");
            }
            if let Some(receipt) = details["receipt_path"].as_str() {
                println!("Receipt: {receipt}");
            }
        }
    }
    Ok(())
}

/// Report selected cache scopes without creating workspace context or cache state.
pub(crate) fn run_status(workspace_root: &Path, scope: CacheScope, format: TextJsonOutputFormat) -> RailResult<()> {
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
        println!("  retained: {}", human_bytes(plan.bytes));
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
            "Reclaimed {} from {} cache path(s).",
            human_bytes(removal.bytes),
            removal.paths.len()
        );
        if crate::output::is_verbose() {
            for path in &removal.paths {
                println!("  {path}");
            }
        }
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
    println!(
        "Installation: {} ({})",
        if status.installation.healthy {
            "healthy"
        } else {
            "unhealthy"
        },
        status.installation.state
    );
    println!(
        "Reuse: {} hits, {} misses, {} bypasses, {} failures",
        status.installation.usage.hits,
        status.installation.usage.misses,
        status.installation.usage.bypasses,
        status.installation.usage.failures
    );
    for issue in &status.installation.issues {
        println!("Warning: {issue}");
    }
    if let Some(workspace) = &status.workspace {
        println!(
            "Workspace: {} in {} file(s)",
            human_bytes(workspace.bytes),
            workspace.files
        );
    }
    if let Some(local) = &status.local {
        if let Some(cache) = &local.cache {
            println!(
                "Local cache: {} / {} ({} results, {} objects)",
                human_bytes(cache.bytes),
                human_bytes(cache.max_bytes),
                cache.results,
                cache.objects
            );
        } else {
            println!("Local cache: absent");
        }
    }
    if crate::output::is_verbose() {
        render_verbose_status(status);
    }
}

fn render_verbose_status(status: &CacheStatus) {
    println!("Cargo home: {}", status.installation.cargo_home);
    println!("Cargo config: {}", status.installation.config_path);
    if let Some(wrapper) = &status.installation.wrapper_path {
        println!("Wrapper: {wrapper}");
    }
    println!(
        "Usage ledger: {} event(s); full={}",
        status.installation.usage.recorded_events, status.installation.usage.ledger_full
    );
    println!(
        "Early bypass ledger: {} event(s); full={}; incomplete_tail={}",
        status.installation.usage.early_bypasses,
        status.installation.usage.early_bypass_ledger_full,
        status.installation.usage.early_bypass_incomplete_tail
    );
    for (reason, count) in &status.installation.usage.early_bypass_reasons {
        println!("  bypass {reason}: {count}");
    }
    if let Some(workspace) = &status.workspace {
        println!("Workspace root: {}", workspace.root);
        for artifact in &workspace.artifacts {
            println!(
                "  {}: {} at {}",
                artifact.kind,
                human_bytes(artifact.bytes),
                artifact.path
            );
        }
    }
    if let Some(local) = &status.local
        && let Some(cache) = &local.cache
    {
        println!("Local root: {}", cache.root);
        println!("Trust domain: {}", cache.trust_domain);
        println!(
            "Native actions: {} ({} unique, {} conflicted, {} quarantined)",
            cache.native_actions, cache.native_unique, cache.native_conflicted, cache.native_quarantined
        );
        println!("Leases: {} active, {} stale", cache.active_leases, cache.stale_leases);
        println!("Reclaimable: {}", human_bytes(cache.reclaimable_bytes));
    }
    if let Some(remote) = &status.remote {
        println!(
            "Remote: {} via {} ({}; {})",
            remote.activation, remote.provider, remote.protocol, remote.mode
        );
    }
}

pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
