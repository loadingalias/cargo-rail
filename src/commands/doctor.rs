//! Read-only workspace and toolchain diagnostics.

use crate::commands::common::TextJsonOutputFormat;
use crate::error::{RailError, RailResult};
use crate::workspace::WorkspaceContext;

/// Report the exact native-cache identity represented by the captured rustc toolchain.
pub fn run_native_cache_doctor(ctx: &WorkspaceContext, format: TextJsonOutputFormat) -> RailResult<()> {
    if format.is_json() {
        crate::output::set_json_mode(true);
    }

    let capability = crate::compiler::collector::native_cache_capability(ctx.snapshot()?)?;
    let installation = crate::cache::installation::status(ctx.workspace_root())?;
    let remote = crate::remote_cache::configuration_status(ctx.workspace_root())
        .map_err(|error| RailError::message(format!("remote cache configuration is unavailable: {error}")))?;
    if format.is_json() {
        let payload = serde_json::json!({
          "capability": capability,
          "installation": installation,
          "repair": "cargo rail cache setup",
          "remote": remote,
        });
        let output = crate::output::machine_json_envelope("doctor", "native_cache", "success", 0, payload);
        println!(
            "{}",
            serde_json::to_string_pretty(&output)
                .map_err(|error| RailError::message(format!("failed to render native-cache capability: {error}")))?
        );
        return Ok(());
    }

    println!("native-cache compiler identity");
    println!("  platform: {}", capability.platform());
    println!("  host target: {}", capability.host_target());
    println!("  identity: {}", capability.identity());
    println!(
        "  transported work: {}",
        crate::compiler::native_cache::native_cache_transported_work_boundary()
    );
    println!("  installation: {}", installation.state);
    println!("  installation healthy: {}", installation.healthy);
    for issue in &installation.issues {
        println!("  installation issue: {issue}");
    }
    if !installation.healthy {
        println!("  repair: cargo rail cache setup");
    }
    if let Some(remote) = remote {
        println!("  remote activation: {}", remote.activation);
        println!("  remote provider: {}", remote.provider);
        println!("  remote protocol: {}", remote.protocol);
        println!("  remote authority: {}", remote.authority);
        println!("  remote mode: {}", remote.mode);
    } else {
        println!("  remote: not configured");
    }
    Ok(())
}
