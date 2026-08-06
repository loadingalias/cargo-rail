//! Read-only cache status and explicitly scoped cache reclamation.

use super::TextJsonOutputFormat;
use super::cli::CacheScope;
use crate::cache::CacheStatus;
use crate::error::{RailError, RailResult};
use std::path::Path;

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
    removal.extend(crate::cache::remove_local()?)?;
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
  let bytes = status
    .workspace
    .as_ref()
    .map_or(0, |workspace| workspace.bytes)
    .saturating_add(
      status
        .local
        .as_ref()
        .and_then(|local| local.cache.as_ref())
        .map_or(0, |local| local.bytes),
    );
  bytes.saturating_add(
    status
      .local
      .as_ref()
      .and_then(|local| local.legacy.as_ref())
      .map_or(0, |legacy| legacy.bytes),
  )
}

fn render_status(status: &CacheStatus) {
  println!("cache status");
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
    if let Some(legacy) = &local.legacy {
      println!("    legacy reclaim-only root: {} bytes ({})", legacy.bytes, legacy.root);
    }
  }
  if let Some(remote) = &status.remote {
    println!("  remote (machine-owned)");
    println!("    alias: {}", remote.alias);
    println!("    transport: {}", remote.transport);
    println!("    authority: {}", remote.authority);
    println!("    role: {}", remote.role);
    println!(
      "    shared compiler environment names: {}",
      remote.shared_environment_names
    );
  }
}
