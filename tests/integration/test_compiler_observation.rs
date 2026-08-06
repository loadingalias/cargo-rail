use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

#[cfg(unix)]
use crate::helpers::compiler_evidence_cache;
use crate::helpers::{TestWorkspace, compiler_evidence_cache_at};

const CACHE_WRAPPER_MARKER: &str = "CARGO_RAIL_COMPILER_CACHE_WRAPPER";
const CAPTURE_PAUSE_PHASE_ENV: &str = "CARGO_RAIL_TEST_NATIVE_CAPTURE_PAUSE_PHASE";
const CAPTURE_PAUSE_CRATE_ENV: &str = "CARGO_RAIL_TEST_NATIVE_CAPTURE_PAUSE_CRATE";
const CAPTURE_PAUSE_DIRECTORY_ENV: &str = "CARGO_RAIL_TEST_NATIVE_CAPTURE_PAUSE_DIRECTORY";
const RESTORE_CANCEL_ENV: &str = "CARGO_RAIL_TEST_NATIVE_RESTORE_CANCEL";
const RESTORE_CRATE_ENV: &str = "CARGO_RAIL_TEST_NATIVE_RESTORE_CRATE";

#[cfg(unix)]
fn write_executable(path: &Path, contents: &str) -> Result<()> {
  use std::os::unix::fs::PermissionsExt as _;

  fs::write(path, contents)?;
  let mut permissions = fs::metadata(path)?.permissions();
  permissions.set_mode(0o755);
  fs::set_permissions(path, permissions)?;
  Ok(())
}

fn run_clean_check(workspace: &Path, target: &Path, wrapper: bool) -> Result<std::process::Output> {
  let mut command = Command::new("cargo");
  command
    .current_dir(workspace)
    .args(["check", "--locked", "--quiet", "--target-dir"])
    .arg(target)
    .env("CARGO_INCREMENTAL", "0")
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .env_remove(CACHE_WRAPPER_MARKER);
  if wrapper {
    command
      .env("RUSTC_WRAPPER", env!("CARGO_BIN_EXE_cargo-rail"))
      .env(CACHE_WRAPPER_MARKER, "1");
  }
  command.output().context("run clean Cargo check")
}

fn scrub_front_door_compiler_environment(command: &mut Command) {
  for name in [
    "CARGO_BUILD_RUSTFLAGS",
    "CARGO_BUILD_TARGET",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_TARGET_DIR",
    "RUSTC",
    "RUSTC_BOOTSTRAP",
    "RUSTC_FORCE_INCREMENTAL",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "RUSTFLAGS",
    CACHE_WRAPPER_MARKER,
  ] {
    command.env_remove(name);
  }
}

fn generate_front_door_lockfile(workspace: &Path, cargo_home: &Path) -> Result<()> {
  let mut command = Command::new("cargo");
  scrub_front_door_compiler_environment(&mut command);
  let output = command
    .current_dir(workspace)
    .arg("generate-lockfile")
    .env("CARGO_HOME", cargo_home)
    .env("CARGO_NET_OFFLINE", "true")
    .env("CARGO_TERM_COLOR", "never")
    .output()?;
  assert!(output.status.success(), "fixture lockfile failed: {output:?}");
  Ok(())
}

#[cfg(unix)]
fn rustc_host_target() -> Result<String> {
  let output = Command::new("rustc").arg("-vV").output()?;
  assert!(output.status.success(), "rustc -vV failed: {output:?}");
  String::from_utf8(output.stdout)?
    .lines()
    .find_map(|line| line.strip_prefix("host: ").map(str::to_string))
    .context("rustc host target")
}

fn compiler_output_files(root: &Path) -> Result<Vec<(PathBuf, Vec<u8>)>> {
  fn visit(root: &Path, directory: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) -> Result<()> {
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_unstable_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
      let path = entry.path();
      let metadata = fs::symlink_metadata(&path)?;
      if metadata.is_dir() {
        visit(root, &path, files)?;
      } else if metadata.is_file()
        && path
          .extension()
          .and_then(std::ffi::OsStr::to_str)
          .is_some_and(|extension| matches!(extension, "d" | "rmeta"))
      {
        files.push((path.strip_prefix(root)?.to_path_buf(), fs::read(path)?));
      }
    }
    Ok(())
  }

  let mut files = Vec::new();
  visit(root, root, &mut files)?;
  Ok(files)
}

fn local_observation(cache: &serde_json::Value, target_name: &str) -> Result<serde_json::Value> {
  cache["entries"]
    .as_object()
    .context("compiler cache entries")?
    .values()
    .filter_map(|entry| {
      let observations = entry["observations"].as_array()?;
      observations
        .iter()
        .any(|observation| {
          observation["unit"]["target_name"] == target_name && observation["unit"]["profile"]["test"] == false
        })
        .then_some(entry)
    })
    .max_by_key(|entry| entry["created_unix_nanos"].as_u64())
    .and_then(|entry| entry["observations"].as_array())
    .and_then(|observations| {
      observations
        .iter()
        .filter(|observation| {
          observation["unit"]["target_name"] == target_name && observation["unit"]["profile"]["test"] == false
        })
        .min_by_key(|observation| observation["execution"]["cache_wrapper"]["status"] == "hit")
    })
    .cloned()
    .with_context(|| format!("compiler observation for {target_name}"))
}

fn wrapper_workspace(name: &str) -> Result<TestWorkspace> {
  let workspace = TestWorkspace::new_named(name)?;
  let root_manifest = workspace.path.join("Cargo.toml");
  let root_contents = fs::read_to_string(&root_manifest)?.replace(
    "resolver = \"2\"",
    "exclude = [\"vendor/wrapper-dep\"]\nresolver = \"2\"",
  );
  fs::write(&root_manifest, root_contents)?;
  let dependency = workspace.path.join("vendor/wrapper-dep");
  fs::create_dir_all(dependency.join("src"))?;
  fs::write(
    dependency.join("Cargo.toml"),
    "[package]\nname = \"wrapper-dep\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
  )?;
  fs::write(dependency.join("src/lib.rs"), "pub fn dependency() {}\n")?;
  workspace.add_crate(
    "wrapper-app",
    "0.1.0",
    &[("wrapper-dep", "{ path = \"../../vendor/wrapper-dep\" }")],
  )?;
  workspace.commit("Add wrapper fixture")?;
  Ok(workspace)
}

fn run_direct_native_build(workspace: &Path, cache: &Path) -> Result<std::process::Output> {
  let output = run_direct_native_build_unchecked(workspace, cache)?;
  assert!(output.status.success(), "direct native-cache run failed: {output:?}");
  Ok(output)
}

fn run_direct_native_build_unchecked(workspace: &Path, cache: &Path) -> Result<std::process::Output> {
  run_direct_native_build_with_environment(workspace, cache, &[])
}

fn direct_native_build_command(workspace: &Path, cache: &Path) -> Command {
  let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-rail"));
  command
    .current_dir(workspace)
    .args([
      "rail",
      "run",
      "--all",
      "--action",
      "build",
      "--explain",
      "--",
      "--offline",
      "--quiet",
    ])
    .env("CARGO_INCREMENTAL", "0")
    .env("CARGO_RAIL_CACHE_DIR", cache)
    .env_remove("CARGO_BUILD_BUILD_DIR")
    .env_remove("CARGO_BUILD_JOBS")
    .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
    .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER")
    .env_remove("CARGO_BUILD_TARGET_DIR")
    .env_remove("CARGO_BUILD_WARNINGS")
    .env_remove("CARGO_TARGET_DIR")
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER");
  command
}

fn run_direct_native_build_with_environment(
  workspace: &Path,
  cache: &Path,
  environment: &[(&str, &str)],
) -> Result<std::process::Output> {
  let mut command = direct_native_build_command(workspace, cache);
  for (name, value) in environment {
    command.env(name, value);
  }
  command.output().map_err(Into::into)
}

fn wait_for_pause_ready(path: &Path) -> Result<()> {
  let started = Instant::now();
  loop {
    match fs::symlink_metadata(path) {
      Ok(metadata) if metadata.is_file() => return Ok(()),
      Ok(_) => anyhow::bail!("capture pause ready path is not a file: {}", path.display()),
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
      Err(error) => return Err(error.into()),
    }
    if started.elapsed() > Duration::from_secs(20) {
      anyhow::bail!("timed out waiting for capture pause at {}", path.display());
    }
    std::thread::sleep(Duration::from_millis(2));
  }
}

fn run_direct_native_build_paused(
  workspace: &Path,
  cache: &Path,
  phase: &str,
  mutate: impl FnOnce() -> Result<()>,
) -> Result<std::process::Output> {
  let control = tempfile::tempdir()?;
  let mut command = direct_native_build_command(workspace, cache);
  command
    .env(CAPTURE_PAUSE_PHASE_ENV, phase)
    .env(CAPTURE_PAUSE_CRATE_ENV, "wrapper_app")
    .env(CAPTURE_PAUSE_DIRECTORY_ENV, control.path())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
  let mut child = command.spawn().context("spawn paused native-cache build")?;
  let synchronized = wait_for_pause_ready(&control.path().join("ready"))
    .and_then(|()| mutate())
    .and_then(|()| {
      OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(control.path().join("continue"))?;
      Ok(())
    });
  if let Err(error) = synchronized {
    let _ = child.kill();
    let _ = child.wait();
    return Err(error);
  }
  child.wait_with_output().context("wait for paused native-cache build")
}

fn run_direct_native_build_two_barriers(
  workspace: &Path,
  cache: &Path,
  first_phase: &str,
  first_mutation: impl FnOnce() -> Result<()>,
  second_phase: &str,
  second_mutation: impl FnOnce() -> Result<()>,
) -> Result<std::process::Output> {
  let control = tempfile::tempdir()?;
  let mut command = direct_native_build_command(workspace, cache);
  command
    .env(CAPTURE_PAUSE_PHASE_ENV, format!("{first_phase},{second_phase}"))
    .env(CAPTURE_PAUSE_CRATE_ENV, "wrapper_app")
    .env(CAPTURE_PAUSE_DIRECTORY_ENV, control.path())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
  let mut child = command.spawn().context("spawn two-barrier native-cache build")?;
  let synchronized = (|| {
    wait_for_pause_ready(&control.path().join(format!("ready-{first_phase}")))?;
    first_mutation()?;
    OpenOptions::new()
      .write(true)
      .create_new(true)
      .open(control.path().join(format!("continue-{first_phase}")))?;
    wait_for_pause_ready(&control.path().join(format!("ready-{second_phase}")))?;
    second_mutation()?;
    OpenOptions::new()
      .write(true)
      .create_new(true)
      .open(control.path().join(format!("continue-{second_phase}")))?;
    Ok(())
  })();
  if let Err(error) = synchronized {
    let _ = child.kill();
    let _ = child.wait();
    return Err(error);
  }
  child
    .wait_with_output()
    .context("wait for two-barrier native-cache build")
}

fn native_unit_event(output: &std::process::Output, crate_name: &str) -> Result<serde_json::Value> {
  [&output.stdout, &output.stderr]
    .into_iter()
    .flat_map(|stream| {
      String::from_utf8_lossy(stream)
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>()
    })
    .filter_map(|line| {
      line
        .split_once(" native compiler cache event: ")
        .map(|(_, event)| event.to_string())
    })
    .map(|event| serde_json::from_str::<serde_json::Value>(&event).map_err(Into::into))
    .find_map(|event| match event {
      Ok(event) if event["unit"]["descriptor"]["crate_name"] == crate_name => Some(Ok(event)),
      Ok(_) => None,
      Err(error) => Some(Err(error)),
    })
    .with_context(|| format!("native-cache event for {crate_name}"))?
}

fn native_event_with_reason(output: &std::process::Output, reason: &str) -> Result<serde_json::Value> {
  [&output.stdout, &output.stderr]
    .into_iter()
    .flat_map(|stream| {
      String::from_utf8_lossy(stream)
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>()
    })
    .filter_map(|line| {
      line
        .split_once(" native compiler cache event: ")
        .map(|(_, event)| event.to_string())
    })
    .map(|event| serde_json::from_str::<serde_json::Value>(&event).map_err(Into::into))
    .find_map(|event| match event {
      Ok(event) if event["reason"] == reason => Some(Ok(event)),
      Ok(_) => None,
      Err(error) => Some(Err(error)),
    })
    .with_context(|| format!("native-cache event with reason {reason}"))?
}

fn run_unify_without_ambient_wrappers(workspace: &Path, cache: &Path) -> Result<std::process::Output> {
  run_unify_with_environment(workspace, cache, &[])
}

#[cfg(unix)]
fn run_unify_with_umask(workspace: &Path, cache: &Path, umask: &str) -> Result<std::process::Output> {
  let mut command = Command::new("sh");
  command
    .current_dir(workspace)
    .args(["-c", "umask \"$1\"; shift; exec \"$@\"", "cargo-rail-umask", umask])
    .arg(env!("CARGO_BIN_EXE_cargo-rail"))
    .args(["rail", "unify", "--check"])
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .env_remove("RUSTFLAGS")
    .env_remove("CARGO_ENCODED_RUSTFLAGS")
    .env_remove("RUSTC_BOOTSTRAP")
    .env_remove("RUSTC_FORCE_INCREMENTAL")
    .env_remove("P73_OPTIONAL")
    .env_remove("P73_UNRELATED")
    .env_remove("P73_VALUE")
    .env_remove(CACHE_WRAPPER_MARKER)
    .env("CARGO_INCREMENTAL", "0")
    .env("CARGO_RAIL_CACHE_DIR", cache)
    .output()
    .context("run cargo-rail unify with a fixed umask")
}

fn run_unify_with_environment(
  workspace: &Path,
  cache: &Path,
  environment: &[(&str, &str)],
) -> Result<std::process::Output> {
  let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-rail"));
  command
    .current_dir(workspace)
    .args(["rail", "unify", "--check"])
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .env_remove("RUSTFLAGS")
    .env_remove("CARGO_ENCODED_RUSTFLAGS")
    .env_remove("RUSTC_BOOTSTRAP")
    .env_remove("RUSTC_FORCE_INCREMENTAL")
    .env_remove(CACHE_WRAPPER_MARKER)
    .env("CARGO_INCREMENTAL", "0")
    .env("CARGO_RAIL_CACHE_DIR", cache);
  for (name, value) in environment {
    command.env(name, value);
  }
  command
    .output()
    .context("run cargo-rail unify without ambient wrappers")
}

fn native_cache_observation(cache_root: &Path) -> Result<serde_json::Value> {
  let cache = compiler_evidence_cache_at(cache_root)?;
  local_observation(&cache, "wrapper_app")
}

fn assert_native_miss(cache_root: &Path, output: &std::process::Output) -> Result<serde_json::Value> {
  assert_eq!(output.status.code(), Some(1), "unexpected unify result: {output:?}");
  let observation = native_cache_observation(cache_root)?;
  assert_ne!(
    observation["execution"]["cache_wrapper"]["status"], "hit",
    "mutated input must not authorize native reuse: {observation}"
  );
  Ok(observation)
}

fn assert_native_invalidation(
  cache_root: &Path,
  output: &std::process::Output,
  prior_action_keys: &[String],
) -> Result<(serde_json::Value, String)> {
  assert_eq!(output.status.code(), Some(1), "unexpected unify result: {output:?}");
  let observation = native_cache_observation(cache_root)?;
  let action_key = observation["execution"]["cache_wrapper"]["action_key"]
    .as_str()
    .context("native-cache action key after input mutation")?
    .to_string();
  assert!(
    !prior_action_keys.contains(&action_key),
    "mutated input reused a prior native-cache action key: {observation}"
  );
  Ok((observation, action_key))
}

fn replace_regular_file(path: &Path, bytes: &[u8], nonce: &str) -> Result<()> {
  let parent = path.parent().context("replacement file parent")?;
  let replacement = parent.join(format!(".cargo-rail-capture-race-{nonce}"));
  fs::write(&replacement, bytes)?;
  fs::remove_file(path)?;
  fs::rename(replacement, path)?;
  Ok(())
}

fn restore_markers(root: &Path) -> Result<Vec<PathBuf>> {
  fn visit(root: &Path, found: &mut Vec<PathBuf>) -> Result<()> {
    if !root.exists() {
      return Ok(());
    }
    for entry in fs::read_dir(root)? {
      let path = entry?.path();
      if path.is_dir() {
        visit(&path, found)?;
      } else if path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|name| name.starts_with(".cargo-rail-restore-") && name.ends_with(".json"))
      {
        found.push(path);
      }
    }
    Ok(())
  }

  let mut found = Vec::new();
  visit(root, &mut found)?;
  found.sort_unstable();
  Ok(found)
}

fn restore_transaction_name(name: &str) -> bool {
  name
    .strip_prefix(".cargo-rail-restore-")
    .is_some_and(|identity| identity.len() == 64 && identity.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn restore_transactions(root: &Path) -> Result<Vec<PathBuf>> {
  fn visit(root: &Path, found: &mut Vec<PathBuf>) -> Result<()> {
    if !root.exists() {
      return Ok(());
    }
    for entry in fs::read_dir(root)? {
      let path = entry?.path();
      let name = path.file_name().and_then(std::ffi::OsStr::to_str).unwrap_or_default();
      if path.is_dir() && restore_transaction_name(name) {
        found.push(path);
      } else if path.is_dir() {
        visit(&path, found)?;
      }
    }
    Ok(())
  }

  let mut found = Vec::new();
  visit(root, &mut found)?;
  found.sort_unstable();
  Ok(found)
}

fn native_restore_residue(root: &Path) -> Result<Vec<PathBuf>> {
  fn visit(root: &Path, found: &mut Vec<PathBuf>) -> Result<()> {
    if !root.exists() {
      return Ok(());
    }
    for entry in fs::read_dir(root)? {
      let path = entry?.path();
      let name = path.file_name().and_then(std::ffi::OsStr::to_str).unwrap_or_default();
      if name.starts_with(".cargo-rail-native-cache-")
        || name.starts_with(".cargo-rail-restore-output-")
        || name.starts_with(".cargo-rail-restore-") && name.ends_with(".json")
        || path.is_dir() && restore_transaction_name(name)
      {
        found.push(path);
      } else if path.is_dir() {
        visit(&path, found)?;
      }
    }
    Ok(())
  }

  let mut found = Vec::new();
  visit(root, &mut found)?;
  found.sort_unstable();
  Ok(found)
}

fn compiler_artifacts_for_crate(target: &Path, crate_name: &str) -> Result<Vec<(PathBuf, Vec<u8>)>> {
  fn visit(root: &Path, directory: &Path, crate_name: &str, files: &mut Vec<(PathBuf, Vec<u8>)>) -> Result<()> {
    if !directory.exists() {
      return Ok(());
    }
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_unstable_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
      let path = entry.path();
      let metadata = fs::symlink_metadata(&path)?;
      if metadata.is_dir() {
        visit(root, &path, crate_name, files)?;
      } else if metadata.is_file()
        && path
          .file_name()
          .and_then(std::ffi::OsStr::to_str)
          .is_some_and(|name| name.contains(crate_name))
        && path
          .extension()
          .and_then(std::ffi::OsStr::to_str)
          .is_some_and(|extension| matches!(extension, "d" | "rmeta" | "rlib"))
      {
        files.push((path.strip_prefix(root)?.to_path_buf(), fs::read(path)?));
      }
    }
    Ok(())
  }

  let mut files = Vec::new();
  visit(target, target, crate_name, &mut files)?;
  Ok(files)
}

fn install_restore_scan_tripwire(parent: &Path, actual_marker: Option<&Path>) -> Result<PathBuf> {
  let mut tripwire = parent.join(format!(".cargo-rail-restore-{}.json", "0".repeat(64)));
  if actual_marker == Some(tripwire.as_path()) {
    tripwire = parent.join(format!(".cargo-rail-restore-{}.json", "f".repeat(64)));
  }
  fs::write(&tripwire, b"malformed unrelated marker")?;
  Ok(tripwire)
}

#[cfg(unix)]
#[test]
fn cache_disabled_wrapper_preserves_the_process_contract() -> Result<()> {
  let root = tempfile::tempdir()?;
  let compiler = root.path().join("compiler.sh");
  let compiler_arguments = root.path().join("compiler-arguments");
  let environment = root.path().join("environment");
  let artifact = root.path().join("artifact");
  let injection_sentinel = root.path().join("must-not-exist");

  write_executable(
    &compiler,
    r#"#!/bin/sh
printf '%s\n' "$@" > "$COMPILER_ARGUMENTS"
printf '%s' "$PRESERVED_VALUE" > "$ENVIRONMENT_OUTPUT"
if [ "${CARGO_RAIL_COMPILER_CACHE_WRAPPER+x}" = x ]; then
  exit 98
fi
printf 'verified artifact\n' > "$ARTIFACT_OUTPUT"
printf 'compiler stdout\n'
printf 'compiler stderr\n' >&2
exit 37
"#,
  )?;

  let injected_argument = format!("$(touch {})", injection_sentinel.display());
  let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .arg(&compiler)
    .args(["--flag", "two words", &injected_argument])
    .env(CACHE_WRAPPER_MARKER, "1")
    .env("COMPILER_ARGUMENTS", &compiler_arguments)
    .env("ENVIRONMENT_OUTPUT", &environment)
    .env("ARTIFACT_OUTPUT", &artifact)
    .env("PRESERVED_VALUE", "preserved exactly")
    .output()
    .context("run cache-disabled compiler wrapper")?;

  assert_eq!(output.status.code(), Some(37));
  assert_eq!(output.stdout, b"compiler stdout\n");
  assert_eq!(output.stderr, b"compiler stderr\n");
  assert_eq!(fs::read(&artifact)?, b"verified artifact\n");
  assert_eq!(fs::read(&environment)?, b"preserved exactly");
  assert_eq!(
    fs::read_to_string(&compiler_arguments)?.lines().collect::<Vec<_>>(),
    ["--flag", "two words", injected_argument.as_str()]
  );
  assert!(
    !injection_sentinel.exists(),
    "wrapper argv must never be shell-evaluated"
  );
  Ok(())
}

#[test]
fn cache_disabled_wrapper_preserves_clean_cargo_outputs() -> Result<()> {
  let workspace = TestWorkspace::new_single_crate("cache-wrapper-cargo", "0.1.0")?;
  let lock = Command::new("cargo")
    .current_dir(&workspace.path)
    .arg("generate-lockfile")
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .output()?;
  assert!(lock.status.success(), "failed to generate fixture lockfile");
  let target = workspace.path.join("target-wrapper-comparison");

  let direct = run_clean_check(&workspace.path, &target, false)?;
  assert!(direct.status.success(), "direct Cargo check failed: {direct:?}");
  let direct_files = compiler_output_files(&target)?;
  assert!(
    !direct_files.is_empty(),
    "fixture must produce compiler output files to compare"
  );
  fs::remove_dir_all(&target)?;

  let wrapped = run_clean_check(&workspace.path, &target, true)?;
  assert!(wrapped.status.success(), "wrapped Cargo check failed: {wrapped:?}");
  assert_eq!(wrapped.stdout, direct.stdout);
  assert_eq!(wrapped.stderr, direct.stderr);
  assert_eq!(compiler_output_files(&target)?, direct_files);
  Ok(())
}

#[cfg(unix)]
#[test]
fn configured_linker_is_transparent_and_bypasses_before_wrapper_setup() -> Result<()> {
  use std::os::unix::fs::PermissionsExt as _;

  fn linker_arguments(log: &Path, target: &Path) -> Result<Vec<String>> {
    let dependency_directory = target.join("release/deps");
    fs::read_to_string(log)?
      .lines()
      .map(|argument| {
        let path = Path::new(argument);
        let compiler_scratch_object = path.file_name() == Some(std::ffi::OsStr::new("symbols.o"))
          && path
            .parent()
            .and_then(Path::file_name)
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|name| name.starts_with("rustc"))
          && path.starts_with(&dependency_directory);
        let compiler_raw_dylibs_directory = path.file_name() == Some(std::ffi::OsStr::new("raw-dylibs"))
          && path
            .parent()
            .and_then(Path::file_name)
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|name| name.starts_with("rustc"))
          && path.starts_with(&dependency_directory);
        if compiler_scratch_object {
          Ok("<rustc-temporary-symbols-object>".to_string())
        } else if compiler_raw_dylibs_directory {
          Ok("<rustc-temporary-raw-dylibs-directory>".to_string())
        } else {
          Ok(argument.to_string())
        }
      })
      .collect()
  }

  let workspace = TestWorkspace::new_single_crate("linker-parity", "0.1.0")?;
  let cargo_home = tempfile::tempdir()?;
  let cache = tempfile::tempdir()?;
  let tools = workspace.path.join("tools with spaces");
  let linker = tools.join("linker-proxy");
  let linker_log = workspace.path.join("linker-arguments.log");
  let target = workspace.path.join("target-compatibility");
  let binary = target.join("release/linker-parity");
  fs::create_dir_all(&tools)?;
  write_executable(
    &linker,
    r#"#!/bin/sh
{
  printf 'BEGIN\n'
  for argument in "$@"; do
    printf '%s\n' "$argument"
  done
  printf 'END\n'
} >> "$LINKER_LOG"
exec cc "$@"
"#,
  )?;
  fs::write(
    workspace.path.join("src/main.rs"),
    "fn main() { println!(\"linker parity\"); }\n",
  )?;
  fs::create_dir_all(workspace.path.join(".cargo"))?;
  let linker = linker.to_str().context("UTF-8 linker path")?;
  fs::write(
    workspace.path.join(".cargo/config.toml"),
    format!(
      "[target.{}]\nlinker = {}\n",
      rustc_host_target()?,
      serde_json::to_string(linker)?
    ),
  )?;
  fs::write(
    workspace.path.join(".gitignore"),
    "target/\ntarget-compatibility/\nlinker-arguments.log\n",
  )?;
  generate_front_door_lockfile(&workspace.path, cargo_home.path())?;
  workspace.commit("Add configured-linker compatibility fixture")?;

  let run = |rail: bool, no_cache: bool, explain: bool| -> Result<std::process::Output> {
    let mut command = if rail {
      let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-rail"));
      command.args(["rail", "run", "--quiet", "--all", "--action", "distribution"]);
      if no_cache {
        command.arg("--no-cache");
      }
      if explain {
        command.arg("--explain");
      }
      command.args(["--", "--quiet", "--target-dir"]).arg(&target);
      command
    } else {
      let mut command = Command::new("cargo");
      command
        .args([
          "build",
          "--workspace",
          "--release",
          "--locked",
          "--quiet",
          "--target-dir",
        ])
        .arg(&target);
      command
    };
    scrub_front_door_compiler_environment(&mut command);
    command
      .current_dir(&workspace.path)
      .env("CARGO_BUILD_JOBS", "1")
      .env("CARGO_HOME", cargo_home.path())
      .env("CARGO_INCREMENTAL", "0")
      .env("CARGO_NET_OFFLINE", "true")
      .env("CARGO_RAIL_CACHE_DIR", cache.path())
      .env("CARGO_TERM_COLOR", "never")
      .env("LINKER_LOG", &linker_log)
      .output()
      .context("run configured-linker front door")
  };
  let reset = || -> Result<()> {
    if target.exists() {
      fs::remove_dir_all(&target)?;
    }
    fs::write(&linker_log, "")?;
    Ok(())
  };

  reset()?;
  let direct = run(false, false, false)?;
  assert!(direct.status.success(), "direct Cargo failed: {direct:?}");
  let direct_linker_arguments = linker_arguments(&linker_log, &target)?;
  let direct_binary = fs::read(&binary)?;
  let direct_mode = fs::metadata(&binary)?.permissions().mode() & 0o777;
  assert!(
    !direct_linker_arguments.is_empty(),
    "the configured linker was not invoked"
  );
  assert_eq!(
    String::from_utf8_lossy(&Command::new(&binary).output()?.stdout).trim(),
    "linker parity"
  );

  for (label, no_cache) in [("cache disabled", true), ("cache requested", false)] {
    reset()?;
    let rail = run(true, no_cache, false)?;
    assert_eq!(rail.status.code(), direct.status.code(), "{label}: {rail:?}");
    assert_eq!(rail.stdout, direct.stdout, "{label} changed stdout");
    assert_eq!(rail.stderr, direct.stderr, "{label} changed stderr");
    assert_eq!(
      linker_arguments(&linker_log, &target)?,
      direct_linker_arguments,
      "{label} changed linker argv"
    );
    assert_eq!(fs::read(&binary)?, direct_binary, "{label} changed binary bytes");
    assert_eq!(
      fs::metadata(&binary)?.permissions().mode() & 0o777,
      direct_mode,
      "{label} changed binary mode"
    );
  }

  reset()?;
  let explained = run(true, false, true)?;
  assert!(
    explained.status.success(),
    "explained cargo-rail run failed: {explained:?}"
  );
  assert_eq!(explained.stderr, direct.stderr);
  assert!(
    String::from_utf8_lossy(&explained.stdout)
      .contains("native compiler cache: bypassed (configured_linker_not_graduated)"),
    "configured-linker bypass was not explicit: {}",
    String::from_utf8_lossy(&explained.stdout)
  );
  Ok(())
}

#[test]
fn bundled_codegen_backend_is_content_identified_and_transparent() -> Result<()> {
  let workspace = TestWorkspace::new_single_crate("codegen-backend-parity", "0.1.0")?;
  let cargo_home = tempfile::tempdir()?;
  let cache = tempfile::tempdir()?;
  let target = workspace.path.join("target-compatibility");
  fs::write(workspace.path.join(".gitignore"), "target/\ntarget-compatibility/\n")?;
  generate_front_door_lockfile(&workspace.path, cargo_home.path())?;
  workspace.commit("Add codegen-backend compatibility fixture")?;

  let run = |rail: bool, no_cache: bool, explain: bool, backend: &str| -> Result<std::process::Output> {
    let mut command = if rail {
      let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-rail"));
      command.args(["rail", "run", "--quiet", "--all", "--action", "build"]);
      if no_cache {
        command.arg("--no-cache");
      }
      if explain {
        command.arg("--explain");
      }
      command.args(["--", "--quiet", "--target-dir"]).arg(&target);
      command
    } else {
      let mut command = Command::new("cargo");
      command
        .args(["check", "--workspace", "--quiet", "--target-dir"])
        .arg(&target);
      command
    };
    scrub_front_door_compiler_environment(&mut command);
    command
      .current_dir(&workspace.path)
      .env("CARGO_BUILD_JOBS", "1")
      .env("CARGO_ENCODED_RUSTFLAGS", format!("-Zcodegen-backend={backend}"))
      .env("CARGO_HOME", cargo_home.path())
      .env("CARGO_INCREMENTAL", "0")
      .env("CARGO_NET_OFFLINE", "true")
      .env("CARGO_RAIL_CACHE_DIR", cache.path())
      .env("CARGO_TERM_COLOR", "never")
      .env("RUSTC_BOOTSTRAP", "1")
      .output()
      .context("run codegen-backend front door")
  };
  let reset = || -> Result<()> {
    if target.exists() {
      fs::remove_dir_all(&target)?;
    }
    Ok(())
  };

  reset()?;
  let direct = run(false, false, false, "llvm")?;
  assert!(direct.status.success(), "direct Cargo failed: {direct:?}");
  let direct_outputs = compiler_output_files(&target)?;
  assert!(
    !direct_outputs.is_empty(),
    "backend fixture produced no compiler output"
  );

  reset()?;
  let disabled = run(true, true, false, "llvm")?;
  assert_eq!(
    disabled.status.code(),
    direct.status.code(),
    "cache disabled: {disabled:?}"
  );
  assert_eq!(disabled.stdout, direct.stdout, "cache disabled changed stdout");
  assert_eq!(disabled.stderr, direct.stderr, "cache disabled changed stderr");
  assert_eq!(
    compiler_output_files(&target)?,
    direct_outputs,
    "cache disabled changed compiler outputs"
  );

  reset()?;
  let cold = run(true, false, false, "llvm")?;
  assert_eq!(cold.status.code(), direct.status.code(), "cache cold: {cold:?}");
  assert_eq!(cold.stdout, direct.stdout, "cache cold changed stdout");
  assert_eq!(cold.stderr, direct.stderr, "cache cold changed stderr");
  let cold_outputs = compiler_output_files(&target)?;
  assert!(!cold_outputs.is_empty(), "cache cold produced no compiler outputs");
  assert_eq!(
    cold_outputs.iter().map(|(path, _)| path).collect::<Vec<_>>(),
    direct_outputs.iter().map(|(path, _)| path).collect::<Vec<_>>(),
    "cache cold changed the direct Cargo compiler output inventory"
  );
  assert!(
    cold_outputs.iter().any(|(_, bytes)| bytes
      .windows(b"/cargo-rail/native-source/v2".len())
      .any(|window| window == b"/cargo-rail/native-source/v2")),
    "cache cold outputs did not use the portable source-root contract"
  );

  reset()?;
  let explained = run(true, false, true, "llvm")?;
  assert!(
    explained.status.success(),
    "explained cargo-rail run failed: {explained:?}"
  );
  assert_eq!(explained.stderr, direct.stderr);
  assert_eq!(
    compiler_output_files(&target)?,
    cold_outputs,
    "bundled-backend warm reuse changed cold cache output bytes"
  );
  assert!(
    String::from_utf8_lossy(&explained.stdout).contains("native compiler cache: hits=1"),
    "bundled backend did not restore a verified result: {}",
    String::from_utf8_lossy(&explained.stdout)
  );
  assert!(
    !String::from_utf8_lossy(&explained.stdout).contains("native compiler cache: bypassed"),
    "bundled backend disabled the whole native cache action: {}",
    String::from_utf8_lossy(&explained.stdout)
  );

  reset()?;
  let direct_failure = run(false, false, false, "cargo_rail_missing_backend")?;
  assert!(
    !direct_failure.status.success(),
    "the missing backend unexpectedly succeeded: {direct_failure:?}"
  );
  for (label, no_cache) in [("cache disabled", true), ("cache requested", false)] {
    reset()?;
    let rail = run(true, no_cache, false, "cargo_rail_missing_backend")?;
    assert_eq!(
      rail.status.code(),
      direct_failure.status.code(),
      "{label} changed the missing-backend exit status"
    );
    assert_eq!(
      rail.stdout, direct_failure.stdout,
      "{label} changed missing-backend stdout"
    );
    assert_eq!(
      rail.stderr, direct_failure.stderr,
      "{label} changed missing-backend diagnostics"
    );
  }
  Ok(())
}

#[test]
fn ineligible_diagnostic_workspace_never_installs_the_native_cache_wrapper() -> Result<()> {
  let workspace = wrapper_workspace("native-cache-ineligible-diagnostic-workspace")?;
  let manifest = workspace.path.join("crates/wrapper-app/Cargo.toml");
  let contents = fs::read_to_string(&manifest)?.replace("[package]\n", "[package]\nbuild = \"build.rs\"\n");
  fs::write(manifest, contents)?;
  fs::write(
    workspace.path.join("crates/wrapper-app/build.rs"),
    r#"fn main() {
  for name in [
    "CARGO_RAIL_COMPILER_CACHE_WRAPPER",
    "CARGO_RAIL_NATIVE_COMPILER_CACHE_SESSION",
  ] {
    assert!(std::env::var_os(name).is_none(), "native-cache capability leaked into build.rs: {name}");
  }
}
"#,
  )?;
  workspace.commit("Add native-cache-ineligible build script")?;

  let local_cache = tempfile::tempdir()?;
  let output = run_unify_without_ambient_wrappers(&workspace.path, local_cache.path())?;
  assert_eq!(output.status.code(), Some(1), "unexpected unify result: {output:?}");
  let observation = native_cache_observation(local_cache.path())?;
  assert_eq!(observation["execution"]["cache_wrapper"]["status"], "bypassed");
  assert_eq!(
    observation["execution"]["cache_wrapper"]["reason"],
    "build_script_observations_unavailable"
  );
  let roles = observation["execution"]["wrappers"]
    .as_array()
    .context("wrapper chain")?
    .iter()
    .map(|wrapper| wrapper["role"].as_str())
    .collect::<Vec<_>>();
  assert_eq!(roles, [Some("cargo_rail_diagnostic")]);
  Ok(())
}

#[test]
fn compiler_observation_records_verified_native_cache_miss_and_hit() -> Result<()> {
  let workspace = wrapper_workspace("disabled-cache-wrapper-observation")?;
  let local_cache = tempfile::tempdir()?;
  let output = run_unify_without_ambient_wrappers(&workspace.path, local_cache.path())?;
  assert_eq!(output.status.code(), Some(1), "unexpected unify result: {output:?}");
  let observation = native_cache_observation(local_cache.path())?;
  assert_eq!(observation["execution"]["cache_wrapper"]["status"], "miss");
  assert!(
    local_cache.path().join("cargo-rail/local-cas-v2").is_dir(),
    "native reuse must publish into the configured local CAS"
  );
  assert!(
    !workspace
      .path
      .join("target/cargo-rail/hermetic/local-cas-v1.json")
      .exists(),
    "native reuse must not recreate the obsolete workspace-local CAS reference"
  );

  let cache = compiler_evidence_cache_at(local_cache.path())?;
  let observation = local_observation(&cache, "wrapper_app")?;
  assert_eq!(
    observation["execution"]["cache_wrapper"]["status"], "miss",
    "unexpected native-cache observation: {observation}"
  );
  assert_eq!(
    observation["execution"]["cache_wrapper"]["reason"],
    "environment_selector_not_found;stored_verified_result"
  );
  let roles = observation["execution"]["wrappers"]
    .as_array()
    .context("wrapper chain")?
    .iter()
    .map(|wrapper| wrapper["role"].as_str())
    .collect::<Vec<_>>();
  assert_eq!(roles, [Some("cargo_rail_cache"), Some("cargo_rail_diagnostic")]);

  let target = workspace.path.join("target");
  let cold_outputs = compiler_output_files(&target)?;
  assert!(!cold_outputs.is_empty());
  let clean = Command::new("cargo")
    .current_dir(&workspace.path)
    .arg("clean")
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .env_remove(CACHE_WRAPPER_MARKER)
    .output()?;
  assert!(clean.status.success(), "cargo clean failed: {clean:?}");
  assert!(!target.exists(), "cargo clean must remove workspace target state");
  let output = run_unify_without_ambient_wrappers(&workspace.path, local_cache.path())?;
  assert_eq!(
    output.status.code(),
    Some(1),
    "unexpected warm unify result: {output:?}"
  );
  let cache_file = compiler_evidence_cache_at(local_cache.path())?;
  let observation = local_observation(&cache_file, "wrapper_app")?;
  assert_eq!(
    observation["execution"]["cache_wrapper"]["status"], "hit",
    "unexpected warm observation: {observation}"
  );
  assert_eq!(
    observation["execution"]["cache_wrapper"]["reason"],
    "verified_local_result"
  );
  assert_eq!(compiler_output_files(&target)?, cold_outputs);

  let first_action = observation["execution"]["cache_wrapper"]["action_key"]
    .as_str()
    .context("first-root action key")?
    .to_string();
  let second = wrapper_workspace("native-cache-second-independent-root")?;
  let second_hit = run_unify_without_ambient_wrappers(&second.path, local_cache.path())?;
  assert_eq!(
    second_hit.status.code(),
    Some(1),
    "second-root cache hit: {second_hit:?}"
  );
  let second_observation = native_cache_observation(local_cache.path())?;
  assert_eq!(second_observation["execution"]["cache_wrapper"]["status"], "hit");
  assert_eq!(
    second_observation["execution"]["cache_wrapper"]["action_key"], first_action,
    "equivalent workspace actions must have a checkout-root-independent identity"
  );
  let second_target = second.path.join("target");
  let second_outputs = compiler_output_files(&second_target)?;
  assert!(
    !second_outputs.is_empty(),
    "second-root reuse produced no compiler outputs"
  );
  fs::remove_dir_all(&second_target)?;
  let second_warm = run_unify_without_ambient_wrappers(&second.path, local_cache.path())?;
  assert_eq!(
    second_warm.status.code(),
    Some(1),
    "second-root warm run: {second_warm:?}"
  );
  let second_observation = native_cache_observation(local_cache.path())?;
  assert_eq!(second_observation["execution"]["cache_wrapper"]["status"], "hit");
  assert_eq!(compiler_output_files(&second_target)?, second_outputs);

  let cleanup = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(&second.path)
    .args(["rail", "clean", "--cache", "--quiet"])
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .env("CARGO_RAIL_CACHE_DIR", local_cache.path())
    .output()?;
  assert!(cleanup.status.success(), "native cache cleanup failed: {cleanup:?}");
  assert!(
    !local_cache.path().join("cargo-rail/local-cas-v2").exists(),
    "validated cleanup must remove the owned native CAS"
  );
  Ok(())
}

#[cfg(unix)]
#[test]
fn native_cache_partitions_exact_output_modes_by_effective_umask() -> Result<()> {
  use std::os::unix::fs::PermissionsExt as _;

  fn output_modes(target: &Path) -> Result<Vec<(PathBuf, u32)>> {
    fn visit(root: &Path, directory: &Path, modes: &mut Vec<(PathBuf, u32)>) -> Result<()> {
      let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
      entries.sort_unstable_by_key(std::fs::DirEntry::file_name);
      for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
          visit(root, &path, modes)?;
        } else if metadata.is_file()
          && path
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|extension| matches!(extension, "d" | "rmeta"))
        {
          modes.push((
            path.strip_prefix(root)?.to_path_buf(),
            metadata.permissions().mode() & 0o777,
          ));
        }
      }
      Ok(())
    }

    let mut modes = Vec::new();
    visit(target, target, &mut modes)?;
    Ok(modes)
  }

  let workspace = wrapper_workspace("native-cache-restrictive-umask")?;
  let local_cache = tempfile::tempdir()?;
  let cold = run_unify_with_umask(&workspace.path, local_cache.path(), "077")?;
  assert_eq!(cold.status.code(), Some(1), "unexpected cold result: {cold:?}");
  let cold_observation = native_cache_observation(local_cache.path())?;
  assert_eq!(cold_observation["execution"]["cache_wrapper"]["status"], "miss");
  let restrictive_action = cold_observation["execution"]["cache_wrapper"]["action_key"]
    .as_str()
    .context("restrictive-umask action key")?
    .to_string();
  let target = workspace.path.join("target");
  let cold_modes = output_modes(&target)?;
  assert!(!cold_modes.is_empty(), "fixture produced no graduated compiler outputs");
  assert!(
    cold_modes.iter().all(|(_, mode)| *mode == 0o600),
    "umask 077 must produce exact 0600 compiler outputs: {cold_modes:?}"
  );

  let clean = Command::new("cargo")
    .current_dir(&workspace.path)
    .arg("clean")
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .env_remove(CACHE_WRAPPER_MARKER)
    .output()?;
  assert!(clean.status.success(), "cargo clean failed: {clean:?}");

  let permissive_cold = run_unify_with_umask(&workspace.path, local_cache.path(), "022")?;
  assert_eq!(
    permissive_cold.status.code(),
    Some(1),
    "unexpected permissive cold result: {permissive_cold:?}"
  );
  let permissive_observation = native_cache_observation(local_cache.path())?;
  assert_eq!(
    permissive_observation["execution"]["cache_wrapper"]["status"], "miss",
    "a different effective file-creation mode must not restore restrictive outputs: {permissive_observation}"
  );
  let permissive_action = permissive_observation["execution"]["cache_wrapper"]["action_key"]
    .as_str()
    .context("permissive-umask action key")?
    .to_string();
  assert_ne!(permissive_action, restrictive_action);
  let permissive_modes = output_modes(&target)?;
  assert!(
    permissive_modes.iter().all(|(_, mode)| *mode == 0o644),
    "umask 022 must produce exact 0644 compiler outputs: {permissive_modes:?}"
  );

  let clean = Command::new("cargo")
    .current_dir(&workspace.path)
    .arg("clean")
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .env_remove(CACHE_WRAPPER_MARKER)
    .output()?;
  assert!(clean.status.success(), "second cargo clean failed: {clean:?}");

  let permissive_warm = run_unify_with_umask(&workspace.path, local_cache.path(), "022")?;
  assert_eq!(
    permissive_warm.status.code(),
    Some(1),
    "unexpected permissive warm result: {permissive_warm:?}"
  );
  let warm_observation = native_cache_observation(local_cache.path())?;
  assert_eq!(warm_observation["execution"]["cache_wrapper"]["status"], "hit");
  assert_eq!(
    warm_observation["execution"]["cache_wrapper"]["action_key"],
    permissive_action
  );
  assert_eq!(output_modes(&target)?, permissive_modes);
  Ok(())
}

#[test]
fn restore_commit_falls_back_only_before_its_first_visible_effect() -> Result<()> {
  let workspace = wrapper_workspace("native-cache-restore-commit")?;
  let cache = tempfile::tempdir()?;
  let target = workspace.path.join("target");
  fs::write(
    workspace.path.join("crates/wrapper-app/src/lib.rs"),
    "pub fn restore_stream_warning() {\n  let restore_stream_sentinel = 1;\n  wrapper_dep::dependency();\n}\n",
  )?;
  let cold = run_direct_native_build(&workspace.path, cache.path())?;
  assert!(
    String::from_utf8_lossy(&cold.stderr).contains("restore_stream_sentinel"),
    "fixture must produce a replayable compiler diagnostic: {cold:?}"
  );
  let cold_event = native_unit_event(&cold, "wrapper_app")?;
  let output_count = cold_event["unit"]["output_paths"]
    .as_array()
    .context("graduated restore output paths")?
    .len();
  assert!(output_count >= 2, "fixture lost graduated outputs: {cold_event}");
  let cold_outputs = compiler_artifacts_for_crate(&target, "wrapper_app")?;
  assert_eq!(
    cold_outputs.len(),
    output_count,
    "fixture output inventory does not match the restore contract"
  );

  let before_effect_phases = [
    "after_registration",
    "before_marker_publish",
    "after_pending_commit",
    "after_marker_publish",
    "after_marker",
  ];
  for phase in before_effect_phases {
    fs::remove_dir_all(&target)?;
    let before_effect = run_direct_native_build_with_environment(
      &workspace.path,
      cache.path(),
      &[
        ("CARGO_RAIL_TEST_NATIVE_RESTORE_FAULT", phase),
        (RESTORE_CRATE_ENV, "wrapper_app"),
      ],
    )?;
    assert!(
      before_effect.status.success(),
      "fault {phase} before the first output must compile cold: {before_effect:?}"
    );
    assert_ne!(native_unit_event(&before_effect, "wrapper_app")?["outcome"], "hit");
    assert_eq!(compiler_artifacts_for_crate(&target, "wrapper_app")?, cold_outputs);
    assert!(
      native_restore_residue(&target)?.is_empty(),
      "pre-effect fault {phase} retained private restore state"
    );
  }

  let mut after_effect_faults = (1..=output_count)
    .map(|index| (format!("after_output:{index}"), false))
    .collect::<Vec<_>>();
  after_effect_faults.extend([
    ("after_observation".to_string(), false),
    ("after_stdout".to_string(), false),
    ("after_stderr".to_string(), true),
    ("before_marker_removal".to_string(), true),
    ("after_marker_removal".to_string(), true),
  ]);
  for (phase, replayed_stderr) in &after_effect_faults {
    fs::remove_dir_all(&target)?;
    let failed = run_direct_native_build_with_environment(
      &workspace.path,
      cache.path(),
      &[
        ("CARGO_RAIL_TEST_NATIVE_RESTORE_FAULT", phase.as_str()),
        (RESTORE_CRATE_ENV, "wrapper_app"),
      ],
    )?;
    assert!(
      !failed.status.success(),
      "fault {phase} ran rustc after the first visible effect: {failed:?}"
    );
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(
      stderr.contains("injected native restore-commit fault"),
      "fault {phase} did not remain operational: {failed:?}"
    );
    assert_eq!(
      stderr.contains("restore_stream_sentinel"),
      *replayed_stderr,
      "fault {phase} crossed the wrong stderr boundary: {failed:?}"
    );
    let artifacts = compiler_artifacts_for_crate(&target, "wrapper_app")?;
    if phase == "after_marker_removal" {
      assert_eq!(artifacts, cold_outputs, "fault {phase} lost a completed output set");
    } else {
      assert!(
        artifacts.is_empty(),
        "fault {phase} retained a partially published compiler output"
      );
    }
    assert!(
      native_restore_residue(&target)?.is_empty(),
      "fault {phase} retained private restore state"
    );
    let retry = run_direct_native_build(&workspace.path, cache.path())?;
    assert_eq!(
      native_unit_event(&retry, "wrapper_app")?["outcome"],
      "hit",
      "fault {phase} damaged reusable CAS authority"
    );
    assert_eq!(compiler_artifacts_for_crate(&target, "wrapper_app")?, cold_outputs);
  }

  for phase in before_effect_phases {
    fs::remove_dir_all(&target)?;
    let cancelled_before_effect = run_direct_native_build_with_environment(
      &workspace.path,
      cache.path(),
      &[(RESTORE_CANCEL_ENV, phase), (RESTORE_CRATE_ENV, "wrapper_app")],
    )?;
    assert!(
      cancelled_before_effect.status.success(),
      "pre-effect cancellation {phase} must compile cold: {cancelled_before_effect:?}"
    );
    assert_ne!(
      native_unit_event(&cancelled_before_effect, "wrapper_app")?["outcome"],
      "hit"
    );
    assert_eq!(compiler_artifacts_for_crate(&target, "wrapper_app")?, cold_outputs);
    assert!(native_restore_residue(&target)?.is_empty());
  }

  for (phase, replayed_stderr) in &after_effect_faults {
    fs::remove_dir_all(&target)?;
    let cancelled = run_direct_native_build_with_environment(
      &workspace.path,
      cache.path(),
      &[(RESTORE_CANCEL_ENV, phase.as_str()), (RESTORE_CRATE_ENV, "wrapper_app")],
    )?;
    assert!(
      !cancelled.status.success(),
      "cancellation {phase} ran rustc after the first visible effect: {cancelled:?}"
    );
    let stderr = String::from_utf8_lossy(&cancelled.stderr);
    assert!(
      stderr.contains("cancelled native restore-commit"),
      "cancellation {phase} did not remain operational: {cancelled:?}"
    );
    assert_eq!(
      stderr.contains("restore_stream_sentinel"),
      *replayed_stderr,
      "cancellation {phase} crossed the wrong stderr boundary: {cancelled:?}"
    );
    let artifacts = compiler_artifacts_for_crate(&target, "wrapper_app")?;
    if phase == "after_marker_removal" {
      assert_eq!(
        artifacts, cold_outputs,
        "cancellation {phase} lost a completed output set"
      );
    } else {
      assert!(artifacts.is_empty());
    }
    assert!(native_restore_residue(&target)?.is_empty());
    let retry = run_direct_native_build(&workspace.path, cache.path())?;
    assert_eq!(native_unit_event(&retry, "wrapper_app")?["outcome"], "hit");
    assert_eq!(compiler_artifacts_for_crate(&target, "wrapper_app")?, cold_outputs);
  }

  for phase in ["after_registration", "before_marker_publish", "after_pending_commit"] {
    fs::remove_dir_all(&target)?;
    let killed = run_direct_native_build_with_environment(
      &workspace.path,
      cache.path(),
      &[
        ("CARGO_RAIL_TEST_NATIVE_RESTORE_ABORT", phase),
        (RESTORE_CRATE_ENV, "wrapper_app"),
      ],
    )?;
    assert!(
      !killed.status.success(),
      "pre-authority abort {phase} unexpectedly succeeded"
    );
    assert!(
      restore_markers(&target)?.is_empty(),
      "pre-authority abort {phase} exposed an authority marker"
    );
    let transactions = restore_transactions(&target)?;
    let [transaction] = transactions.as_slice() else {
      panic!("pre-authority abort {phase} must retain one exact transaction: {transactions:?}");
    };
    if phase == "after_registration" {
      let mut entries = fs::read_dir(transaction)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()?;
      entries.sort_unstable();
      assert_eq!(
        entries,
        [std::ffi::OsString::from("registration.json")],
        "registration must be durable before materialization begins"
      );
    }
    let parent = transaction.parent().context("restore transaction parent")?;
    let scan_tripwire = install_restore_scan_tripwire(parent, None)?;
    let recovered = run_direct_native_build(&workspace.path, cache.path())?;
    assert_eq!(
      native_unit_event(&recovered, "wrapper_app")?["outcome"],
      "hit",
      "pre-authority abort {phase} did not recover before exact cache reuse"
    );
    assert!(scan_tripwire.is_file(), "recovery scanned an unrelated marker");
    fs::remove_file(scan_tripwire)?;
    assert_eq!(compiler_artifacts_for_crate(&target, "wrapper_app")?, cold_outputs);
    assert!(native_restore_residue(&target)?.is_empty());
  }

  let mut death_phases = vec![
    ("after_marker_publish".to_string(), false),
    ("after_marker".to_string(), false),
  ];
  death_phases.extend(
    (1..=output_count)
      .map(|index| (format!("after_output:{index}"), false))
      .collect::<Vec<_>>(),
  );
  death_phases.extend([
    ("after_observation".to_string(), false),
    ("after_stdout".to_string(), false),
    ("after_stderr".to_string(), true),
    ("before_marker_removal".to_string(), true),
  ]);
  for (phase, replayed_stderr) in &death_phases {
    fs::remove_dir_all(&target)?;
    let killed = run_direct_native_build_with_environment(
      &workspace.path,
      cache.path(),
      &[
        ("CARGO_RAIL_TEST_NATIVE_RESTORE_ABORT", phase.as_str()),
        (RESTORE_CRATE_ENV, "wrapper_app"),
      ],
    )?;
    assert!(!killed.status.success(), "abort {phase} unexpectedly succeeded");
    assert_eq!(
      String::from_utf8_lossy(&killed.stderr).contains("restore_stream_sentinel"),
      *replayed_stderr,
      "abort {phase} crossed the wrong stderr boundary: {killed:?}"
    );
    let markers = restore_markers(&target)?;
    assert_eq!(
      markers.len(),
      1,
      "abort {phase} must retain one exact recovery marker: {markers:?}"
    );
    let marker = &markers[0];
    if phase == "after_marker" {
      let marker_json: serde_json::Value = serde_json::from_slice(&fs::read(marker)?)?;
      let transaction = PathBuf::from(
        marker_json["transaction_directory"]
          .as_str()
          .context("restore marker transaction directory")?,
      );
      let unknown = transaction.join("unknown-member");
      fs::write(&unknown, b"must survive failed recovery")?;
      let unknown_member = run_direct_native_build_unchecked(&workspace.path, cache.path())?;
      assert!(
        !unknown_member.status.success(),
        "an unknown transaction member must fail closed"
      );
      assert!(marker.is_file() && unknown.is_file());
      fs::remove_file(unknown)?;

      let registration = fs::read(transaction.join("registration.json"))?;
      let replaced = transaction.with_extension("replaced");
      fs::rename(&transaction, &replaced)?;
      fs::create_dir(&transaction)?;
      fs::write(transaction.join("registration.json"), registration)?;
      let replaced_directory = run_direct_native_build_unchecked(&workspace.path, cache.path())?;
      assert!(
        !replaced_directory.status.success(),
        "a replaced transaction directory must fail closed"
      );
      assert!(marker.is_file() && transaction.is_dir() && replaced.is_dir());
      fs::remove_dir_all(&transaction)?;
      fs::rename(replaced, &transaction)?;
    }
    if phase == "after_output:1" {
      let marker_bytes = fs::read(marker)?;
      fs::write(marker, b"{}")?;
      let malformed = run_direct_native_build_unchecked(&workspace.path, cache.path())?;
      assert!(
        !malformed.status.success(),
        "a malformed recovery marker must fail closed"
      );
      assert!(marker.is_file(), "failed recovery must retain its marker");
      fs::write(marker, marker_bytes)?;
    }
    let scan_tripwire = install_restore_scan_tripwire(marker.parent().context("restore marker parent")?, Some(marker))?;
    let recovered = run_direct_native_build(&workspace.path, cache.path())?;
    assert_eq!(
      native_unit_event(&recovered, "wrapper_app")?["outcome"],
      "hit",
      "abort {phase} did not recover before exact cache reuse"
    );
    assert!(!marker.exists(), "abort {phase} retained its exact recovery marker");
    assert!(
      scan_tripwire.is_file(),
      "abort {phase} scanned or consumed an unrelated restore marker"
    );
    fs::remove_file(scan_tripwire)?;
    assert_eq!(compiler_artifacts_for_crate(&target, "wrapper_app")?, cold_outputs);
    let residue = native_restore_residue(&target)?;
    assert!(
      residue.is_empty(),
      "abort {phase} recovery retained private restore state: {residue:?}"
    );
  }

  fs::remove_dir_all(&target)?;
  let committed_death = run_direct_native_build_with_environment(
    &workspace.path,
    cache.path(),
    &[
      ("CARGO_RAIL_TEST_NATIVE_RESTORE_ABORT", "after_marker_removal"),
      (RESTORE_CRATE_ENV, "wrapper_app"),
    ],
  )?;
  assert!(
    !committed_death.status.success(),
    "post-commit abort unexpectedly succeeded"
  );
  assert!(
    String::from_utf8_lossy(&committed_death.stderr).contains("restore_stream_sentinel"),
    "post-commit abort lost the already replayed stderr"
  );
  assert!(
    restore_markers(&target)?.is_empty(),
    "post-commit abort recreated a removed recovery marker"
  );
  assert_eq!(
    compiler_artifacts_for_crate(&target, "wrapper_app")?,
    cold_outputs,
    "post-commit abort did not leave a complete output set"
  );
  let committed_transactions = restore_transactions(&target)?;
  assert_eq!(
    committed_transactions.len(),
    1,
    "post-commit abort must retain only its private transaction: {committed_transactions:?}"
  );
  let completed_retry = run_direct_native_build(&workspace.path, cache.path())?;
  assert_eq!(native_unit_event(&completed_retry, "wrapper_app")?["outcome"], "hit");
  assert_eq!(compiler_artifacts_for_crate(&target, "wrapper_app")?, cold_outputs);
  assert!(native_restore_residue(&target)?.is_empty());
  Ok(())
}

#[test]
fn native_cache_mutations_produce_no_false_hits() -> Result<()> {
  let workspace = wrapper_workspace("native-cache-mutation-matrix")?;
  let source = workspace.path.join("crates/wrapper-app/src/lib.rs");
  fs::write(
    &source,
    "pub const BUILD_VALUE: &str = env!(\"P73_VALUE\");\n\
     pub const OPTIONAL_BUILD_VALUE: Option<&str> = option_env!(\"P73_OPTIONAL\");\n\
     pub const MARKER: u8 = 1;\n",
  )?;
  workspace.commit("Add native cache mutation fixture")?;
  let dependency = workspace.path.join("vendor/wrapper-dep/src/lib.rs");
  let local_cache = tempfile::tempdir()?;
  let target = workspace.path.join("target");

  let cold = run_unify_with_environment(&workspace.path, local_cache.path(), &[("P73_VALUE", "one")])?;
  assert_eq!(cold.status.code(), Some(1), "cold run: {cold:?}");
  let cold_observation = native_cache_observation(local_cache.path())?;
  assert_eq!(cold_observation["execution"]["cache_wrapper"]["status"], "miss");
  fs::remove_dir_all(&target)?;
  let warm = run_unify_with_environment(&workspace.path, local_cache.path(), &[("P73_VALUE", "one")])?;
  assert_eq!(warm.status.code(), Some(1), "warm run: {warm:?}");
  assert_eq!(
    native_cache_observation(local_cache.path())?["execution"]["cache_wrapper"]["status"],
    "hit"
  );
  let baseline_action_key = native_cache_observation(local_cache.path())?["execution"]["cache_wrapper"]["action_key"]
    .as_str()
    .context("baseline native-cache action key")?
    .to_string();
  let mut prior_action_keys = vec![baseline_action_key];

  fs::remove_dir_all(&target)?;
  let unrelated_environment = run_unify_with_environment(
    &workspace.path,
    local_cache.path(),
    &[("P73_VALUE", "one"), ("P73_UNRELATED", "changed")],
  )?;
  assert_eq!(
    unrelated_environment.status.code(),
    Some(1),
    "unrelated environment run"
  );
  let unrelated_observation = native_cache_observation(local_cache.path())?;
  assert_eq!(
    unrelated_observation["execution"]["cache_wrapper"]["status"], "hit",
    "an environment variable rustc did not select must cross-hit: {unrelated_observation}"
  );
  assert_eq!(
    unrelated_observation["execution"]["cache_wrapper"]["action_key"], prior_action_keys[0],
    "an environment variable rustc did not select changed action identity"
  );

  fs::remove_dir_all(&target)?;
  let optional_environment = run_unify_with_environment(
    &workspace.path,
    local_cache.path(),
    &[("P73_VALUE", "one"), ("P73_OPTIONAL", "present")],
  )?;
  let (_, optional_environment_key) =
    assert_native_invalidation(local_cache.path(), &optional_environment, &prior_action_keys)?;
  prior_action_keys.push(optional_environment_key);

  fs::remove_dir_all(&target)?;
  let forced_incremental = run_unify_with_environment(
    &workspace.path,
    local_cache.path(),
    &[("P73_VALUE", "one"), ("RUSTC_FORCE_INCREMENTAL", "1")],
  )?;
  let forced_observation = assert_native_miss(local_cache.path(), &forced_incremental)?;
  assert_eq!(forced_observation["execution"]["cache_wrapper"]["status"], "bypassed");
  assert_eq!(
    forced_observation["execution"]["cache_wrapper"]["reason"],
    "forced_incremental_compilation_not_graduated"
  );

  fs::remove_dir_all(&target)?;
  fs::write(
    &source,
    "pub const BUILD_VALUE: &str = env!(\"P73_VALUE\");\n\
     pub const OPTIONAL_BUILD_VALUE: Option<&str> = option_env!(\"P73_OPTIONAL\");\n\
     pub const MARKER: u8 = 2;\n",
  )?;
  let source_mutation = run_unify_with_environment(
    &workspace.path,
    local_cache.path(),
    &[("P73_VALUE", "one"), ("P73_OPTIONAL", "present")],
  )?;
  let (_, source_action_key) = assert_native_invalidation(local_cache.path(), &source_mutation, &prior_action_keys)?;
  prior_action_keys.push(source_action_key);

  fs::remove_dir_all(&target)?;
  let environment_mutation = run_unify_with_environment(
    &workspace.path,
    local_cache.path(),
    &[("P73_VALUE", "two"), ("P73_OPTIONAL", "present")],
  )?;
  let (_, environment_action_key) =
    assert_native_invalidation(local_cache.path(), &environment_mutation, &prior_action_keys)?;
  prior_action_keys.push(environment_action_key);

  fs::remove_dir_all(&target)?;
  let flag_mutation = run_unify_with_environment(
    &workspace.path,
    local_cache.path(),
    &[
      ("P73_VALUE", "two"),
      ("P73_OPTIONAL", "present"),
      ("RUSTFLAGS", "--cfg=p73_mutation"),
    ],
  )?;
  let (_, flag_action_key) = assert_native_invalidation(local_cache.path(), &flag_mutation, &prior_action_keys)?;
  prior_action_keys.push(flag_action_key);

  fs::remove_dir_all(&target)?;
  let compiler_environment_mutation = run_unify_with_environment(
    &workspace.path,
    local_cache.path(),
    &[
      ("P73_VALUE", "two"),
      ("P73_OPTIONAL", "present"),
      ("RUSTC_BOOTSTRAP", "1"),
    ],
  )?;
  let (_, compiler_environment_action_key) =
    assert_native_invalidation(local_cache.path(), &compiler_environment_mutation, &prior_action_keys)?;
  prior_action_keys.push(compiler_environment_action_key);

  fs::remove_dir_all(&target)?;
  fs::write(&dependency, "pub fn dependency() { let _changed = true; }\n")?;
  let dependency_mutation = run_unify_with_environment(
    &workspace.path,
    local_cache.path(),
    &[("P73_VALUE", "two"), ("P73_OPTIONAL", "present")],
  )?;
  assert_native_invalidation(local_cache.path(), &dependency_mutation, &prior_action_keys)?;
  Ok(())
}

#[test]
fn source_capture_limit_bypasses_cold_without_creating_authority() -> Result<()> {
  let workspace = wrapper_workspace("native-cache-source-capture-limit")?;
  let cache = tempfile::tempdir()?;
  let mut directory = workspace.path.join("crates/wrapper-app/src");
  for index in 0..129 {
    directory = directory.join(format!("d{index}"));
    fs::create_dir(&directory)?;
  }

  for attempt in 0..2 {
    let target = workspace.path.join("target");
    if target.exists() {
      fs::remove_dir_all(&target)?;
    }
    let output = run_direct_native_build(&workspace.path, cache.path())?;
    let event = native_event_with_reason(&output, "complete_action_capture_unavailable")?;
    assert_eq!(
      event["outcome"], "bypassed",
      "attempt {attempt} unexpectedly acquired native authority: {event}"
    );
    assert!(
      event["unit_identity"].is_null() && event["result_key"].is_null(),
      "a bounded-capture failure must not expose authority: {event}"
    );
  }
  Ok(())
}

#[test]
fn source_state_races_never_publish_or_restore_false_authority() -> Result<()> {
  #[derive(Clone, Copy, Debug)]
  enum Mutation {
    Replacement,
    DirectoryMembership,
    ByteRestoringAba,
  }

  let workspace = wrapper_workspace("native-cache-source-state-races")?;
  let cache = tempfile::tempdir()?;
  let source = workspace.path.join("crates/wrapper-app/src/lib.rs");
  let target = workspace.path.join("target");
  let phases = [
    ("after_initial_capture", true),
    ("before_restore_revalidation", true),
    ("before_admission_revalidation", false),
  ];
  let mutations = [
    Mutation::Replacement,
    Mutation::DirectoryMembership,
    Mutation::ByteRestoringAba,
  ];

  for (phase_index, (phase, seed_authority)) in phases.into_iter().enumerate() {
    for (mutation_index, mutation) in mutations.into_iter().enumerate() {
      let case = phase_index * mutations.len() + mutation_index;
      let baseline = format!("pub fn capture_race_value() -> u16 {{ {case} }}\n");
      replace_regular_file(&source, baseline.as_bytes(), &format!("baseline-{case}"))?;
      if target.exists() {
        fs::remove_dir_all(&target)?;
      }

      let seeded_action = if seed_authority {
        let seeded = run_direct_native_build(&workspace.path, cache.path())?;
        let event = native_unit_event(&seeded, "wrapper_app")?;
        assert_eq!(event["outcome"], "miss", "failed to seed {phase}/{mutation:?}: {event}");
        let action = event["unit_identity"]
          .as_str()
          .context("seeded source-race action identity")?
          .to_string();
        fs::remove_dir_all(&target)?;
        Some(action)
      } else {
        None
      };

      let raced = run_direct_native_build_paused(&workspace.path, cache.path(), phase, || {
        match mutation {
          Mutation::Replacement => {
            let changed = format!("pub fn capture_race_value() -> u16 {{ {} }}\n", case + 100);
            replace_regular_file(&source, changed.as_bytes(), &format!("replacement-{case}"))?;
          }
          Mutation::DirectoryMembership => {
            let transient = source
              .parent()
              .context("source namespace")?
              .join(format!("transient-{case}"));
            fs::create_dir(&transient)?;
            fs::remove_dir(&transient)?;
          }
          Mutation::ByteRestoringAba => {
            let changed = format!("pub fn capture_race_value() -> u16 {{ {} }}\n", case + 200);
            replace_regular_file(&source, changed.as_bytes(), &format!("aba-b-{case}"))?;
            replace_regular_file(&source, baseline.as_bytes(), &format!("aba-a-{case}"))?;
          }
        }
        Ok(())
      })?;
      assert!(
        raced.status.success(),
        "source race {phase}/{mutation:?} failed: {raced:?}"
      );
      let raced_event = native_unit_event(&raced, "wrapper_app")?;
      assert_ne!(
        raced_event["outcome"], "hit",
        "source race {phase}/{mutation:?} restored a result across changed captured state: {raced_event}"
      );
      assert!(
        raced_event["result_key"].is_null(),
        "source race {phase}/{mutation:?} published a result: {raced_event}"
      );
      if let Some(seeded_action) = seeded_action {
        assert_eq!(
          raced_event["unit_identity"], seeded_action,
          "private pause controls changed the action identity for {phase}/{mutation:?}: {raced_event}"
        );
      } else {
        if matches!(mutation, Mutation::Replacement) {
          replace_regular_file(&source, baseline.as_bytes(), &format!("restore-baseline-{case}"))?;
        }
        if target.exists() {
          fs::remove_dir_all(&target)?;
        }
        let proof = run_direct_native_build(&workspace.path, cache.path())?;
        let proof_event = native_unit_event(&proof, "wrapper_app")?;
        assert_eq!(
          proof_event["outcome"], "miss",
          "rejected admission left authority behind for {phase}/{mutation:?}: {proof_event}"
        );
        assert_eq!(
          proof_event["unit_identity"], raced_event["unit_identity"],
          "private pause controls changed the action identity for {phase}/{mutation:?}"
        );
      }
    }
  }
  Ok(())
}

#[test]
fn same_file_x_y_x_after_rustc_before_admission_never_publishes_y_under_x() -> Result<()> {
  const RUSTC_Y_MARKER: &str = "cargo_rail_rustc_consumed_y_marker";

  let workspace = wrapper_workspace("native-cache-two-barrier-aba")?;
  let cache = tempfile::tempdir()?;
  let source = workspace.path.join("crates/wrapper-app/src/lib.rs");
  let target = workspace.path.join("target");
  let x = "pub fn capture_aba_value() -> u16 { 1 }\n".to_string();
  let y = format!("#[warn(unused_variables)]\npub fn capture_aba_value() -> u16 {{ let {RUSTC_Y_MARKER} = 2; 2 }}\n");
  let width = x.len().max(y.len());
  let x = format!("{x}{}", " ".repeat(width - x.len()));
  let y = format!("{y}{}", " ".repeat(width - y.len()));
  fs::write(&source, &x)?;
  let original_modified = fs::metadata(&source)?.modified()?;
  if target.exists() {
    fs::remove_dir_all(&target)?;
  }

  let raced = run_direct_native_build_two_barriers(
    &workspace.path,
    cache.path(),
    "after_initial_capture",
    || {
      fs::write(&source, &y)?;
      Ok(())
    },
    "after_compiler_execution",
    || {
      assert_eq!(
        fs::read(&source)?,
        y.as_bytes(),
        "rustc did not execute between barriers"
      );
      fs::write(&source, &x)?;
      OpenOptions::new()
        .write(true)
        .open(&source)?
        .set_times(fs::FileTimes::new().set_modified(original_modified))?;
      Ok(())
    },
  )?;
  assert!(raced.status.success(), "two-barrier ABA build failed: {raced:?}");
  let compiler_output = format!(
    "{}\n{}",
    String::from_utf8_lossy(&raced.stdout),
    String::from_utf8_lossy(&raced.stderr)
  );
  assert!(
    compiler_output.contains(RUSTC_Y_MARKER),
    "rustc did not report the Y-only marker before X was restored: {compiler_output}"
  );
  let raced_event = native_unit_event(&raced, "wrapper_app")?;
  assert_ne!(
    raced_event["outcome"], "hit",
    "ABA race restored authority: {raced_event}"
  );
  assert!(
    raced_event["result_key"].is_null(),
    "Y gained publication authority under X: {raced_event}"
  );

  fs::remove_dir_all(&target)?;
  let x_proof = run_direct_native_build(&workspace.path, cache.path())?;
  let x_event = native_unit_event(&x_proof, "wrapper_app")?;
  assert_eq!(x_event["outcome"], "miss", "restored X reused Y authority: {x_event}");
  assert_eq!(
    x_event["unit_identity"], raced_event["unit_identity"],
    "the two-barrier test did not restore the original X action"
  );
  Ok(())
}

#[test]
fn corrupt_native_cache_object_falls_back_to_exact_cold_outputs() -> Result<()> {
  fn collect_blobs(directory: &Path, blobs: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory)? {
      let path = entry?.path();
      let metadata = fs::symlink_metadata(&path)?;
      if metadata.is_dir() {
        collect_blobs(&path, blobs)?;
      } else if metadata.is_file() && path.extension().is_some_and(|extension| extension == "blob") {
        blobs.push(path);
      }
    }
    Ok(())
  }

  let workspace = wrapper_workspace("native-cache-corrupt-object")?;
  let local_cache = tempfile::tempdir()?;
  let cold = run_unify_without_ambient_wrappers(&workspace.path, local_cache.path())?;
  assert_native_miss(local_cache.path(), &cold)?;
  let target = workspace.path.join("target");
  let cold_outputs = compiler_output_files(&target)?;
  let mut blobs = Vec::new();
  collect_blobs(local_cache.path(), &mut blobs)?;
  assert!(!blobs.is_empty(), "native cache fixture must publish blobs");
  for blob in blobs {
    fs::write(blob, b"truncated")?;
  }

  fs::remove_dir_all(&target)?;
  let fallback = run_unify_without_ambient_wrappers(&workspace.path, local_cache.path())?;
  let observation = assert_native_miss(local_cache.path(), &fallback)?;
  assert!(
    observation["execution"]["cache_wrapper"]["reason"]
      .as_str()
      .is_some_and(|reason| reason.contains("local_cache_store_failed") || reason.contains("materialization")),
    "corruption must be explicit: {observation}"
  );
  assert_eq!(compiler_output_files(&target)?, cold_outputs);
  Ok(())
}

#[test]
fn filesystem_macro_inputs_are_reused_and_invalidated_from_dep_info() -> Result<()> {
  let workspace = wrapper_workspace("native-cache-filesystem-macro")?;
  let included = workspace.path.join("crates/wrapper-app/src/value.txt");
  fs::write(&included, "observed\n")?;
  fs::write(
    workspace.path.join("crates/wrapper-app/src/lib.rs"),
    "pub const VALUE: &str = include_str!(\"value.txt\");\n",
  )?;
  workspace.commit("Add filesystem macro fixture")?;
  let local_cache = tempfile::tempdir()?;
  let target = workspace.path.join("target");

  let cold = run_direct_native_build(&workspace.path, local_cache.path())?;
  let cold_event = native_unit_event(&cold, "wrapper_app")?;
  assert_eq!(cold_event["outcome"], "miss", "unexpected cold event: {cold_event}");
  assert_eq!(cold_event["reason"], "empty_local_authority;stored_verified_result");
  assert!(
    compiler_output_files(&target)?.iter().any(|(path, bytes)| path
      .extension()
      .is_some_and(|extension| extension == "d")
      && bytes.windows(b"value.txt".len()).any(|window| window == b"value.txt")),
    "rustc dep-info did not report the included file"
  );
  let original_action_key = cold_event["unit_identity"]
    .as_str()
    .context("filesystem macro cold action key")?
    .to_string();

  fs::remove_dir_all(&target)?;
  let warm = run_direct_native_build(&workspace.path, local_cache.path())?;
  let warm_event = native_unit_event(&warm, "wrapper_app")?;
  assert_eq!(
    warm_event["outcome"],
    "hit",
    "unexpected warm event:\ncold: {cold_event}\nwarm: {warm_event}\nstderr:\n{}",
    String::from_utf8_lossy(&warm.stderr)
  );
  assert_eq!(warm_event["unit_identity"], original_action_key);

  fs::remove_dir_all(&target)?;
  fs::write(&included, "changed!\n")?;
  let changed = run_direct_native_build(&workspace.path, local_cache.path())?;
  let changed_event = native_unit_event(&changed, "wrapper_app")?;
  assert_eq!(
    changed_event["outcome"], "miss",
    "unexpected changed event: {changed_event}"
  );
  let changed_action_key = changed_event["unit_identity"]
    .as_str()
    .context("filesystem macro changed action key")?;
  assert_ne!(changed_action_key, original_action_key);

  fs::remove_dir_all(&target)?;
  fs::write(&included, "observed\n")?;
  let restored = run_direct_native_build(&workspace.path, local_cache.path())?;
  let restored_event = native_unit_event(&restored, "wrapper_app")?;
  assert_eq!(
    restored_event["outcome"], "hit",
    "unexpected restored event: {restored_event}"
  );
  assert_eq!(restored_event["unit_identity"], original_action_key);
  assert_ne!(restored_event["unit_identity"], changed_action_key);
  Ok(())
}

#[test]
fn competing_module_paths_never_restore_a_positive_only_result() -> Result<()> {
  for (layout, initial, competing) in [
    (
      "flat-first",
      "crates/wrapper-app/src/foo.rs",
      "crates/wrapper-app/src/foo/mod.rs",
    ),
    (
      "nested-first",
      "crates/wrapper-app/src/foo/mod.rs",
      "crates/wrapper-app/src/foo.rs",
    ),
  ] {
    let workspace = wrapper_workspace(&format!("native-cache-competing-module-{layout}"))?;
    fs::write(
      workspace.path.join("crates/wrapper-app/src/lib.rs"),
      "mod foo;\npub use foo::value;\n",
    )?;
    let initial = workspace.path.join(initial);
    fs::create_dir_all(initial.parent().context("initial module parent")?)?;
    fs::write(&initial, "pub fn value() -> u8 { 1 }\n")?;
    let cache = tempfile::tempdir()?;

    run_direct_native_build(&workspace.path, cache.path())?;

    fs::remove_dir_all(workspace.path.join("target"))?;
    let competing = workspace.path.join(competing);
    fs::create_dir_all(competing.parent().context("competing module parent")?)?;
    fs::write(&competing, "pub fn value() -> u8 { 2 }\n")?;

    let rejected = run_direct_native_build_unchecked(&workspace.path, cache.path())?;
    assert!(
      !rejected.status.success(),
      "{layout} restored a stale result after a competing module path appeared: {rejected:?}"
    );
    assert!(
      String::from_utf8_lossy(&rejected.stderr).contains("E0761"),
      "{layout} did not preserve rustc's competing-module diagnostic: {rejected:?}"
    );
  }
  Ok(())
}

#[test]
fn output_neutral_cargo_configuration_reuses_across_output_directory_changes() -> Result<()> {
  let workspace = wrapper_workspace("native-cache-output-neutral-configuration")?;
  let configuration_directory = workspace.path.join(".cargo");
  fs::create_dir_all(&configuration_directory)?;
  let configuration = configuration_directory.join("config.toml");
  fs::write(
    &configuration,
    "[build]\nbuild-dir = \"build-one\"\njobs = 1\ntarget-dir = \"target-one\"\nwarnings = \"warn\"\n\n[net]\noffline = true\nretry = 1\n",
  )?;
  let local_cache = tempfile::tempdir()?;

  let cold = run_direct_native_build(&workspace.path, local_cache.path())?;
  let cold_event = native_unit_event(&cold, "wrapper_app")?;
  assert_eq!(cold_event["outcome"], "miss", "unexpected cold event: {cold_event}");
  let cold_action = cold_event["unit_identity"]
    .as_str()
    .context("output-neutral cold action key")?
    .to_string();
  for directory in ["build-one", "target-one"] {
    let path = workspace.path.join(directory);
    if path.exists() {
      fs::remove_dir_all(path)?;
    }
  }

  fs::write(
    &configuration,
    "[build]\nbuild-dir = \"build-one\"\njobs = 1\ntarget-dir = \"target-one\"\nwarnings = \"deny\"\n\n[net]\noffline = true\nretry = 2\n\n[registries.crates-io]\nprotocol = \"sparse\"\n",
  )?;
  let warm = run_direct_native_build(&workspace.path, local_cache.path())?;
  let warm_event = native_unit_event(&warm, "wrapper_app")?;
  assert_eq!(
    warm_event["outcome"],
    "hit",
    "unexpected warm event:\ncold: {cold_event}\nwarm: {warm_event}\nstderr:\n{}",
    String::from_utf8_lossy(&warm.stderr)
  );
  assert_eq!(warm_event["unit_identity"], cold_action);

  for directory in ["build-one", "target-one"] {
    let path = workspace.path.join(directory);
    if path.exists() {
      fs::remove_dir_all(path)?;
    }
  }
  fs::write(
    &configuration,
    "[build]\nbuild-dir = \"build-two\"\njobs = 1\ntarget-dir = \"target-two\"\nwarnings = \"deny\"\n\n[net]\noffline = true\nretry = 2\n\n[registries.crates-io]\nprotocol = \"sparse\"\n",
  )?;
  let rebound = run_direct_native_build(&workspace.path, local_cache.path())?;
  let rebound_event = native_unit_event(&rebound, "wrapper_app")?;
  assert_eq!(
    rebound_event["outcome"], "hit",
    "changed Cargo output roots did not reuse the root-independent result: {rebound_event}"
  );
  assert_eq!(rebound_event["unit_identity"], cold_action);
  let outputs = rebound_event["unit"]["observed_outputs"]
    .as_array()
    .context("rebound output observations")?;
  assert!(
    outputs.iter().all(|output| output["path"]["path"]
      .as_str()
      .is_some_and(|path| path.starts_with("build-two/"))),
    "cache hit materialized stale output paths: {rebound_event}"
  );
  let rebound_outputs = compiler_output_files(&workspace.path.join("build-two"))?;
  for (_, bytes) in rebound_outputs
    .iter()
    .filter(|(path, _)| path.extension().is_some_and(|extension| extension == "d"))
  {
    assert!(
      !bytes.windows("build-one".len()).any(|window| window == b"build-one")
        && !bytes.windows("target-one".len()).any(|window| window == b"target-one"),
      "materialized dep-info retained the previous Cargo output directory"
    );
  }

  for directory in ["build-two", "target-two"] {
    let path = workspace.path.join(directory);
    if path.exists() {
      fs::remove_dir_all(path)?;
    }
  }
  let forced_cold_cache = tempfile::tempdir()?;
  let forced_cold = run_direct_native_build(&workspace.path, forced_cold_cache.path())?;
  let forced_cold_event = native_unit_event(&forced_cold, "wrapper_app")?;
  assert_eq!(
    forced_cold_event["outcome"], "miss",
    "empty control cache unexpectedly hit: {forced_cold_event}"
  );
  assert_eq!(forced_cold_event["unit_identity"], cold_action);
  assert_eq!(forced_cold_event["result_key"], rebound_event["result_key"]);
  let forced_cold_outputs = compiler_output_files(&workspace.path.join("build-two"))?;
  assert_eq!(
    rebound_outputs, forced_cold_outputs,
    "root-rebound cache hit differed from a forced-cold compilation"
  );
  Ok(())
}

#[test]
fn unrelated_lockfile_resolution_churn_reuses_native_results() -> Result<()> {
  let workspace = wrapper_workspace("native-cache-unrelated-lockfile-churn")?;
  let unused = workspace.path.join("vendor/unused-dep");
  fs::create_dir_all(unused.join("src"))?;
  fs::write(
    unused.join("Cargo.toml"),
    "[package]\nname = \"unused-dep\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
  )?;
  fs::write(unused.join("src/lib.rs"), "pub fn unused() {}\n")?;
  let manifest = workspace.path.join("crates/wrapper-app/Cargo.toml");
  let mut contents = fs::read_to_string(&manifest)?;
  contents.push_str("unused-dep = { path = \"../../vendor/unused-dep\", optional = true }\n");
  fs::write(&manifest, contents)?;
  let cargo_home = tempfile::tempdir()?;
  generate_front_door_lockfile(&workspace.path, cargo_home.path())?;
  let local_cache = tempfile::tempdir()?;

  let cold = run_direct_native_build(&workspace.path, local_cache.path())?;
  let cold_event = native_unit_event(&cold, "wrapper_app")?;
  assert_eq!(cold_event["outcome"], "miss", "unexpected cold event: {cold_event}");
  let cold_action = cold_event["unit_identity"]
    .as_str()
    .context("lockfile cold action key")?
    .to_string();
  let original_lockfile = fs::read(workspace.path.join("Cargo.lock"))?;
  fs::remove_dir_all(workspace.path.join("target"))?;

  fs::write(
    unused.join("Cargo.toml"),
    "[package]\nname = \"unused-dep\"\nversion = \"0.2.0\"\nedition = \"2021\"\n",
  )?;
  generate_front_door_lockfile(&workspace.path, cargo_home.path())?;
  assert_ne!(fs::read(workspace.path.join("Cargo.lock"))?, original_lockfile);
  let warm = run_direct_native_build(&workspace.path, local_cache.path())?;
  let warm_event = native_unit_event(&warm, "wrapper_app")?;
  assert_eq!(warm_event["outcome"], "hit", "unexpected warm event: {warm_event}");
  assert_eq!(warm_event["unit_identity"], cold_action);
  Ok(())
}

#[cfg(unix)]
#[test]
fn configured_sccache_is_preserved_outside_the_diagnostic_wrapper() -> Result<()> {
  let workspace = wrapper_workspace("sccache-wrapper-coexistence")?;
  let tools = workspace.path.join("tools");
  fs::create_dir_all(&tools)?;
  let sccache = tools.join("sccache");
  let workspace_wrapper = tools.join("workspace-wrapper");
  write_executable(
    &sccache,
    r#"#!/bin/sh
printf '%s\t%s\n' "$1" "$2" >> "$WRAPPER_LOG"
exec "$@"
"#,
  )?;
  write_executable(
    &workspace_wrapper,
    r#"#!/bin/sh
printf '%s\n' "$1" >> "$WORKSPACE_WRAPPER_LOG"
exec "$@"
"#,
  )?;
  workspace.commit("Add sccache-compatible wrapper")?;
  let wrapper_log = workspace.path.join("target/sccache-wrapper.log");
  let workspace_wrapper_log = workspace.path.join("target/workspace-wrapper.log");

  let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(&workspace.path)
    .args(["rail", "unify", "--check"])
    .env("RUSTC_WRAPPER", &sccache)
    .env("RUSTC_WORKSPACE_WRAPPER", &workspace_wrapper)
    .env_remove(CACHE_WRAPPER_MARKER)
    .env(
      "CARGO_RAIL_CACHE_DIR",
      workspace.path.join("target/cargo-rail-test-cache"),
    )
    .env("WRAPPER_LOG", &wrapper_log)
    .env("WORKSPACE_WRAPPER_LOG", &workspace_wrapper_log)
    .output()?;
  assert_eq!(output.status.code(), Some(1), "unexpected unify result: {output:?}");

  let cache = compiler_evidence_cache(&workspace.path)?;
  let observation = local_observation(&cache, "wrapper_app")?;
  assert_eq!(observation["execution"]["cache_wrapper"]["status"], "bypassed");
  assert_eq!(
    observation["execution"]["cache_wrapper"]["reason"],
    "sccache_wrapper_preserved"
  );
  let roles = observation["execution"]["wrappers"]
    .as_array()
    .context("wrapper chain")?
    .iter()
    .map(|wrapper| wrapper["role"].as_str())
    .collect::<Vec<_>>();
  assert_eq!(
    roles,
    [
      Some("cargo_global"),
      Some("cargo_rail_diagnostic"),
      Some("cargo_workspace")
    ]
  );

  let cargo_rail = Path::new(env!("CARGO_BIN_EXE_cargo-rail"));
  let preserved_workspace_call = fs::read_to_string(&wrapper_log)?.lines().any(|line| {
    line.split_once('\t').is_some_and(|(first, second)| {
      Path::new(first) == cargo_rail && Path::new(second).file_stem().is_some_and(|name| name == "rustc")
    })
  });
  assert!(
    preserved_workspace_call,
    "sccache must receive cargo-rail's diagnostic wrapper followed by rustc, without a second cache wrapper"
  );
  assert!(
    fs::read_to_string(&workspace_wrapper_log)?
      .lines()
      .any(|program| Path::new(program).file_stem().is_some_and(|name| name == "rustc")),
    "the existing workspace wrapper must receive rustc after cargo-rail's diagnostic wrapper"
  );
  Ok(())
}

#[cfg(unix)]
#[test]
fn direct_native_cache_preserves_existing_wrapper_compilation_argv_and_outputs() -> Result<()> {
  fn compilation_invocations(log: &Path) -> Result<Vec<Vec<String>>> {
    let log = fs::read_to_string(log)?;
    Ok(
      log
        .split("BEGIN\n")
        .filter_map(|record| record.split_once("END\n").map(|(arguments, _)| arguments))
        .map(|arguments| arguments.lines().map(str::to_string).collect::<Vec<_>>())
        .filter(|arguments| {
          arguments
            .iter()
            .any(|argument| argument == "--emit" || argument.starts_with("--emit="))
        })
        .collect(),
    )
  }

  let workspace = wrapper_workspace("direct-native-cache-existing-wrapper")?;
  let tools = workspace.path.join("tools");
  fs::create_dir_all(&tools)?;
  let target = workspace.path.join("target");
  let cache = tempfile::tempdir()?;

  for (wrapper_name, reason) in [
    ("custom-wrapper", "existing_compiler_wrapper_preserved"),
    ("sccache", "sccache_wrapper_preserved"),
  ] {
    let wrapper = tools.join(wrapper_name);
    let log = cache.path().join(format!("{wrapper_name}.log"));
    let environment_log = cache.path().join(format!("{wrapper_name}-environment.log"));
    write_executable(
      &wrapper,
      r#"#!/bin/sh
{
  printf 'BEGIN\n'
  compilation=false
  for argument in "$@"; do
    printf '%s\n' "$argument"
    case "$argument" in
      --emit | --emit=*) compilation=true ;;
    esac
  done
  printf 'END\n'
} >> "$WRAPPER_LOG"
if [ "$compilation" = true ]; then
  if [ "${CARGO_RAIL_CACHE_DIR+x}" = x ]; then
    printf 'leaked\n' >> "$WRAPPER_ENV_LOG"
  else
    printf 'clean\n' >> "$WRAPPER_ENV_LOG"
  fi
fi
exec "$@"
"#,
    )?;
    fs::write(&log, "")?;
    fs::write(&environment_log, "")?;
    if target.exists() {
      fs::remove_dir_all(&target)?;
    }
    let direct = Command::new("cargo")
      .current_dir(&workspace.path)
      .args(["check", "--workspace", "--quiet", "--target-dir"])
      .arg(&target)
      .env("RUSTC_WRAPPER", &wrapper)
      .env("WRAPPER_LOG", &log)
      .env("WRAPPER_ENV_LOG", &environment_log)
      .env("CARGO_BUILD_JOBS", "1")
      .env("CARGO_INCREMENTAL", "0")
      .env_remove("CARGO_RAIL_CACHE_DIR")
      .env_remove("RUSTC_WORKSPACE_WRAPPER")
      .output()?;
    assert!(direct.status.success(), "direct Cargo failed: {direct:?}");
    let direct_invocations = compilation_invocations(&log)?;
    let direct_outputs = compiler_output_files(&target)?;
    assert!(!direct_invocations.is_empty());
    assert!(!direct_outputs.is_empty());

    fs::remove_dir_all(&target)?;
    fs::write(&log, "")?;
    fs::write(&environment_log, "")?;
    let wrapped = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
      .current_dir(&workspace.path)
      .args([
        "rail",
        "run",
        "--all",
        "--action",
        "build",
        "--explain",
        "--",
        "--quiet",
        "--target-dir",
      ])
      .arg(&target)
      .env("RUSTC_WRAPPER", &wrapper)
      .env("WRAPPER_LOG", &log)
      .env("WRAPPER_ENV_LOG", &environment_log)
      .env("CARGO_BUILD_JOBS", "1")
      .env("CARGO_INCREMENTAL", "0")
      .env("CARGO_RAIL_CACHE_DIR", cache.path())
      .env_remove("RUSTC_WORKSPACE_WRAPPER")
      .output()?;
    assert!(wrapped.status.success(), "cargo-rail Cargo action failed: {wrapped:?}");
    assert!(
      String::from_utf8_lossy(&wrapped.stdout).contains(&format!("native compiler cache: bypassed ({reason})")),
      "wrapper bypass was not explicit: {}",
      String::from_utf8_lossy(&wrapped.stdout)
    );
    assert_eq!(
      compilation_invocations(&log)?,
      direct_invocations,
      "cargo-rail changed compiler argv for {wrapper_name}"
    );
    assert_eq!(
      compiler_output_files(&target)?,
      direct_outputs,
      "cargo-rail changed compiler outputs for {wrapper_name}"
    );
    let environment_observations = fs::read_to_string(&environment_log)?;
    assert!(
      !environment_observations.is_empty()
        && environment_observations
          .lines()
          .all(|observation| observation == "clean"),
      "cargo-rail private controls reached {wrapper_name}: {environment_observations}"
    );
  }
  Ok(())
}

#[test]
fn recursive_cargo_rail_wrapper_configuration_is_rejected_before_cargo() -> Result<()> {
  let workspace = wrapper_workspace("recursive-cargo-rail-wrapper")?;
  let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(&workspace.path)
    .args(["rail", "unify", "--check"])
    .env("RUSTC_WRAPPER", env!("CARGO_BIN_EXE_cargo-rail"))
    .env(
      "CARGO_RAIL_CACHE_DIR",
      workspace.path.join("target/cargo-rail-test-cache"),
    )
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .env_remove(CACHE_WRAPPER_MARKER)
    .output()?;
  assert_eq!(output.status.code(), Some(2));
  assert!(
    String::from_utf8_lossy(&output.stderr).contains("recursive cargo-rail rustc wrapper configuration"),
    "recursive configuration must fail explicitly: {output:?}"
  );
  assert!(
    !workspace
      .path
      .join("target/cargo-rail/cache/compiler-diags-v1.json")
      .exists()
  );
  Ok(())
}

#[cfg(unix)]
#[test]
fn bare_name_recursive_cargo_rail_wrapper_is_rejected_explicitly() -> Result<()> {
  use std::os::unix::fs::symlink;

  let workspace = wrapper_workspace("bare-recursive-cargo-rail-wrapper")?;
  let tools = workspace.path.join("tools");
  fs::create_dir_all(&tools)?;
  symlink(env!("CARGO_BIN_EXE_cargo-rail"), tools.join("cargo-rail"))?;
  let inherited_path = std::env::var_os("PATH").unwrap_or_default();
  let search_path = std::env::join_paths(std::iter::once(tools).chain(std::env::split_paths(&inherited_path)))?;

  let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(&workspace.path)
    .args(["rail", "unify", "--check"])
    .env("PATH", search_path)
    .env("RUSTC_WRAPPER", "cargo-rail")
    .env(
      "CARGO_RAIL_CACHE_DIR",
      workspace.path.join("target/cargo-rail-test-cache"),
    )
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .env_remove(CACHE_WRAPPER_MARKER)
    .output()?;
  assert_eq!(output.status.code(), Some(2));
  assert!(
    String::from_utf8_lossy(&output.stderr).contains("recursive cargo-rail rustc wrapper configuration"),
    "bare-name recursion must fail explicitly: {output:?}"
  );
  assert!(
    !workspace
      .path
      .join("target/cargo-rail/cache/compiler-diags-v1.json")
      .exists()
  );
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

  let output = Command::new("cargo")
    .current_dir(&workspace.path)
    .args(["doc", "--no-deps", "--message-format=json", "--target-dir"])
    .arg(&target_directory)
    .env("RUSTDOC", env!("CARGO_BIN_EXE_cargo-rail"))
    .env("CARGO_RAIL_RUSTDOC_WRAPPER", "1")
    .env("CARGO_RAIL_INNER_RUSTDOC", "rustdoc")
    .env("CARGO_RAIL_COMPILER_OBSERVATION_DIRECTORY", &observation_directory)
    .env("CARGO_RAIL_COMPILER_OBSERVATION_SOURCE_ROOT", &workspace.path)
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
