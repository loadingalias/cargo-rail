//! Front-door coverage for transparent local compiler-cache installation.

use anyhow::{Context as _, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::helpers::TestWorkspace;

fn rail(workspace: &Path, cargo_home: &Path, arguments: &[&str]) -> Result<Output> {
  Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(workspace)
    .args(arguments)
    .env("CARGO_HOME", cargo_home)
    .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .output()
    .context("run cargo-rail cache command")
}

fn cargo_check(workspace: &Path, cargo_home: &Path, rustc: Option<&Path>, cache: Option<&str>) -> Result<Output> {
  let mut command = Command::new("cargo");
  command
    .current_dir(workspace)
    .args(["check", "--quiet"])
    .env("CARGO_HOME", cargo_home)
    .env("CARGO_INCREMENTAL", "0")
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER");
  if let Some(rustc) = rustc {
    command.env("RUSTC", rustc);
  }
  if let Some(cache) = cache {
    command.env("CARGO_RAIL_CACHE", cache);
  }
  command.output().context("run isolated cargo check")
}

fn json(output: &Output) -> Result<serde_json::Value> {
  serde_json::from_slice(&output.stdout).context("decode command JSON")
}

#[test]
fn setup_preview_apply_repeat_status_and_exact_remove_are_lossless() -> Result<()> {
  let workspace = TestWorkspace::new_single_crate("transparent-setup", "0.1.0")?;
  let cargo_home = tempfile::tempdir()?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(cargo_home.path(), fs::Permissions::from_mode(0o755))?;
  }
  let config = cargo_home.path().join("config.toml");
  let original = "# retained\n[net]\noffline = true\n";
  fs::write(&config, original)?;

  let check = rail(
    &workspace.path,
    cargo_home.path(),
    &["rail", "cache", "setup", "--check", "-f", "json"],
  )?;
  assert_eq!(
    check.status.code(),
    Some(1),
    "setup preview must report changes: {check:?}"
  );
  assert_eq!(
    fs::read_to_string(&config)?,
    original,
    "setup preview mutated Cargo config"
  );
  assert!(!cargo_home.path().join("cargo-rail/compiler-cache-v1").exists());

  let apply = rail(
    &workspace.path,
    cargo_home.path(),
    &["rail", "cache", "setup", "--max-size", "32MiB", "-f", "json"],
  )?;
  assert!(apply.status.success(), "setup failed: {apply:?}");
  let applied = json(&apply)?;
  assert_eq!(applied["changed"], true);
  assert_eq!(applied["pending"], false);
  assert_eq!(applied["max_bytes"], 32 * 1024 * 1024);
  let configured = fs::read_to_string(&config)?;
  assert!(configured.starts_with(original));
  assert!(configured.contains("rustc-wrapper"));
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(fs::metadata(cargo_home.path())?.permissions().mode() & 0o777, 0o755);
  }

  let repeated = rail(
    &workspace.path,
    cargo_home.path(),
    &["rail", "cache", "setup", "--check", "-f", "json"],
  )?;
  assert!(
    repeated.status.success(),
    "repeat setup was not idempotent: {repeated:?}"
  );
  assert_eq!(json(&repeated)?["pending"], false);

  let status = rail(
    &workspace.path,
    cargo_home.path(),
    &["rail", "cache", "status", "--scope", "local", "-f", "json"],
  )?;
  assert!(status.status.success(), "installation status failed: {status:?}");
  let status = json(&status)?;
  assert_eq!(status["status"]["schema_version"], 8);
  assert_eq!(status["status"]["installation"]["state"], "installed");
  assert_eq!(status["status"]["installation"]["healthy"], true);
  let wrapper = PathBuf::from(
    status["status"]["installation"]["wrapper_path"]
      .as_str()
      .context("installed wrapper path")?,
  );

  fs::remove_file(&wrapper)?;
  let drifted = rail(
    &workspace.path,
    cargo_home.path(),
    &["rail", "cache", "status", "--scope", "local", "-f", "json"],
  )?;
  assert_eq!(json(&drifted)?["status"]["installation"]["state"], "drifted");
  let repair_check = rail(
    &workspace.path,
    cargo_home.path(),
    &["rail", "cache", "setup", "--check"],
  )?;
  assert_eq!(repair_check.status.code(), Some(1));
  let repair = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
  assert!(repair.status.success(), "setup repair failed: {repair:?}");
  assert!(wrapper.is_file(), "setup repair did not restore the owned wrapper");
  #[cfg(not(windows))]
  let worker = wrapper.with_file_name("cargo-rail-native-rustc-worker");
  #[cfg(windows)]
  let worker = wrapper.with_file_name("cargo-rail-native-rustc-worker.exe");
  assert!(worker.is_file(), "setup did not install the owned worker");
  fs::remove_file(&worker)?;
  let drifted = rail(
    &workspace.path,
    cargo_home.path(),
    &["rail", "cache", "status", "--scope", "local", "-f", "json"],
  )?;
  assert_eq!(json(&drifted)?["status"]["installation"]["state"], "drifted");
  let repair = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
  assert!(repair.status.success(), "worker repair failed: {repair:?}");
  assert!(worker.is_file(), "setup repair did not restore the owned worker");

  let remove_check = rail(
    &workspace.path,
    cargo_home.path(),
    &["rail", "cache", "remove", "--check", "-f", "json"],
  )?;
  assert_eq!(remove_check.status.code(), Some(1));
  assert!(config.exists(), "removal preview mutated Cargo config");
  let remove = rail(
    &workspace.path,
    cargo_home.path(),
    &["rail", "cache", "remove", "-f", "json"],
  )?;
  assert!(remove.status.success(), "removal failed: {remove:?}");
  assert_eq!(fs::read_to_string(&config)?, original);
  assert!(!cargo_home.path().join("cargo-rail/compiler-cache-v1").exists());
  assert!(
    cargo_home.path().join("cargo-rail/local-cas-v2").exists(),
    "removal deleted the compiler-result cache"
  );
  Ok(())
}

#[test]
fn setup_refuses_global_conflicts_and_workspace_shadowing() -> Result<()> {
  let workspace = TestWorkspace::new_single_crate("transparent-conflict", "0.1.0")?;
  let cargo_home = tempfile::tempdir()?;
  let config = cargo_home.path().join("config.toml");
  fs::write(&config, "[build]\nrustc-wrapper = 'sccache'\n")?;
  let conflict = rail(
    &workspace.path,
    cargo_home.path(),
    &["rail", "cache", "setup", "--check"],
  )?;
  assert_eq!(conflict.status.code(), Some(2));
  assert!(String::from_utf8_lossy(&conflict.stderr).contains("already selects rustc wrapper"));

  fs::write(&config, "[net]\noffline = true\n")?;
  for name in ["RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER"] {
    let shadowed = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
      .current_dir(&workspace.path)
      .args(["rail", "cache", "setup", "--check"])
      .env("CARGO_HOME", cargo_home.path())
      .env(name, "environment-wrapper")
      .env_remove(if name == "RUSTC_WRAPPER" {
        "CARGO_BUILD_RUSTC_WRAPPER"
      } else {
        "RUSTC_WRAPPER"
      })
      .output()?;
    assert_eq!(shadowed.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&shadowed.stderr).contains("shadows Cargo's user rustc-wrapper setting"));
  }
  fs::create_dir_all(workspace.path.join(".cargo"))?;
  fs::write(
    workspace.path.join(".cargo/config.toml"),
    "[build]\nrustc-wrapper = 'workspace-wrapper'\n",
  )?;
  let shadowed = rail(
    &workspace.path,
    cargo_home.path(),
    &["rail", "cache", "setup", "--check"],
  )?;
  assert_eq!(shadowed.status.code(), Some(2));
  assert!(String::from_utf8_lossy(&shadowed.stderr).contains("shadows the user rustc-wrapper setting"));
  assert_eq!(fs::read_to_string(config)?, "[net]\noffline = true\n");
  Ok(())
}

#[test]
fn cache_status_labels_retained_remote_configuration_as_inactive() -> Result<()> {
  let workspace = TestWorkspace::new_single_crate("transparent-remote-status", "0.1.0")?;
  let cargo_home = tempfile::tempdir()?;
  fs::create_dir_all(workspace.path.join(".config"))?;
  fs::write(workspace.path.join(".config/rail.toml"), "[cache]\nl2 = 'team'\n")?;
  let target_map = cargo_home.path().join("targets.json");
  fs::write(
    &target_map,
    r#"{"version":1,"targets":{"team":{"protocol":"s3","region":"us-east-1","expected_bucket_owner":"123456789012","bucket":"cargo-rail-cache-fixture","prefix":"cache","role":"read","shareable_environment":[]}}}"#,
  )?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(&target_map, fs::Permissions::from_mode(0o600))?;
  }
  let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(&workspace.path)
    .args(["rail", "cache", "status", "--scope", "local", "-f", "json"])
    .env("CARGO_HOME", cargo_home.path())
    .env("CARGO_RAIL_CACHE_TARGETS_FILE", &target_map)
    .env_remove("RUSTC_WRAPPER")
    .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
    .output()?;
  assert!(output.status.success(), "configuration-only status failed: {output:?}");
  let value = json(&output)?;
  assert_eq!(value["status"]["schema_version"], 8);
  assert_eq!(
    value["status"]["remote"]["activation"],
    "configuration_only_transparent_cache_is_local"
  );
  Ok(())
}

#[cfg(unix)]
#[test]
fn direct_cargo_reuses_verified_outputs_and_off_never_touches_l1() -> Result<()> {
  use std::os::unix::fs::PermissionsExt as _;

  let workspace = TestWorkspace::new_single_crate("transparent-hit", "0.1.0")?;
  let cargo_home = tempfile::tempdir()?;
  let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
  assert!(setup.status.success(), "setup failed: {setup:?}");
  let cold = cargo_check(&workspace.path, cargo_home.path(), None, None)?;
  assert!(cold.status.success(), "cold compilation failed: {cold:?}");
  fs::remove_dir_all(workspace.path.join("target"))?;

  let real_rustc = Path::new("rustc");
  let shim = workspace.path.join("rustc-hit-proof");
  fs::write(
    &shim,
    "#!/bin/sh\nif [ -n \"$CACHE_ENV_LOG\" ]; then printf '%s\\n' \"${CARGO_RAIL_CACHE-unset}\" >> \"$CACHE_ENV_LOG\"; fi\nfor arg in \"$@\"; do\n  if [ \"$arg\" = \"transparent_hit\" ]; then exit 91; fi\ndone\nexec \"$REAL_RUSTC\" \"$@\"\n",
  )?;
  fs::set_permissions(&shim, fs::Permissions::from_mode(0o700))?;
  let hit = Command::new("cargo")
    .current_dir(&workspace.path)
    .args(["check", "--quiet"])
    .env("CARGO_HOME", cargo_home.path())
    .env("CARGO_INCREMENTAL", "0")
    .env("RUSTC", &shim)
    .env("REAL_RUSTC", real_rustc)
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .output()?;
  assert!(
    hit.status.success(),
    "verified hit executed the rejecting rustc shim: {hit:?}"
  );

  let observed = rail(
    &workspace.path,
    cargo_home.path(),
    &["rail", "cache", "status", "--scope", "local", "-f", "json"],
  )?;
  let observed = json(&observed)?;
  let installed_wrapper = PathBuf::from(
    observed["status"]["installation"]["wrapper_path"]
      .as_str()
      .context("installed wrapper path")?,
  );
  assert!(
    observed["status"]["installation"]["usage"]["misses"]
      .as_u64()
      .unwrap_or_default()
      >= 1
  );
  assert!(
    observed["status"]["installation"]["usage"]["hits"]
      .as_u64()
      .unwrap_or_default()
      >= 1
  );

  fs::remove_dir_all(workspace.path.join("target"))?;
  let cache_root = cargo_home.path().join("cargo-rail/local-cas-v2");
  let before = directory_snapshot(&cache_root)?;
  #[cfg(not(windows))]
  let installed_worker = installed_wrapper.with_file_name("cargo-rail-native-rustc-worker");
  #[cfg(windows)]
  let installed_worker = installed_wrapper.with_file_name("cargo-rail-native-rustc-worker.exe");
  fs::remove_file(installed_worker)?;
  let off = Command::new("cargo")
    .current_dir(&workspace.path)
    .args(["check", "--quiet"])
    .env("CARGO_HOME", cargo_home.path())
    .env("CARGO_INCREMENTAL", "0")
    .env("CARGO_RAIL_CACHE", "off")
    .env("CACHE_ENV_LOG", workspace.path.join("cache-env.log"))
    .env("RUSTC", &shim)
    .env("REAL_RUSTC", real_rustc)
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .output()?;
  assert!(
    !off.status.success(),
    "cache opt-out did not execute the original compiler"
  );
  assert_eq!(
    directory_snapshot(&cache_root)?,
    before,
    "cache opt-out touched L1 state"
  );
  let observed_environment = fs::read_to_string(workspace.path.join("cache-env.log"))?;
  assert!(
    observed_environment.lines().all(|value| value == "off"),
    "cache opt-out changed the compiler environment: {observed_environment:?}"
  );
  Ok(())
}

#[test]
fn unsupported_shapes_bypass_before_acquisition_while_proc_macro_producers_remain_cacheable() -> Result<()> {
  let workspace = TestWorkspace::new_single_crate("transparent-early-bypass", "0.1.0")?;
  let cargo_home = tempfile::tempdir()?;
  let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
  assert!(setup.status.success(), "setup failed: {setup:?}");
  let installation = cargo_home.path().join("cargo-rail/compiler-cache-v1");
  let cache_root = cargo_home.path().join("cargo-rail/local-cas-v2");
  let before = directory_snapshot(&cache_root)?;

  let incremental = Command::new("cargo")
    .current_dir(&workspace.path)
    .args(["check", "--quiet"])
    .env("CARGO_HOME", cargo_home.path())
    .env_remove("CARGO_INCREMENTAL")
    .env_remove("RUSTC_FORCE_INCREMENTAL")
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .output()?;
  assert!(
    incremental.status.success(),
    "incremental bypass failed: {incremental:?}"
  );
  assert!(!installation.join("session.json").exists());
  assert!(!installation.join("usage-v1.log").exists());
  assert_eq!(
    directory_snapshot(&cache_root)?,
    before,
    "incremental bypass touched L1"
  );

  let clippy = Command::new("cargo")
    .current_dir(&workspace.path)
    .args(["clippy", "--quiet"])
    .env("CARGO_HOME", cargo_home.path())
    .env_remove("CARGO_INCREMENTAL")
    .env_remove("RUSTC_FORCE_INCREMENTAL")
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .output()?;
  assert!(clippy.status.success(), "clippy bypass failed: {clippy:?}");
  assert!(!installation.join("session.json").exists());
  assert!(!installation.join("usage-v1.log").exists());
  assert_eq!(directory_snapshot(&cache_root)?, before, "clippy bypass touched L1");

  let custom_target = Command::new("cargo")
    .current_dir(&workspace.path)
    .args(["check", "--quiet"])
    .env("CARGO_HOME", cargo_home.path())
    .env("CARGO_INCREMENTAL", "0")
    .env("CARGO_TARGET_DIR", workspace.path.join("custom-target"))
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .output()?;
  assert!(
    custom_target.status.success(),
    "custom target-dir bypass failed: {custom_target:?}"
  );
  assert!(!installation.join("session.json").exists());
  assert!(!installation.join("usage-v1.log").exists());
  assert_eq!(
    directory_snapshot(&cache_root)?,
    before,
    "custom target-dir bypass touched L1"
  );

  fs::create_dir_all(workspace.path.join("fixture-macros/src"))?;
  fs::write(
    workspace.path.join("fixture-macros/Cargo.toml"),
    r#"[package]
name = "fixture-macros"
version = "0.1.0"
edition = "2024"

[lib]
proc-macro = true
"#,
  )?;
  fs::write(
    workspace.path.join("fixture-macros/src/lib.rs"),
    r#"extern crate proc_macro;

use proc_macro::TokenStream;

#[proc_macro_derive(Fixture)]
pub fn derive_fixture(_: TokenStream) -> TokenStream {
  TokenStream::new()
}
"#,
  )?;
  fs::write(
    workspace.path.join("Cargo.toml"),
    r#"[package]
name = "transparent-early-bypass"
version = "0.1.0"
edition = "2024"

[dependencies]
fixture-macros = { path = "fixture-macros" }

[workspace]
members = ["fixture-macros"]
resolver = "3"
"#,
  )?;
  fs::write(
    workspace.path.join("src/lib.rs"),
    "#[derive(fixture_macros::Fixture)]\npub struct Fixture;\n",
  )?;
  let coverage = tempfile::tempdir()?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(coverage.path(), fs::Permissions::from_mode(0o700))?;
  }
  let coverage_path = fs::canonicalize(coverage.path())?;
  let proc_macro = Command::new("cargo")
    .current_dir(&workspace.path)
    .args(["check", "--workspace", "--quiet"])
    .env("CARGO_HOME", cargo_home.path())
    .env("CARGO_INCREMENTAL", "0")
    .env("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1")
    .env("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY", &coverage_path)
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .output()?;
  assert!(proc_macro.status.success(), "proc-macro bypass failed: {proc_macro:?}");
  assert!(installation.join("session.json").is_file());
  assert!(installation.join("usage-v1.log").is_file());
  assert_ne!(
    directory_snapshot(&cache_root)?,
    before,
    "graduated proc-macro producer did not touch L1"
  );

  let mut producer_cached = false;
  let mut consumer_bypassed_before_acquisition = false;
  let mut event_summary = Vec::new();
  for entry in fs::read_dir(&coverage_path)? {
    let event: serde_json::Value = serde_json::from_slice(&fs::read(entry?.path())?)?;
    let arguments = event["arguments"].as_array().context("coverage arguments")?;
    let crate_name = arguments.windows(2).find_map(|pair| {
      (pair[0].as_str() == Some("--crate-name"))
        .then(|| pair[1].as_str())
        .flatten()
    });
    event_summary.push((
      crate_name.map(str::to_string),
      event["status"].as_str().map(str::to_string),
      event["reason"].as_str().map(str::to_string),
      event["action_key"].as_str().map(str::to_string),
    ));
    if crate_name == Some("fixture_macros") && matches!(event["status"].as_str(), Some("hit" | "miss")) {
      producer_cached |= event["action_key"].as_str().is_some();
    }
    let consumes_fixture_macro = crate_name == Some("transparent_early_bypass")
      && arguments.iter().any(|argument| {
        argument
          .as_str()
          .is_some_and(|argument| argument.starts_with("fixture_macros="))
      });
    if consumes_fixture_macro {
      assert_eq!(event["status"], "bypassed");
      assert_eq!(event["reason"], "dependency_artifact_class_not_graduated");
      assert!(event.get("action_key").is_none());
      assert_eq!(event["bytes_hashed"], 0);
      assert_eq!(event["cache_bytes_read"], 0);
      consumer_bypassed_before_acquisition = true;
    }
  }
  assert!(
    producer_cached,
    "proc-macro producer did not enter verified L1: {event_summary:?}"
  );
  assert!(
    consumer_bypassed_before_acquisition,
    "native proc-macro consumer did not retain its acquisition-free bypass: {event_summary:?}"
  );
  Ok(())
}

#[cfg(unix)]
#[test]
fn local_cache_outage_executes_cold_and_setup_repairs_the_same_authority() -> Result<()> {
  use std::os::unix::fs::PermissionsExt as _;

  let workspace = TestWorkspace::new_single_crate("transparent-outage", "0.1.0")?;
  let cargo_home = tempfile::tempdir()?;
  let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
  assert!(setup.status.success(), "setup failed: {setup:?}");
  let cold = cargo_check(&workspace.path, cargo_home.path(), None, None)?;
  assert!(cold.status.success(), "cold seed failed: {cold:?}");
  fs::remove_dir_all(workspace.path.join("target"))?;
  fs::remove_dir_all(cargo_home.path().join("cargo-rail/local-cas-v2"))?;

  let log = workspace.path.join("outage-rustc.log");
  let shim = workspace.path.join("rustc-outage-proof");
  fs::write(
    &shim,
    "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$RUSTC_LOG\"\nexec \"$REAL_RUSTC\" \"$@\"\n",
  )?;
  fs::set_permissions(&shim, fs::Permissions::from_mode(0o700))?;
  let outage = Command::new("cargo")
    .current_dir(&workspace.path)
    .args(["check", "--quiet"])
    .env("CARGO_HOME", cargo_home.path())
    .env("CARGO_INCREMENTAL", "0")
    .env("RUSTC", &shim)
    .env("REAL_RUSTC", "rustc")
    .env("RUSTC_LOG", &log)
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .output()?;
  assert!(outage.status.success(), "cache outage did not compile cold: {outage:?}");
  assert!(fs::read_to_string(&log)?.contains("transparent_outage"));

  let status = rail(
    &workspace.path,
    cargo_home.path(),
    &["rail", "cache", "status", "--scope", "local", "-f", "json"],
  )?;
  assert_eq!(json(&status)?["status"]["installation"]["state"], "drifted");
  let check = rail(
    &workspace.path,
    cargo_home.path(),
    &["rail", "cache", "setup", "--check"],
  )?;
  assert_eq!(check.status.code(), Some(1));
  let repair = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
  assert!(repair.status.success(), "cache authority repair failed: {repair:?}");
  assert!(cargo_home.path().join("cargo-rail/local-cas-v2").is_dir());
  Ok(())
}

#[cfg(unix)]
#[test]
fn workspace_wrapper_composes_by_bypassing_and_recursive_composition_is_rejected() -> Result<()> {
  use std::os::unix::fs::PermissionsExt as _;

  let workspace = TestWorkspace::new_single_crate("transparent-wrapper-chain", "0.1.0")?;
  let cargo_home = tempfile::tempdir()?;
  let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
  assert!(setup.status.success(), "setup failed: {setup:?}");
  let status = rail(
    &workspace.path,
    cargo_home.path(),
    &["rail", "cache", "status", "--scope", "local", "-f", "json"],
  )?;
  let wrapper = json(&status)?["status"]["installation"]["wrapper_path"]
    .as_str()
    .context("wrapper path")?
    .to_string();

  let chain = workspace.path.join("workspace-rustc-wrapper");
  let log = workspace.path.join("workspace-wrapper.log");
  fs::write(
    &chain,
    "#!/bin/sh\nprintf 'called\\n' >> \"$WRAPPER_LOG\"\nexec \"$@\"\n",
  )?;
  fs::set_permissions(&chain, fs::Permissions::from_mode(0o700))?;
  fs::create_dir_all(workspace.path.join(".cargo"))?;
  fs::write(
    workspace.path.join(".cargo/config.toml"),
    format!("[build]\nrustc-workspace-wrapper = '{}'\n", chain.display()),
  )?;
  let composed = Command::new("cargo")
    .current_dir(&workspace.path)
    .args(["check", "--quiet"])
    .env("CARGO_HOME", cargo_home.path())
    .env("WRAPPER_LOG", &log)
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .output()?;
  assert!(
    composed.status.success(),
    "safe wrapper composition failed: {composed:?}"
  );
  assert!(fs::read_to_string(&log)?.contains("called"));

  fs::remove_dir_all(workspace.path.join("target"))?;
  fs::write(
    workspace.path.join(".cargo/config.toml"),
    format!("[build]\nrustc-workspace-wrapper = '{wrapper}'\n"),
  )?;
  let recursive = Command::new("cargo")
    .current_dir(&workspace.path)
    .args(["check", "--quiet"])
    .env("CARGO_HOME", cargo_home.path())
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .output()?;
  assert!(!recursive.status.success(), "recursive wrapper unexpectedly ran");
  assert!(
    String::from_utf8_lossy(&recursive.stderr).contains("recursive transparent wrapper configuration"),
    "recursive wrapper failure was ambiguous: {recursive:?}"
  );
  Ok(())
}

#[test]
fn removal_refuses_a_changed_wrapper_field_and_preserves_unowned_configuration() -> Result<()> {
  let workspace = TestWorkspace::new_single_crate("transparent-remove-drift", "0.1.0")?;
  let cargo_home = tempfile::tempdir()?;
  let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
  assert!(setup.status.success(), "setup failed: {setup:?}");
  let config = cargo_home.path().join("config.toml");
  fs::write(
    &config,
    "[build]\nrustc-wrapper = 'replacement-wrapper'\n[net]\noffline = true\n",
  )?;
  let remove = rail(
    &workspace.path,
    cargo_home.path(),
    &["rail", "cache", "remove", "--check"],
  )?;
  assert_eq!(remove.status.code(), Some(2));
  assert!(String::from_utf8_lossy(&remove.stderr).contains("removal refused"));
  assert!(fs::read_to_string(&config)?.contains("replacement-wrapper"));
  assert!(
    cargo_home
      .path()
      .join("cargo-rail/compiler-cache-v1/setup.json")
      .is_file()
  );
  Ok(())
}

#[test]
fn local_cleanup_uses_the_receipt_selected_custom_cache_and_is_repairable() -> Result<()> {
  let workspace = TestWorkspace::new_single_crate("transparent-custom-clean", "0.1.0")?;
  let cargo_home = tempfile::tempdir()?;
  let cache_base = tempfile::tempdir()?;
  let cache_base_arg = cache_base.path().to_str().context("cache base path")?;
  let setup = rail(
    &workspace.path,
    cargo_home.path(),
    &["rail", "cache", "setup", "--local-dir", cache_base_arg],
  )?;
  assert!(setup.status.success(), "custom setup failed: {setup:?}");
  let cold = cargo_check(&workspace.path, cargo_home.path(), None, None)?;
  assert!(cold.status.success(), "custom cache seed failed: {cold:?}");
  let custom_root = cache_base.path().join("cargo-rail/local-cas-v2");
  assert!(custom_root.is_dir());
  assert!(!cargo_home.path().join("cargo-rail/local-cas-v2").exists());

  let check = rail(
    &workspace.path,
    cargo_home.path(),
    &["rail", "cache", "clean", "--scope", "local", "--check"],
  )?;
  assert_eq!(check.status.code(), Some(1));
  assert!(custom_root.is_dir(), "cleanup preview mutated the selected cache");
  let clean = rail(
    &workspace.path,
    cargo_home.path(),
    &["rail", "cache", "clean", "--scope", "local"],
  )?;
  assert!(clean.status.success(), "custom cache cleanup failed: {clean:?}");
  assert!(!custom_root.exists());
  assert!(!cargo_home.path().join("cargo-rail/local-cas-v2").exists());

  let repair = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
  assert!(repair.status.success(), "custom cache repair failed: {repair:?}");
  assert!(
    custom_root.is_dir(),
    "repair changed or ignored the receipt-selected cache"
  );
  Ok(())
}

#[cfg(unix)]
#[test]
fn ordinary_cargo_and_nextest_commands_receive_eligible_library_reuse() -> Result<()> {
  use std::os::unix::fs::PermissionsExt as _;

  let workspace = TestWorkspace::new_single_crate("transparent_shapes", "0.1.0")?;
  fs::write(
    workspace.path.join("src/main.rs"),
    "fn main() { println!(\"{}\", transparent_shapes::hello()); }\n",
  )?;
  let cargo_home = tempfile::tempdir()?;
  let setup = rail(&workspace.path, cargo_home.path(), &["rail", "cache", "setup"])?;
  assert!(setup.status.success(), "setup failed: {setup:?}");

  let shim = workspace.path.join("rustc-command-shape-proof");
  fs::write(
    &shim,
    r#"#!/bin/sh
crate_name=
crate_type=
previous=
for argument in "$@"; do
  if [ "$previous" = "--crate-name" ]; then crate_name="$argument"; fi
  if [ "$previous" = "--crate-type" ]; then crate_type="$argument"; fi
  case "$argument" in
    --crate-name=*) crate_name="${argument#--crate-name=}" ;;
    --crate-type=*) crate_type="${argument#--crate-type=}" ;;
  esac
  previous="$argument"
done
if [ "$crate_name" = "transparent_shapes" ] && [ "$crate_type" = "lib" ]; then exit 91; fi
exec "$REAL_RUSTC" "$@"
"#,
  )?;
  fs::set_permissions(&shim, fs::Permissions::from_mode(0o700))?;

  let lanes: &[(&str, &[&str])] = &[
    ("check", &["check", "--quiet"]),
    ("build", &["build", "--quiet"]),
    ("test", &["test", "--quiet"]),
    ("run", &["run", "--quiet"]),
    ("bench", &["bench", "--quiet", "--no-run"]),
    ("nextest", &["nextest", "run"]),
  ];
  for (name, arguments) in lanes {
    let seed = Command::new("cargo")
      .current_dir(&workspace.path)
      .args(*arguments)
      .env("CARGO_HOME", cargo_home.path())
      .env("CARGO_INCREMENTAL", "0")
      .env_remove("RUSTC_WRAPPER")
      .env_remove("RUSTC_WORKSPACE_WRAPPER")
      .output()?;
    assert!(seed.status.success(), "{name} seed failed: {seed:?}");
    fs::remove_dir_all(workspace.path.join("target"))?;
    let reused = Command::new("cargo")
      .current_dir(&workspace.path)
      .args(*arguments)
      .env("CARGO_HOME", cargo_home.path())
      .env("CARGO_INCREMENTAL", "0")
      .env("RUSTC", &shim)
      .env("REAL_RUSTC", "rustc")
      .env_remove("RUSTC_WRAPPER")
      .env_remove("RUSTC_WORKSPACE_WRAPPER")
      .output()?;
    assert!(
      reused.status.success(),
      "{name} executed an eligible library compiler instead of restoring it: {reused:?}"
    );
    fs::remove_dir_all(workspace.path.join("target"))?;
  }
  Ok(())
}

fn directory_snapshot(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
  fn visit(root: &Path, current: &Path, snapshot: &mut BTreeMap<PathBuf, Vec<u8>>) -> Result<()> {
    let mut entries = fs::read_dir(current)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_unstable_by_key(fs::DirEntry::file_name);
    for entry in entries {
      let path = entry.path();
      let metadata = fs::symlink_metadata(&path)?;
      if metadata.is_dir() {
        visit(root, &path, snapshot)?;
      } else if metadata.is_file() {
        snapshot.insert(path.strip_prefix(root)?.to_path_buf(), fs::read(path)?);
      }
    }
    Ok(())
  }

  let mut snapshot = BTreeMap::new();
  visit(root, root, &mut snapshot)?;
  Ok(snapshot)
}
