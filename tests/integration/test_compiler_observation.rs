use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use sha2::{Digest as _, Sha256};

use crate::helpers::TestWorkspace;

const CACHE_WRAPPER_MARKER: &str = "CARGO_RAIL_COMPILER_CACHE_WRAPPER";
const CACHE_CONTROL_ENV: &str = "CARGO_RAIL_CACHE";
const BENCH_COVERAGE_CONTROL: &str = "__cargo_rail_benchmark_coverage_v1";
const BENCH_COVERAGE_DIRECTORY_ENV: &str = "CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY";

fn compiler_fact_path_identity(path: &Path) -> Result<String> {
  let path = fs::canonicalize(path)?;
  let bytes = path.as_os_str().as_encoded_bytes();
  let mut hasher = Sha256::new();
  hasher.update(b"cargo-rail-compiler-fact-path-v1\0");
  hasher.update((bytes.len() as u64).to_le_bytes());
  hasher.update(bytes);
  let digest = hasher
    .finalize()
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect::<String>();
  Ok(format!("sha256:{digest}"))
}

fn write_compiler_fact_capability(observation_directory: &Path, source_root: &Path) -> Result<PathBuf> {
  fs::create_dir_all(observation_directory)?;
  let capability = observation_directory.join("test-fact-session.cap");
  let encoded = serde_json::to_vec(&serde_json::json!({
    "version": 1,
    "observation_directory_identity": compiler_fact_path_identity(observation_directory)?,
    "source_root_identity": compiler_fact_path_identity(source_root)?,
  }))?;
  let mut options = OpenOptions::new();
  options.write(true).create_new(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt as _;

    options.mode(0o600);
  }
  use std::io::Write as _;
  options.open(&capability)?.write_all(&encoded)?;
  Ok(capability)
}
#[cfg(unix)]
fn write_executable(path: &Path, contents: &str) -> Result<()> {
  use std::os::unix::fs::PermissionsExt as _;

  fs::write(path, contents)?;
  let mut permissions = fs::metadata(path)?.permissions();
  permissions.set_mode(0o755);
  fs::set_permissions(path, permissions)?;
  Ok(())
}

#[test]
fn cache_off_bypasses_direct_wrapper_context_and_cas_acquisition() -> Result<()> {
  let state = tempfile::tempdir()?;
  let absent_cache = state.path().join("cache-must-not-exist");
  let absent_session = state.path().join("session-must-not-be-read.json");
  let absent_coverage = state.path().join("coverage-must-not-exist");

  let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail-native-rustc-wrapper"))
    .args(["rustc", "--version"])
    .env(CACHE_CONTROL_ENV, "off")
    .env("CARGO_RAIL_CACHE_DIR", &absent_cache)
    .env("CARGO_RAIL_NATIVE_COMPILER_CACHE_SESSION", &absent_session)
    .env("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY", &absent_coverage)
    .env_remove(CACHE_WRAPPER_MARKER)
    .env_remove("CARGO_RAIL_RUSTC_WRAPPER")
    .env_remove("CARGO_RAIL_RUSTDOC_WRAPPER")
    .output()?;

  assert!(output.status.success(), "cache-off compiler bypass failed: {output:?}");
  assert!(
    String::from_utf8_lossy(&output.stdout).starts_with("rustc "),
    "selected compiler output was not preserved: {output:?}"
  );
  assert!(!absent_cache.exists(), "cache-off bypass acquired the CAS");
  assert!(
    !absent_session.exists(),
    "cache-off bypass created or changed session state"
  );
  assert!(
    !absent_coverage.exists(),
    "cache-off bypass acquired benchmark coverage state"
  );
  Ok(())
}

#[test]
fn unsupported_incremental_invocation_bypasses_before_direct_context_load() -> Result<()> {
  let state = tempfile::tempdir()?;
  fs::create_dir_all(state.path().join("src"))?;
  fs::create_dir_all(state.path().join("out"))?;
  fs::write(state.path().join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n")?;
  let absent_cache = state.path().join("cache-must-not-exist");

  let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail-native-rustc-wrapper"))
    .current_dir(state.path())
    .args([
      "rustc",
      "--crate-name",
      "fixture",
      "--crate-type=lib",
      "--emit=dep-info,metadata",
      "--error-format=json",
      "--out-dir",
      "out",
      "-Cincremental=incremental",
      "src/lib.rs",
    ])
    .env("CARGO_RAIL_CACHE_DIR", &absent_cache)
    .env_remove(CACHE_CONTROL_ENV)
    .env_remove(CACHE_WRAPPER_MARKER)
    .env_remove("CARGO_RAIL_RUSTC_WRAPPER")
    .env_remove("CARGO_RAIL_RUSTDOC_WRAPPER")
    .output()?;

  assert!(
    output.status.success(),
    "incremental compiler bypass failed: {output:?}"
  );
  assert!(!absent_cache.exists(), "unsupported invocation acquired the CAS");
  Ok(())
}

#[test]
fn benchmark_coverage_records_fast_bypass_without_cache_context() -> Result<()> {
  let state = tempfile::tempdir()?;
  let state_root = fs::canonicalize(state.path())?;
  let coverage = state_root.join("coverage");
  fs::create_dir(&coverage)?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(&coverage, fs::Permissions::from_mode(0o700))?;
  }
  let absent_cache = state_root.join("cache-must-not-exist");

  let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail-native-rustc-wrapper"))
    .args(["rustc", "--version"])
    .env(CACHE_CONTROL_ENV, BENCH_COVERAGE_CONTROL)
    .env(BENCH_COVERAGE_DIRECTORY_ENV, &coverage)
    .env("CARGO_RAIL_CACHE_DIR", &absent_cache)
    .env_remove(CACHE_WRAPPER_MARKER)
    .env_remove("CARGO_RAIL_RUSTC_WRAPPER")
    .env_remove("CARGO_RAIL_RUSTDOC_WRAPPER")
    .output()?;

  assert!(output.status.success(), "benchmark compiler bypass failed: {output:?}");
  assert!(!absent_cache.exists(), "benchmark fast bypass acquired the CAS");
  let events = fs::read_dir(&coverage)?.collect::<Result<Vec<_>, _>>()?;
  assert_eq!(events.len(), 1, "benchmark bypass did not retain one event");
  let event: serde_json::Value = serde_json::from_slice(&fs::read(events[0].path())?)?;
  assert_eq!(event["status"], "bypassed");
  assert_eq!(event["reason"], "compiler_information_request");
  assert_eq!(event["compiler"], "rustc");
  assert_eq!(event["arguments"], serde_json::json!(["--version"]));
  Ok(())
}

#[cfg(unix)]
#[test]
fn benchmark_coverage_rejects_a_symlink_without_changing_compiler_behavior() -> Result<()> {
  use std::os::unix::fs::symlink;

  let state = tempfile::tempdir()?;
  let state_root = fs::canonicalize(state.path())?;
  let real = state_root.join("real-coverage");
  let selected = state_root.join("selected-coverage");
  fs::create_dir(&real)?;
  {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(&real, fs::Permissions::from_mode(0o700))?;
  }
  symlink(&real, &selected)?;

  let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail-native-rustc-wrapper"))
    .args(["rustc", "--version"])
    .env(CACHE_CONTROL_ENV, BENCH_COVERAGE_CONTROL)
    .env(BENCH_COVERAGE_DIRECTORY_ENV, &selected)
    .env_remove(CACHE_WRAPPER_MARKER)
    .env_remove("CARGO_RAIL_RUSTC_WRAPPER")
    .env_remove("CARGO_RAIL_RUSTDOC_WRAPPER")
    .output()?;

  assert!(
    output.status.success(),
    "hostile benchmark path changed compiler behavior"
  );
  assert!(
    fs::read_dir(real)?.next().is_none(),
    "hostile benchmark path received evidence"
  );
  Ok(())
}

#[cfg(unix)]
#[test]
fn cache_off_bypass_preserves_compiler_signal_status() -> Result<()> {
  use std::os::unix::process::ExitStatusExt as _;

  let state = tempfile::tempdir()?;
  let compiler = state.path().join("signal-compiler");
  write_executable(&compiler, "#!/bin/sh\nkill -TERM $$\n")?;

  let status = Command::new(env!("CARGO_BIN_EXE_cargo-rail-native-rustc-wrapper"))
    .arg(&compiler)
    .env(CACHE_CONTROL_ENV, "off")
    .env_remove(CACHE_WRAPPER_MARKER)
    .env_remove("CARGO_RAIL_RUSTC_WRAPPER")
    .env_remove("CARGO_RAIL_RUSTDOC_WRAPPER")
    .status()?;

  assert_eq!(status.signal(), Some(15));
  Ok(())
}

#[cfg(unix)]
#[test]
fn cache_off_bypass_preserves_non_utf8_argument_bytes() -> Result<()> {
  use std::ffi::OsString;
  use std::os::unix::ffi::OsStringExt as _;

  let state = tempfile::tempdir()?;
  let compiler = state.path().join("argument-compiler");
  let captured = state.path().join("captured-argument");
  write_executable(&compiler, "#!/bin/sh\nprintf '%s' \"$1\" > \"$CAPTURE_PATH\"\n")?;
  let argument = vec![b'a', b'r', b'g', b'-', 0x80, 0xff];

  let status = Command::new(env!("CARGO_BIN_EXE_cargo-rail-native-rustc-wrapper"))
    .arg(&compiler)
    .arg(OsString::from_vec(argument.clone()))
    .env(CACHE_CONTROL_ENV, "off")
    .env("CAPTURE_PATH", &captured)
    .env_remove(CACHE_WRAPPER_MARKER)
    .env_remove("CARGO_RAIL_RUSTC_WRAPPER")
    .env_remove("CARGO_RAIL_RUSTDOC_WRAPPER")
    .status()?;

  assert!(status.success());
  assert_eq!(fs::read(captured)?, argument);
  Ok(())
}

#[cfg(unix)]
#[test]
fn fact_driver_preserves_compiler_signal_status_after_publication() -> Result<()> {
  use std::os::unix::process::ExitStatusExt as _;

  let state = tempfile::tempdir()?;
  fs::create_dir_all(state.path().join("src"))?;
  fs::create_dir_all(state.path().join("out"))?;
  fs::write(state.path().join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n")?;
  let compiler = state.path().join("signal-compiler");
  write_executable(&compiler, "#!/bin/sh\nkill -TERM $$\n")?;
  let observations = state.path().join("observations");
  let fact_capability = write_compiler_fact_capability(&observations, state.path())?;

  let status = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(state.path())
    .arg(&compiler)
    .args([
      "--crate-name",
      "fixture",
      "--crate-type=lib",
      "--emit=dep-info,metadata",
      "--error-format=json",
      "--out-dir",
      "out",
      "src/lib.rs",
    ])
    .env("CARGO_RAIL_RUSTC_WRAPPER", "1")
    .env("CARGO_RAIL_COMPILER_FACT_SESSION", fact_capability)
    .env("CARGO_RAIL_COMPILER_OBSERVATION_DIRECTORY", &observations)
    .env("CARGO_RAIL_COMPILER_OBSERVATION_SOURCE_ROOT", state.path())
    .env_remove(CACHE_WRAPPER_MARKER)
    .env_remove("CARGO_RAIL_RUSTDOC_WRAPPER")
    .status()?;

  assert_eq!(status.signal(), Some(15));
  assert!(fs::read_dir(observations)?.any(|entry| {
    entry.ok().is_some_and(|entry| {
      entry
        .path()
        .file_name()
        .is_some_and(|name| name.as_encoded_bytes().starts_with(b"rustc-"))
    })
  }));
  Ok(())
}

#[test]
fn conflicting_compiler_roles_fail_before_clap_or_compiler_execution() -> Result<()> {
  let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .arg("--version")
    .env("CARGO_RAIL_RUSTC_WRAPPER", "1")
    .env("CARGO_RAIL_RUSTDOC_WRAPPER", "1")
    .env_remove(CACHE_WRAPPER_MARKER)
    .env_remove(CACHE_CONTROL_ENV)
    .output()?;

  assert_eq!(
    output.status.code(),
    Some(2),
    "ambiguous compiler role did not fail: {output:?}"
  );
  assert!(
    String::from_utf8_lossy(&output.stderr).contains("conflicting compiler role markers"),
    "role conflict was not diagnosed: {output:?}"
  );
  assert!(
    output.stdout.is_empty(),
    "Clap or the compiler ran after role rejection"
  );
  Ok(())
}

#[test]
fn absent_fact_capability_executes_the_original_compiler_without_collection() -> Result<()> {
  let state = tempfile::tempdir()?;
  fs::create_dir_all(state.path().join("src"))?;
  fs::create_dir_all(state.path().join("out"))?;
  fs::write(state.path().join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n")?;

  let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(state.path())
    .args([
      "rustc",
      "--crate-name",
      "fixture",
      "--crate-type=lib",
      "--emit=metadata",
      "--out-dir",
      "out",
      "src/lib.rs",
    ])
    .env("CARGO_RAIL_RUSTC_WRAPPER", "1")
    .env_remove("CARGO_RAIL_COMPILER_FACT_SESSION")
    .env_remove("CARGO_RAIL_COMPILER_OBSERVATION_DIRECTORY")
    .env_remove("CARGO_RAIL_COMPILER_OBSERVATION_SOURCE_ROOT")
    .env_remove(CACHE_WRAPPER_MARKER)
    .env_remove("CARGO_RAIL_RUSTDOC_WRAPPER")
    .output()?;

  assert!(output.status.success(), "fact-free compiler bypass failed: {output:?}");
  assert!(fs::read_dir(state.path().join("out"))?.next().is_some());
  Ok(())
}

#[test]
fn incomplete_fact_capability_fails_before_compiler_execution() -> Result<()> {
  let state = tempfile::tempdir()?;
  fs::create_dir_all(state.path().join("src"))?;
  fs::create_dir_all(state.path().join("out"))?;
  fs::write(state.path().join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n")?;

  let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(state.path())
    .args([
      "rustc",
      "--crate-name",
      "fixture",
      "--crate-type=lib",
      "--emit=metadata",
      "--out-dir",
      "out",
      "src/lib.rs",
    ])
    .env("CARGO_RAIL_RUSTC_WRAPPER", "1")
    .env(
      "CARGO_RAIL_COMPILER_OBSERVATION_DIRECTORY",
      state.path().join("observations"),
    )
    .env("CARGO_RAIL_COMPILER_OBSERVATION_SOURCE_ROOT", state.path())
    .env_remove("CARGO_RAIL_COMPILER_FACT_SESSION")
    .env_remove(CACHE_WRAPPER_MARKER)
    .env_remove("CARGO_RAIL_RUSTDOC_WRAPPER")
    .output()?;

  assert_eq!(output.status.code(), Some(2));
  assert!(String::from_utf8_lossy(&output.stderr).contains("compiler fact capability is incomplete"));
  assert!(fs::read_dir(state.path().join("out"))?.next().is_none());
  Ok(())
}

#[test]
fn rustdoc_proxy_preserves_cargo_docs_and_records_dep_info() -> Result<()> {
  let workspace = TestWorkspace::new_single_crate("rustdoc-observation", "0.1.0")?;
  fs::write(
    workspace.path.join("src/lib.rs"),
    "mod nested;\npub use nested::value;\n",
  )?;
  fs::write(workspace.path.join("src/nested.rs"), "pub fn value() -> u8 { 1 }\n")?;
  let observation_directory = workspace.path.join("observations");
  let target_directory = workspace.path.join("target-observation");
  let fact_capability = write_compiler_fact_capability(&observation_directory, &workspace.path)?;

  let output = Command::new("cargo")
    .current_dir(&workspace.path)
    .args(["doc", "--no-deps", "--message-format=json", "--target-dir"])
    .arg(&target_directory)
    .env("RUSTDOC", env!("CARGO_BIN_EXE_cargo-rail"))
    .env("CARGO_RAIL_RUSTDOC_WRAPPER", "1")
    .env("CARGO_RAIL_INNER_RUSTDOC", "rustdoc")
    .env("CARGO_RAIL_COMPILER_OBSERVATION_DIRECTORY", &observation_directory)
    .env("CARGO_RAIL_COMPILER_OBSERVATION_SOURCE_ROOT", &workspace.path)
    .env("CARGO_RAIL_COMPILER_FACT_SESSION", fact_capability)
    .output()
    .context("run cargo doc through the rustdoc observation proxy")?;
  assert!(
    output.status.success(),
    "cargo doc failed\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  let index = target_directory.join("doc/rustdoc_observation/index.html");
  assert!(
    index.is_file(),
    "rustdoc proxy must preserve HTML output at {}",
    index.display()
  );
  let canonical_index = fs::canonicalize(&index)?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let cargo_retained_index = stdout.lines().any(|line| {
    serde_json::from_str::<serde_json::Value>(line)
      .ok()
      .and_then(|message| message["filenames"].as_array().cloned())
      .is_some_and(|filenames| {
        filenames.iter().any(|filename| {
          filename
            .as_str()
            .and_then(|filename| fs::canonicalize(filename).ok())
            .is_some_and(|filename| filename == canonical_index)
        })
      })
  });
  assert!(
    cargo_retained_index,
    "Cargo's stable artifact message must retain the documentation index\n{}",
    stdout
  );

  let records = fs::read_dir(&observation_directory)?
    .map(|entry| -> Result<serde_json::Value> {
      let path = entry?.path();
      Ok(serde_json::from_slice(&fs::read(path)?)?)
    })
    .collect::<Result<Vec<_>>>()?;
  let record = records
    .iter()
    .find(|record| record["crate_name"] == "rustdoc_observation")
    .context("rustdoc crate invocation observation")?;
  assert_eq!(record["mode"], "rustdoc");
  assert_eq!(record["success"], true);
  let records_dep_info = record["compiler_arguments"].as_array().is_some_and(|arguments| {
    arguments.iter().any(|argument| {
      argument
        .as_str()
        .is_some_and(|argument| argument.starts_with("--emit=") && argument.contains("dep-info"))
    })
  });
  let observed_paths = record["observed_reads"]
    .as_array()
    .context("observed rustdoc reads")?
    .iter()
    .filter_map(|read| read["path"]["path"].as_str())
    .collect::<Vec<_>>();
  let declared_paths = record["declared_inputs"]
    .as_array()
    .context("declared rustdoc inputs")?
    .iter()
    .filter_map(|input| input["path"]["path"].as_str())
    .collect::<Vec<_>>();
  assert!(
    declared_paths.contains(&"src/lib.rs") || observed_paths.contains(&"src/lib.rs"),
    "crate root missing from {record}"
  );
  if records_dep_info {
    assert!(
      observed_paths.contains(&"src/nested.rs"),
      "module source missing from {record}"
    );
    assert!(
      record["emitted_outputs"].as_array().is_some_and(|outputs| outputs
        .iter()
        .any(|output| { output["path"]["path"] == "target-observation/doc/rustdoc_observation.d" })),
      "rustdoc dep-info output missing from {record}"
    );
  } else {
    assert!(
      record["bypasses"]
        .as_array()
        .is_some_and(|bypasses| bypasses.iter().any(|reason| reason == "rustdoc_dep_info_unavailable")),
      "rustdoc without stable dep-info must remain an explicit bypass: {record}"
    );
  }
  assert!(
    record["bypasses"].as_array().is_some_and(|bypasses| bypasses
      .iter()
      .any(|reason| reason == "rustdoc_output_tree_unavailable")),
    "Cargo does not enumerate the complete HTML tree, so reuse must remain disabled: {record}"
  );
  let encoded = serde_json::to_string(record)?;
  let canonical_workspace = fs::canonicalize(&workspace.path)?;
  for root in [&workspace.path, &canonical_workspace] {
    assert!(
      !encoded.contains(root.to_string_lossy().as_ref()),
      "captured compiler observation must not retain checkout root '{}': {record}",
      root.display()
    );
  }

  Ok(())
}
