//! Read-only workspace and toolchain diagnostics.

use crate::commands::common::TextJsonOutputFormat;
use crate::error::{RailError, RailResult};
use crate::workspace::WorkspaceContext;

/// Report the exact native-cache identity represented by the captured toolchain.
pub fn run_native_cache_doctor(ctx: &WorkspaceContext, format: TextJsonOutputFormat) -> RailResult<()> {
  if format.is_json() {
    crate::output::set_json_mode(true);
  }

  let capability = crate::compiler::collector::native_cache_capability(ctx.snapshot()?)?;
  let alias = ctx.config().and_then(|config| config.cache.l2.as_deref());
  let remote = crate::remote_cache::probe(ctx.workspace_root(), alias)
    .map_err(|error| RailError::message(format!("remote cache probe failed: {error}")))?;
  if format.is_json() {
    let payload = serde_json::json!({ "capability": capability, "remote": remote });
    let output = crate::output::machine_json_envelope("doctor", "native_cache", "success", 0, payload);
    println!(
      "{}",
      serde_json::to_string_pretty(&output)
        .map_err(|error| RailError::message(format!("failed to render native-cache capability: {error}")))?
    );
    return Ok(());
  }

  println!("native-cache toolchain identity");
  println!("  platform: {}", capability.platform());
  println!("  host target: {}", capability.host_target());
  println!("  identity: {}", capability.identity());
  if let Some(remote) = remote {
    println!("  remote alias: {}", remote.alias);
    println!("  remote transport: {}", remote.transport);
    println!("  remote authority: {}", remote.authority);
    println!("  remote role: {}", remote.role);
  } else {
    println!("  remote: not configured");
  }
  Ok(())
}
