//! Read-only workspace and toolchain diagnostics.

use crate::commands::common::TextJsonOutputFormat;
use crate::error::{RailError, RailResult};
use crate::workspace::WorkspaceContext;

/// Report the exact native-cache capability represented by the captured toolchain.
pub fn run_native_cache_doctor(ctx: &WorkspaceContext, format: TextJsonOutputFormat) -> RailResult<()> {
  if format.is_json() {
    crate::output::set_json_mode(true);
  }

  let capability = crate::compiler::collector::native_cache_capability(ctx.snapshot()?)?;
  if format.is_json() {
    let payload = serde_json::json!({ "capability": capability });
    let output = crate::output::machine_json_envelope("doctor", "native_cache", "success", 0, payload);
    println!(
      "{}",
      serde_json::to_string_pretty(&output)
        .map_err(|error| RailError::message(format!("failed to render native-cache capability: {error}")))?
    );
    return Ok(());
  }

  println!("native-cache capability");
  println!("  platform: {}", capability.platform());
  println!("  host target: {}", capability.host_target());
  println!("  identity: {}", capability.identity());
  println!(
    "  certificate: {}",
    if capability.is_certified() {
      "certified"
    } else {
      "not certified"
    }
  );
  if let Some(evidence) = capability.evidence() {
    println!("  evidence: {evidence}");
  }
  Ok(())
}
