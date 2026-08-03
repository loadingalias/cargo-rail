use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};
use sha2::{Digest as _, Sha256};

#[cfg(windows)]
fn git_bash() -> Result<PathBuf> {
  let output = Command::new("git")
    .arg("--exec-path")
    .output()
    .context("resolve Git installation for native-cache fixture")?;
  ensure!(
    output.status.success(),
    "git --exec-path failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  let exec_path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
  exec_path
    .ancestors()
    .map(|ancestor| ancestor.join("bin/bash.exe"))
    .find(|candidate| candidate.is_file())
    .with_context(|| format!("Git Bash was not found above {}", exec_path.display()))
}

fn materialize_fixture(destination: &Path, git_source: &Path) -> Result<()> {
  let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/fixtures/materialize-native-cache.sh");
  #[cfg(windows)]
  let mut command = {
    let mut command = Command::new(git_bash()?);
    command.arg(script);
    command
  };
  #[cfg(not(windows))]
  let mut command = Command::new(script);
  let output = command
    .arg(destination)
    .arg(git_source)
    .output()
    .context("materialize native-cache fixture")?;
  ensure!(
    output.status.success(),
    "fixture materialization failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  Ok(())
}

fn run_cargo_rail(fixture: &Path, action: &str, cache: &Path) -> Result<String> {
  run_cargo_rail_with_options(fixture, action, cache, true, &[])
}

fn run_cargo_rail_with_environment(
  fixture: &Path,
  action: &str,
  cache: &Path,
  environment: &[(&str, &str)],
) -> Result<String> {
  run_cargo_rail_with_options(fixture, action, cache, true, environment)
}

fn run_cargo_rail_with_options(
  fixture: &Path,
  action: &str,
  cache: &Path,
  all_features: bool,
  environment: &[(&str, &str)],
) -> Result<String> {
  let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-rail"));
  command
    .current_dir(fixture)
    .args(["rail", "run", "--all", "--action", action, "--explain", "--"]);
  if all_features {
    command.arg("--all-features");
  }
  command.arg("--offline");
  if action == "build" {
    command.arg("--locked");
  }
  command
    .env("CARGO_RAIL_CACHE_DIR", cache)
    .env("CARGO_INCREMENTAL", "0")
    .env_remove("RUSTC_WRAPPER")
    .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER");
  for (name, value) in environment {
    command.env(name, value);
  }
  let output = command.output()?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  ensure!(
    output.status.success(),
    "cargo-rail {action} failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
  );
  Ok(format!("{stdout}\n{stderr}"))
}

fn cache_metric(output: &str, name: &str) -> Result<u64> {
  let marker = format!("{name}=");
  output
    .lines()
    .find(|line| line.contains("native compiler cache:"))
    .and_then(|line| {
      line
        .split_ascii_whitespace()
        .find_map(|field| field.strip_prefix(&marker))
    })
    .and_then(|value| value.trim_end_matches(',').parse().ok())
    .with_context(|| format!("missing native-cache metric '{name}' in:\n{output}"))
}

fn reusable_cache_units(output: &str) -> Result<BTreeMap<String, serde_json::Value>> {
  let mut units = BTreeMap::new();
  for event in output.lines().filter_map(|line| {
    line
      .split_once(" native compiler cache event: ")
      .map(|(_, event)| event)
  }) {
    let event = serde_json::from_str::<serde_json::Value>(event)?;
    if !event["action_key"].is_string() || !matches!(event["outcome"].as_str(), Some("hit" | "miss")) {
      continue;
    }
    ensure!(event["schema_version"] == 4, "unexpected native-cache event: {event}");
    ensure!(
      event["unit"].is_object(),
      "native-cache event lacks unit evidence: {event}"
    );
    let identity = event["unit_identity"]
      .as_str()
      .context("native-cache event lacks unit identity")?
      .to_string();
    ensure!(
      units.insert(identity.clone(), event).is_none(),
      "duplicate native-cache unit identity: {identity}"
    );
  }
  Ok(units)
}

fn reusable_output_digests(root: &Path, target: &Path, report: &str) -> Result<BTreeMap<PathBuf, String>> {
  let mut outputs = BTreeMap::new();
  for event in reusable_cache_units(report)?.into_values() {
    let paths = event["unit"]["output_paths"]
      .as_array()
      .into_iter()
      .flatten()
      .filter_map(|path| path["path"].as_str())
      .chain(
        event["unit"]["observed_outputs"]
          .as_array()
          .into_iter()
          .flatten()
          .filter_map(|output| output["path"]["path"].as_str()),
      )
      .map(PathBuf::from)
      .collect::<BTreeSet<_>>();
    for path in paths {
      let physical = root.join(&path);
      if !physical.is_file() {
        continue;
      }
      let relative = physical
        .strip_prefix(target)
        .with_context(|| format!("native-cache output escaped target directory: {}", physical.display()))?
        .to_path_buf();
      let bytes = fs::read(&physical).with_context(|| format!("read native-cache output {}", physical.display()))?;
      let digest = Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect();
      ensure!(
        outputs.insert(relative.clone(), digest).is_none(),
        "duplicate native-cache output: {}",
        relative.display()
      );
    }
  }
  Ok(outputs)
}

fn output_difference(
  expected: &BTreeMap<PathBuf, String>,
  actual: &BTreeMap<PathBuf, String>,
) -> (Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>) {
  let missing = expected
    .keys()
    .filter(|path| !actual.contains_key(*path))
    .cloned()
    .collect();
  let unexpected = actual
    .keys()
    .filter(|path| !expected.contains_key(*path))
    .cloned()
    .collect();
  let changed = expected
    .iter()
    .filter(|(path, digest)| actual.get(*path).is_some_and(|actual| actual != *digest))
    .map(|(path, _)| path.clone())
    .collect();
  (missing, unexpected, changed)
}

fn output_digests_at<'a>(target: &Path, paths: impl Iterator<Item = &'a PathBuf>) -> Result<BTreeMap<PathBuf, String>> {
  paths
    .filter_map(|path| {
      let physical = target.join(path);
      physical.is_file().then_some((path, physical))
    })
    .map(|(path, physical)| {
      let bytes = fs::read(&physical).with_context(|| format!("read native-cache output {}", physical.display()))?;
      let digest = Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect();
      Ok((path.clone(), digest))
    })
    .collect()
}

fn run_cargo(fixture: &Path, arguments: &[&str], target: &Path) -> Result<()> {
  let output = Command::new("cargo")
    .current_dir(fixture)
    .args(arguments)
    .args(["--target-dir"])
    .arg(target)
    .env("CARGO_INCREMENTAL", "0")
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .output()?;
  ensure!(
    output.status.success(),
    "cargo {} failed:\nstdout:\n{}\nstderr:\n{}",
    arguments.join(" "),
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  Ok(())
}

fn executable(path: PathBuf) -> PathBuf {
  if cfg!(windows) {
    path.with_extension("exe")
  } else {
    path
  }
}

#[test]
fn real_cargo_check_and_build_reuse_exact_outputs_within_clean_roots() -> Result<()> {
  let root = tempfile::tempdir()?;
  let first = root.path().join("first");
  let second = root.path().join("second");
  let git_source = root.path().join("git-source");
  let check_cache = root.path().join("check-cache");
  materialize_fixture(&first, &git_source)?;
  materialize_fixture(&second, &git_source)?;

  let first_cold = run_cargo_rail(&first, "build", &check_cache)?;
  ensure!(cache_metric(&first_cold, "hits")? == 0, "{first_cold}");
  ensure!(cache_metric(&first_cold, "misses")? >= 12, "{first_cold}");
  ensure!(cache_metric(&first_cold, "cache_bytes_written")? > 0, "{first_cold}");
  let first_units = reusable_cache_units(&first_cold)?;
  let first_outputs = reusable_output_digests(&first, &first.join("target"), &first_cold)?;

  let second_cold = run_cargo_rail(&second, "build", &check_cache)?;
  ensure!(cache_metric(&second_cold, "hits")? == 0, "{second_cold}");
  ensure!(cache_metric(&second_cold, "misses")? >= 12, "{second_cold}");
  ensure!(cache_metric(&second_cold, "cache_bytes_written")? > 0, "{second_cold}");
  let second_units = reusable_cache_units(&second_cold)?;
  let overlapping_units = first_units
    .keys()
    .filter(|identity| second_units.contains_key(*identity))
    .collect::<Vec<_>>();
  ensure!(
    overlapping_units.is_empty(),
    "opaque compiler outputs reused source-root-independent identities: {overlapping_units:#?}"
  );
  let second_outputs = reusable_output_digests(&second, &second.join("target"), &second_cold)?;
  ensure!(
    first_outputs != second_outputs,
    "the fixture must expose rustc's physical source-root binding"
  );

  fs::remove_dir_all(second.join("target"))?;
  let second_warm = run_cargo_rail(&second, "build", &check_cache)?;
  let check_hits = cache_metric(&second_warm, "hits")?;
  ensure!(
    check_hits == cache_metric(&second_cold, "hits")? + cache_metric(&second_cold, "misses")?,
    "warm check must restore every cold cache publication:\ncold:\n{second_cold}\nwarm:\n{second_warm}"
  );
  ensure!(
    cache_metric(&second_warm, "bypasses")? == cache_metric(&second_cold, "bypasses")?,
    "warm check changed the cold bypass set:\ncold:\n{second_cold}\nwarm:\n{second_warm}"
  );
  ensure!(cache_metric(&second_warm, "misses")? == 0, "{second_warm}");
  let check_bytes_restored = cache_metric(&second_warm, "bytes_restored")?;
  ensure!(check_bytes_restored > 0, "{second_warm}");
  ensure!(
    cache_metric(&second_warm, "cache_bytes_read")? >= check_bytes_restored,
    "{second_warm}"
  );
  ensure!(cache_metric(&second_warm, "cache_bytes_written")? == 0, "{second_warm}");
  let warm_outputs = output_digests_at(&second.join("target"), second_outputs.keys())?;
  ensure!(
    warm_outputs == second_outputs,
    "verified check hits changed exact cold compiler output bytes: {:?}",
    output_difference(&second_outputs, &warm_outputs)
  );
  for reason in [
    "build_script_not_graduated",
    "proc_macro_not_graduated",
    "native_linking_not_graduated",
    "binary_not_graduated",
  ] {
    ensure!(
      second_warm.contains(reason),
      "missing bypass '{reason}':\n{second_warm}"
    );
  }

  fs::remove_dir_all(second.join("target"))?;
  let sdk_root = if cfg!(target_os = "macos") {
    let output = Command::new("xcrun").arg("--show-sdk-path").output()?;
    ensure!(output.status.success(), "xcrun did not resolve the active SDK");
    format!(
      "{}/.",
      String::from_utf8_lossy(&output.stdout).trim_end_matches(['\r', '\n', '/'])
    )
  } else {
    "/".to_string()
  };
  let sdk_mutation =
    run_cargo_rail_with_environment(&second, "build", &check_cache, &[("SDKROOT", sdk_root.as_str())])?;
  ensure!(cache_metric(&sdk_mutation, "hits")? == 0, "{sdk_mutation}");

  fs::remove_dir_all(second.join("target"))?;
  let linker_mutation = run_cargo_rail_with_environment(
    &second,
    "build",
    &check_cache,
    &[("LD", "/cargo-rail/not-used-by-graduated-library-units")],
  )?;
  ensure!(cache_metric(&linker_mutation, "hits")? == 0, "{linker_mutation}");

  fs::remove_dir_all(first.join("target"))?;
  let release_default = run_cargo_rail_with_options(&first, "distribution", &check_cache, false, &[])?;
  ensure!(cache_metric(&release_default, "misses")? >= 8, "{release_default}");
  ensure!(
    cache_metric(&release_default, "cache_bytes_written")? > 0,
    "{release_default}"
  );
  let default_binary = executable(first.join("target/release/fixture-cli"));
  let default_output = Command::new(&default_binary).output()?;
  ensure!(default_output.status.success());
  ensure!(String::from_utf8_lossy(&default_output.stdout).trim() == "101");

  fs::remove_dir_all(first.join("target"))?;
  let feature_cold = run_cargo_rail(&first, "distribution", &check_cache)?;
  ensure!(cache_metric(&feature_cold, "misses")? >= 8, "{feature_cold}");
  let feature_binary = executable(first.join("target/release/fixture-cli"));
  let feature_output = Command::new(&feature_binary).output()?;
  ensure!(feature_output.status.success());
  ensure!(String::from_utf8_lossy(&feature_output.stdout).trim() == "119");
  let feature_outputs = reusable_output_digests(&first, &first.join("target/release"), &feature_cold)?;

  fs::remove_dir_all(first.join("target"))?;
  let feature_warm = run_cargo_rail(&first, "distribution", &check_cache)?;
  let build_hits = cache_metric(&feature_warm, "hits")?;
  ensure!(
    build_hits == cache_metric(&feature_cold, "hits")? + cache_metric(&feature_cold, "misses")?,
    "warm build must restore every cold cache publication:\ncold:\n{feature_cold}\nwarm:\n{feature_warm}"
  );
  ensure!(
    cache_metric(&feature_warm, "bypasses")? == cache_metric(&feature_cold, "bypasses")?,
    "warm build changed the cold bypass set:\ncold:\n{feature_cold}\nwarm:\n{feature_warm}"
  );
  let build_bytes_restored = cache_metric(&feature_warm, "bytes_restored")?;
  ensure!(build_bytes_restored > 0, "{feature_warm}");
  ensure!(
    cache_metric(&feature_warm, "cache_bytes_read")? >= build_bytes_restored,
    "{feature_warm}"
  );
  ensure!(
    cache_metric(&feature_warm, "cache_bytes_written")? == 0,
    "{feature_warm}"
  );
  let warm_outputs = output_digests_at(&first.join("target/release"), feature_outputs.keys())?;
  ensure!(
    warm_outputs == feature_outputs,
    "verified build hits changed exact cold compiler output bytes: {:?}",
    output_difference(&feature_outputs, &warm_outputs)
  );

  let binary = executable(first.join("target/release/fixture-cli"));
  ensure!(binary.is_file(), "linked fixture binary was not produced");
  let output = Command::new(binary).output()?;
  ensure!(output.status.success());
  ensure!(String::from_utf8_lossy(&output.stdout).trim() == "119");
  Ok(())
}

#[test]
fn real_world_native_cache_fixture_exercises_required_compiler_classes() -> Result<()> {
  let root = tempfile::tempdir()?;
  let fixture = root.path().join("fixture");
  let target = fixture.join("target");
  materialize_fixture(&fixture, &root.path().join("git-source"))?;

  let metadata = Command::new("cargo")
    .current_dir(&fixture)
    .args(["metadata", "--locked", "--offline", "--format-version=1"])
    .output()?;
  ensure!(metadata.status.success(), "fixture metadata failed");
  let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout)?;
  let packages = metadata["packages"].as_array().context("fixture packages")?;
  ensure!(
    metadata["workspace_members"]
      .as_array()
      .context("workspace members")?
      .len()
      >= 10
  );
  ensure!(packages.iter().any(|package| {
    package["source"]
      .as_str()
      .is_some_and(|source| source.starts_with("registry+"))
  }));
  ensure!(packages.iter().any(|package| {
    package["source"]
      .as_str()
      .is_some_and(|source| source.starts_with("git+file:"))
  }));
  ensure!(
    packages
      .iter()
      .flat_map(|package| package["targets"].as_array().into_iter().flatten())
      .any(|target| {
        target["kind"]
          .as_array()
          .is_some_and(|kinds| kinds.iter().any(|kind| kind == "custom-build"))
      })
  );
  ensure!(
    packages
      .iter()
      .flat_map(|package| package["targets"].as_array().into_iter().flatten())
      .any(|target| {
        target["kind"]
          .as_array()
          .is_some_and(|kinds| kinds.iter().any(|kind| kind == "proc-macro"))
      })
  );

  run_cargo(
    &fixture,
    &[
      "check",
      "--workspace",
      "--all-targets",
      "--all-features",
      "--locked",
      "--offline",
    ],
    &target,
  )?;
  run_cargo(
    &fixture,
    &["build", "--workspace", "--all-features", "--locked", "--offline"],
    &target,
  )?;
  ensure!(executable(target.join("debug/fixture-cli")).is_file());
  Ok(())
}
