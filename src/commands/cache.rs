//! Read-only cache status and explicitly scoped cache reclamation.

use super::TextJsonOutputFormat;
use super::cli::CacheScope;
use crate::cache::CacheStatus;
use crate::compiler::acquisition::process::BoundedProcessOutput;
use crate::error::{RailError, RailResult};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
            let cause = error.probe_failure_cause();
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
                        "cause": cause,
                        "message": error.to_string(),
                        "retry": cause.map(crate::remote_cache::RemoteProbeFailureCause::retry_guidance),
                      },
                    }),
                );
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                if let Some(cause) = cause {
                    println!("Remote cache probe failed ({failure}/{}): {error}", cause.as_str());
                    println!("Next: {}", cause.retry_guidance());
                } else {
                    println!("Remote cache probe failed ({failure}): {error}");
                }
            }
            Err(RailError::ExitWithCode { code: 2 })
        }
    }
}

/// Prove local cache readiness with one fixed, isolated compilation sequence.
pub(crate) fn run_ready(current_dir: &Path, format: TextJsonOutputFormat) -> RailResult<()> {
    let status = crate::cache::installation::status(current_dir)?;
    let profile_id = status.profile_id.as_deref().ok_or_else(|| {
        RailError::with_help(
            "cache readiness requires an enrolled workspace",
            "run cargo rail cache setup before the readiness probe",
        )
    })?;
    if !status.healthy || status.component_authentication != "authenticated" {
        return Err(RailError::with_help(
            "cache readiness requires an intact authenticated installation",
            "run cargo rail cache setup to repair the installation before the readiness probe",
        ));
    }
    if crate::remote_cache::configuration_status(current_dir)
        .map_err(|error| RailError::message(format!("remote cache configuration is unavailable: {error}")))?
        .is_some()
    {
        return Err(RailError::with_help(
            "local cache readiness cannot be proved while remote cache authority is active",
            "use a local-only cache profile for this probe, then qualify remote transport separately",
        ));
    }

    let cargo_config = Arc::new(crate::cargo::CargoConfigSnapshot::capture(current_dir)?);
    let inputs = crate::cargo::resolution::ResolutionInputs::capture_with_config(current_dir, cargo_config)?;
    let probe_parent = crate::cache::readiness_probe_parent(current_dir)?;
    let directory = tempfile::Builder::new()
        .prefix("cargo-rail-cache-readiness-")
        .tempdir_in(probe_parent)?;
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| RailError::message("system clock is before the Unix epoch"))?
            .as_nanos()
    );
    fs::create_dir(directory.path().join("src"))?;
    fs::write(
        directory.path().join("Cargo.toml"),
        b"[package]\nname = \"cargo-rail-cache-readiness-probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[workspace]\n",
    )?;
    let source = directory.path().join("src/main.rs");
    fs::write(
        &source,
        format!("fn main() {{ println!(\"cargo-rail-cache-readiness:{nonce}\"); }}\n"),
    )?;
    let uncached_target = directory.path().join("uncached-target");
    let cached_target = directory.path().join("cached-target");
    let cargo = inputs.toolchain.cargo_program();
    let manifest = directory.path().join("Cargo.toml");

    let uncached = run_readiness_cargo(cargo, current_dir, &manifest, &uncached_target, None, None, true)?;
    require_readiness_cargo_success("uncached", &uncached)?;
    let uncached_binary = readiness_binary(&uncached_target);
    require_readiness_binary(&uncached_binary, &nonce)?;

    let cold_report = directory.path().join("cold.json");
    let cold_coverage = directory.path().join("cold-coverage");
    fs::create_dir(&cold_coverage)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&cold_coverage, fs::Permissions::from_mode(0o700))?;
    }
    crate::cache::report::start(&cold_report)?;
    let cold = run_readiness_cargo(
        cargo,
        current_dir,
        &manifest,
        &cached_target,
        Some(&cold_report),
        Some(&cold_coverage),
        false,
    )?;
    require_readiness_cargo_success("cold cache", &cold)?;
    let cached_binary = readiness_binary(&cached_target);
    require_readiness_binary(&cached_binary, &nonce)?;
    let cold_digest = crate::source::ContentDigest::sha256(&fs::read(&cached_binary)?);
    let cold_measurements = crate::cache::report::finish(&cold_report)?;
    if cold_measurements.misses == 0 || cold_measurements.failures > 0 || cold_measurements.incomplete {
        let usage = crate::cache::installation::status(current_dir)?.usage;
        let coverage = readiness_coverage_reasons(&cold_coverage)?;
        return Err(RailError::message(format!(
            "cold cache readiness expected at least one miss and no failures; observed {} misses, {} failures (durable usage: {} hits, {} misses, {} bypasses, {} failures; early bypasses: {:?}; coverage: {:?}): {}",
            cold_measurements.misses,
            cold_measurements.failures,
            usage.hits,
            usage.misses,
            usage.bypasses,
            usage.failures,
            usage.early_bypass_reasons,
            coverage,
            bounded_command_stderr(&cold.stderr),
        )));
    }

    fs::remove_dir_all(&cached_target)?;
    let warm_report = directory.path().join("warm.json");
    let warm_coverage = directory.path().join("warm-coverage");
    fs::create_dir(&warm_coverage)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&warm_coverage, fs::Permissions::from_mode(0o700))?;
    }
    crate::cache::report::start(&warm_report)?;
    let warm = run_readiness_cargo(
        cargo,
        current_dir,
        &manifest,
        &cached_target,
        Some(&warm_report),
        Some(&warm_coverage),
        false,
    )?;
    require_readiness_cargo_success("warm cache", &warm)?;
    let warm_binary = readiness_binary(&cached_target);
    require_readiness_binary(&warm_binary, &nonce)?;
    let warm_digest = crate::source::ContentDigest::sha256(&fs::read(&warm_binary)?);
    let warm_measurements = crate::cache::report::finish(&warm_report)?;
    if warm_measurements.hits == 0 || warm_measurements.failures > 0 || warm_measurements.incomplete {
        return Err(RailError::message(format!(
            "warm cache readiness expected at least one hit and no failures; observed {} hits, {} failures",
            warm_measurements.hits, warm_measurements.failures
        )));
    }
    if cold_digest != warm_digest {
        return Err(RailError::message(
            "verified warm cache output differs from the cold compiler output",
        ));
    }
    let mut bypass_reasons = cold_measurements.bypass_reasons.clone();
    for (reason, count) in &warm_measurements.bypass_reasons {
        let total = bypass_reasons.entry(reason.clone()).or_default();
        *total = total.saturating_add(*count);
    }
    let measurements = crate::cache::report::Measurements {
        hits: warm_measurements.hits,
        misses: cold_measurements.misses,
        bypasses: cold_measurements.bypasses.saturating_add(warm_measurements.bypasses),
        failures: 0,
        local_bytes_read: warm_measurements.local_bytes_read,
        remote_bytes_read: 0,
        remote_bytes_written: 0,
        bypass_reasons,
        failure_reasons: Default::default(),
        incomplete: false,
    };
    crate::cache::record_readiness(
        current_dir,
        profile_id,
        inputs.toolchain.direct_rustc_verbose_version(),
        &measurements,
    )?;

    if format.is_json() {
        let output = crate::output::machine_json_envelope(
            "cache",
            "ready",
            "ready",
            0,
            serde_json::json!({
                "ready": true,
                "installation_integrity": status.installation_integrity,
                "component_authentication": status.component_authentication,
                "selected_toolchain_readiness": "ready",
                "workspace_enrollment": status.workspace_enrollment,
                "remote_authority": "not_configured",
                "observed_reuse": "verified_hit_observed",
                "uncached_success": true,
                "cold": cold_measurements,
                "warm": warm_measurements,
            }),
        );
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Cache ready: uncached success, cold miss, verified warm hit.");
    }
    Ok(())
}

fn run_readiness_cargo(
    cargo: &std::ffi::OsStr,
    current_dir: &Path,
    manifest: &Path,
    target: &Path,
    report: Option<&Path>,
    coverage: Option<&Path>,
    uncached: bool,
) -> RailResult<BoundedProcessOutput> {
    let mut command = Command::new(cargo);
    command
        .current_dir(current_dir)
        .args(["build", "--offline", "--quiet", "--manifest-path"])
        .arg(manifest)
        .arg("--target-dir")
        .arg(target)
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_TARGET_DIR", target)
        .env(crate::cache::installation::READINESS_WORKSPACE_ENV, current_dir)
        .env("CARGO_RAIL_CACHE_TRACE", "1")
        .env("RUSTUP_AUTO_INSTALL", "0")
        .env("RUSTUP_NO_UPDATE_CHECK", "1");
    if let Some(report) = report {
        command.env(crate::cache::report::REPORT_ENV, report);
    }
    if let Some(coverage) = coverage {
        command
            .env(
                crate::compiler::invocation::CACHE_CONTROL_ENV,
                crate::compiler::invocation::BENCH_COVERAGE_CACHE_CONTROL,
            )
            .env(crate::compiler::native_cache::BENCH_COVERAGE_DIRECTORY_ENV, coverage);
    }
    if uncached {
        command
            .env("RUSTC_WRAPPER", "")
            .env("CARGO_BUILD_RUSTC_WRAPPER", "")
            .env("RUSTC_WORKSPACE_WRAPPER", "")
            .env("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER", "");
    }
    crate::compiler::acquisition::process::run_bounded_process(&mut command, Duration::from_secs(180), 0, 16 * 1024)
        .map_err(Into::into)
}

fn readiness_coverage_reasons(directory: &Path) -> RailResult<Vec<(String, String)>> {
    const MAX_EVENTS: usize = 64;
    const MAX_EVENT_BYTES: u64 = 64 * 1024;

    let mut events = Vec::new();
    for entry in fs::read_dir(directory)? {
        if events.len() == MAX_EVENTS {
            events.push(("truncated".to_string(), "too_many_events".to_string()));
            break;
        }
        let entry = entry?;
        let Some(bytes) = crate::cache::read_bounded_private_file(&entry.path(), MAX_EVENT_BYTES)? else {
            continue;
        };
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        events.push((
            value["status"].as_str().unwrap_or("invalid").to_string(),
            value["reason"].as_str().unwrap_or("invalid").to_string(),
        ));
    }
    events.sort();
    Ok(events)
}

fn require_readiness_cargo_success(phase: &str, output: &BoundedProcessOutput) -> RailResult<()> {
    if output.status.success() {
        return Ok(());
    }
    Err(RailError::message(format!(
        "{phase} readiness compilation failed with status {}: {}",
        output.status,
        bounded_command_stderr(&output.stderr)
    )))
}

fn readiness_binary(target: &Path) -> PathBuf {
    target.join("debug").join(if cfg!(windows) {
        "cargo-rail-cache-readiness-probe.exe"
    } else {
        "cargo-rail-cache-readiness-probe"
    })
}

fn require_readiness_binary(binary: &Path, nonce: &str) -> RailResult<()> {
    let output = crate::compiler::acquisition::process::run_bounded_process(
        &mut Command::new(binary),
        Duration::from_secs(10),
        4096,
        4096,
    )?;
    let expected = format!("cargo-rail-cache-readiness:{nonce}\n");
    if !output.status.success() || !output.stderr.is_empty() || output.stdout != expected.as_bytes() {
        return Err(RailError::message(
            "cache readiness binary did not preserve normal compiler behavior",
        ));
    }
    Ok(())
}

fn bounded_command_stderr(stderr: &[u8]) -> String {
    const MAX_BYTES: usize = 4096;

    let start = stderr.len().saturating_sub(MAX_BYTES);
    String::from_utf8_lossy(&stderr[start..]).trim().to_string()
}

/// Preview or apply one exact transparent compiler-cache installation.
pub(crate) fn run_setup(
    current_dir: &Path,
    request: crate::cache::installation::SetupRequest,
    check: bool,
    format: TextJsonOutputFormat,
) -> RailResult<()> {
    if let Some(reason) = crate::compiler::native_cache::unsupported_native_cache_host_reason() {
        if format.is_json() {
            let output = crate::output::machine_json_envelope(
                "cache",
                if check { "setup_check" } else { "setup" },
                "unsupported",
                2,
                serde_json::json!({
                  "capability": "transparent_compiler_cache",
                  "supported": false,
                  "reason": reason,
                  "host_os": std::env::consts::OS,
                  "host_arch": std::env::consts::ARCH,
                  "fallback": "cargo",
                }),
            );
            println!("{}", serde_json::to_string_pretty(&output)?);
            return Err(RailError::ExitWithCode { code: 2 });
        }
        return Err(crate::cache::installation::unsupported_native_cache_host_error(reason));
    }
    if check && request == crate::cache::installation::SetupRequest::default() {
        let status = crate::cache::installation::status(current_dir)?;
        if status.healthy && status.state == "installed" && status.profile_id.is_some() {
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
      "private_state_action": plan.private_state_action(),
      "quarantine_receipt_path": plan.quarantine_receipt_path(),
      "profile_id": plan.profile_id(),
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
        error.context("run `just build`, then rerun `cargo rail cache setup --check`")
    } else {
        error.context("reinstall the complete cargo-rail component set, then rerun `cargo rail cache setup --check`")
    }
}

/// Preview or apply removal of the exact receipt-owned installation.
pub(crate) fn run_uninstall(current_dir: &Path, check: bool, format: TextJsonOutputFormat) -> RailResult<()> {
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
        render_installation_operation("uninstall_check", pending, &details, format)?;
        return if pending {
            Err(RailError::CheckHasPendingChanges)
        } else {
            Ok(())
        };
    }
    crate::cache::installation::apply_removal(plan)?;
    render_installation_operation("uninstall", false, &details, format)
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
            ("uninstall_check", true, _) => {
                println!("Global cache-wrapper uninstall pending.");
                println!("Next: cargo rail cache uninstall");
            }
            ("uninstall_check", false, _) | ("uninstall", false, false) => {
                println!("Global cache wrapper already uninstalled.")
            }
            ("uninstall", false, true) => println!("Global cache wrapper uninstalled; profiles preserved."),
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

pub(crate) fn run_report(start: Option<&Path>, finish: Option<&Path>, format: TextJsonOutputFormat) -> RailResult<()> {
    let (mode, measurements) = match (start, finish) {
        (Some(path), None) => {
            crate::cache::report::start(path)?;
            ("start", None)
        }
        (None, Some(path)) => ("finish", Some(crate::cache::report::finish(path)?)),
        _ => {
            return Err(RailError::message(
                "choose exactly one cache report operation: --start or --finish",
            ));
        }
    };
    if format.is_json() {
        println!(
            "{}",
            serde_json::to_string(&crate::output::machine_json_envelope(
                "cache",
                "report",
                "success",
                0,
                serde_json::json!({"operation": mode, "measurements": measurements})
            ))?
        );
    } else if let Some(counts) = measurements {
        println!(
            "Cache: {} reused, {} misses, {} bypasses, {} failures",
            counts.hits, counts.misses, counts.bypasses, counts.failures
        );
        if counts.incomplete {
            println!("Measurements are incomplete.");
        }
    } else {
        println!("Cache recording started. Set CARGO_RAIL_CACHE_REPORT to the recording path for subsequent commands.");
    }
    Ok(())
}

pub(crate) fn run_profiles(workspace_root: &Path, format: TextJsonOutputFormat) -> RailResult<()> {
    let cargo_home = crate::cache::installation::selected_cargo_home(workspace_root)?;
    let profiles = crate::cache::profile::list(&cargo_home)?;
    if format.is_json() {
        let output = crate::output::machine_json_envelope(
            "cache",
            "profiles",
            "success",
            0,
            serde_json::json!({
              "profiles": profiles,
            }),
        );
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Installed cache profiles: {}", profiles.len());
        for profile in profiles {
            println!(
                "  {}: {} root(s), {}, {}",
                profile.profile_id,
                profile.roots.len(),
                profile.state,
                profile.root_portability
            );
        }
    }
    Ok(())
}

pub(crate) fn run_detach(workspace_root: &Path, check: bool, format: TextJsonOutputFormat) -> RailResult<()> {
    let cargo_home = crate::cache::installation::selected_cargo_home(workspace_root)?;
    let plan = crate::cache::profile::plan_detach(&cargo_home, workspace_root)?;
    let pending = plan.pending();
    let reported_pending = check && pending;
    let profile_id = plan.profile_id().to_string();
    if !check {
        crate::cache::installation::stop_current_profile_coordinator(workspace_root)?;
        crate::cache::profile::apply_detach(&plan)?;
    }
    if format.is_json() {
        let output = crate::output::machine_json_envelope(
            "cache",
            if check { "detach_check" } else { "detach" },
            if check && pending { "pending_changes" } else { "success" },
            if check && pending { 1 } else { 0 },
            serde_json::json!({
              "pending": reported_pending,
              "profile_id": profile_id,
              "cache_preserved": true,
            }),
        );
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if pending {
        println!(
            "Workspace {} profile {}.",
            if check { "would detach from" } else { "detached from" },
            profile_id
        );
    } else {
        println!("Workspace has no installed cache profile.");
    }
    if check && pending {
        Err(RailError::CheckHasPendingChanges)
    } else {
        Ok(())
    }
}

pub(crate) fn run_drop_profile(
    workspace_root: &Path,
    profile_id: &str,
    check: bool,
    format: TextJsonOutputFormat,
) -> RailResult<()> {
    let cargo_home = crate::cache::installation::selected_cargo_home(workspace_root)?;
    let plan = crate::cache::profile::plan_removal(&cargo_home, profile_id)?;
    let pending = plan.pending();
    let details = serde_json::json!({
      "pending": pending,
      "profile_id": plan.profile_id(),
      "cache_root": plan.cache_root(),
      "state_root": plan.state_root(),
      "bytes": plan.bytes(),
    });
    if !check {
        crate::cache::profile::apply_removal(&plan)?;
    }
    if format.is_json() {
        let output = crate::output::machine_json_envelope(
            "cache",
            if check { "drop_profile_check" } else { "drop_profile" },
            if check && pending { "pending_changes" } else { "success" },
            if check && pending { 1 } else { 0 },
            details,
        );
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if pending {
        println!(
            "Profile {} {} ({} reclaimed).",
            plan.profile_id(),
            if check { "would be removed" } else { "removed" },
            human_bytes(plan.bytes())
        );
    } else {
        println!("Profile {} is not installed.", plan.profile_id());
    }
    if check && pending {
        Err(RailError::CheckHasPendingChanges)
    } else {
        Ok(())
    }
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
    // deleting the selected profile's CAS. Each removal then measures and mutates
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
    println!("Installation integrity: {}", status.installation.installation_integrity);
    println!(
        "Component authentication: {}",
        status.installation.component_authentication
    );
    println!("Selected toolchain: {}", status.selected_toolchain_readiness);
    println!("Workspace enrollment: {}", status.installation.workspace_enrollment);
    println!("Remote authority: {}", status.remote_authority);
    println!("Observed reuse: {}", status.installation.observed_reuse);
    println!(
        "Reuse: {} hits, {} misses, {} bypasses, {} failures",
        status.installation.usage.hits,
        status.installation.usage.misses,
        status.installation.usage.bypasses,
        status.installation.usage.failures
    );
    println!(
        "Installation storage: {} required, {} reclaimable",
        human_bytes(status.installation.required_bytes),
        human_bytes(status.installation.reclaimable_bytes)
    );
    if let Some(action) = status.installation.recovery_action {
        println!("Recovery: {action}");
    }
    if status.installation.usage.failure_reason_counts_available {
        for (reason, count) in &status.installation.usage.failure_reasons {
            println!("Cache failure {reason}: {count}");
        }
    } else {
        println!("Cache failure reasons: unavailable");
    }
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
    println!(
        "Failure reason counters: available={}",
        status.installation.usage.failure_reason_counts_available
    );
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
        println!(
            "Native restore synchronization: {} persistent shard(s); staging residue: {} entry(s)",
            cache.native_restore_lock_files, cache.staging_entries
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
