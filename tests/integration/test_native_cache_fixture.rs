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
    if !event["action_key"].is_string() {
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

struct CompilerOutputEvidence {
  portable: BTreeMap<PathBuf, String>,
  root_bound: BTreeSet<PathBuf>,
}

fn compiler_output_evidence(root: &Path, target: &Path, report: &str) -> Result<CompilerOutputEvidence> {
  let compiler_outputs = event_output_paths(report, None)?;
  let canonical = fs::canonicalize(root)?;
  let mut root_spellings = [root, canonical.as_path()]
    .into_iter()
    .flat_map(|root| {
      let native = root.as_os_str().as_encoded_bytes().to_vec();
      #[cfg(windows)]
      {
        let forward = String::from_utf8_lossy(&native).replace('\\', "/").into_bytes();
        return vec![native, forward];
      }
      #[cfg(not(windows))]
      {
        vec![native]
      }
    })
    .filter(|root| !root.is_empty())
    .collect::<Vec<_>>();
  root_spellings.sort();
  root_spellings.dedup();
  let mut portable = BTreeMap::new();
  let mut root_bound = BTreeSet::new();
  let mut pending = vec![target.to_path_buf()];
  while let Some(directory) = pending.pop() {
    let Ok(entries) = fs::read_dir(&directory) else {
      continue;
    };
    for entry in entries {
      let path = entry?.path();
      let metadata = fs::symlink_metadata(&path)?;
      if metadata.is_dir() && !metadata.file_type().is_symlink() {
        pending.push(path);
        continue;
      }
      if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || !matches!(
          path.extension().and_then(|extension| extension.to_str()),
          Some("d" | "rmeta" | "rlib")
        )
      {
        continue;
      }
      let repository_path = path.strip_prefix(root)?.to_path_buf();
      if !compiler_outputs.contains(&repository_path) {
        continue;
      }
      let bytes = fs::read(&path)?;
      let contains = |needle: &[u8]| !needle.is_empty() && bytes.windows(needle.len()).any(|window| window == needle);
      if root_spellings.iter().any(|root| contains(root)) {
        root_bound.insert(repository_path);
        continue;
      }
      portable.insert(
        path.strip_prefix(target)?.to_path_buf(),
        Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect(),
      );
    }
  }
  Ok(CompilerOutputEvidence { portable, root_bound })
}

fn event_output_paths(output: &str, outcome: Option<&str>) -> Result<BTreeSet<PathBuf>> {
  let mut paths = BTreeSet::new();
  for event in output.lines().filter_map(|line| {
    line
      .split_once(" native compiler cache event: ")
      .map(|(_, event)| event)
  }) {
    let event = serde_json::from_str::<serde_json::Value>(event)?;
    if outcome.is_some_and(|outcome| event["outcome"] != outcome) {
      continue;
    }
    for expected in event["unit"]["output_paths"].as_array().into_iter().flatten() {
      if expected["root"] == "repository"
        && let Some(path) = expected["path"].as_str()
      {
        paths.insert(PathBuf::from(path));
      }
    }
    for observed in event["unit"]["observed_outputs"].as_array().into_iter().flatten() {
      if observed["path"]["root"] == "repository"
        && let Some(path) = observed["path"]["path"].as_str()
      {
        paths.insert(PathBuf::from(path));
      }
    }
  }
  Ok(paths)
}

fn verify_root_bound_outputs_are_bypassed(evidence: &CompilerOutputEvidence, report: &str, label: &str) -> Result<()> {
  let bypassed = event_output_paths(report, Some("bypassed"))?;
  let unaccounted = evidence.root_bound.difference(&bypassed).collect::<Vec<_>>();
  ensure!(
    unaccounted.is_empty(),
    "{label} retained physical workspace roots in outputs not owned by an explicit bypass: {unaccounted:?}"
  );
  Ok(())
}

fn output_difference(
  first: &BTreeMap<PathBuf, String>,
  second: &BTreeMap<PathBuf, String>,
) -> (Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>) {
  let first_only = first
    .keys()
    .filter(|path| !second.contains_key(*path))
    .cloned()
    .collect();
  let second_only = second
    .keys()
    .filter(|path| !first.contains_key(*path))
    .cloned()
    .collect();
  let changed = first
    .iter()
    .filter_map(|(path, digest)| (second.get(path).is_some_and(|other| other != digest)).then_some(path.clone()))
    .collect();
  (first_only, second_only, changed)
}

fn verify_portable_hit_outputs(
  first: &BTreeMap<PathBuf, String>,
  second: &BTreeMap<PathBuf, String>,
  hits: u64,
  label: &str,
) -> Result<()> {
  let difference = output_difference(first, second);
  ensure!(
    difference.0.is_empty() && difference.1.is_empty(),
    "{label} output sets differ: {difference:?}"
  );
  let matching = first.len().saturating_sub(difference.2.len());
  ensure!(
    matching >= hits as usize * 2,
    "{label} has {hits} hits but only {matching} byte-identical portable outputs: {difference:?}"
  );
  Ok(())
}

#[cfg(target_os = "macos")]
fn unique_dependency_output(target: &Path, prefix: &str, extensions: &[&str]) -> Result<Vec<u8>> {
  let dependency_directory = target.join("debug/deps");
  let matches = fs::read_dir(&dependency_directory)?
    .filter_map(|entry| entry.ok().map(|entry| entry.path()))
    .filter(|path| {
      path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(prefix))
        && path
          .extension()
          .and_then(|extension| extension.to_str())
          .is_some_and(|extension| extensions.contains(&extension))
    })
    .collect::<Vec<_>>();
  ensure!(
    matches.len() == 1,
    "expected one {prefix} dependency output in {}, found {matches:?}",
    dependency_directory.display()
  );
  fs::read(&matches[0]).with_context(|| format!("reading portable dependency output {}", matches[0].display()))
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

#[test]
fn real_cargo_check_and_build_reuse_verified_outputs_across_clean_roots() -> Result<()> {
  let root = tempfile::tempdir()?;
  let first = root.path().join("first");
  let second = root.path().join("second");
  let git_source = root.path().join("git-source");
  let check_cache = root.path().join("check-cache");
  materialize_fixture(&first, &git_source)?;
  materialize_fixture(&second, &git_source)?;

  let cold_check = run_cargo_rail(&first, "build", &check_cache)?;
  let warm_check = run_cargo_rail(&second, "build", &check_cache)?;
  ensure!(cache_metric(&cold_check, "hits")? == 0, "{cold_check}");
  ensure!(cache_metric(&cold_check, "misses")? >= 12, "{cold_check}");
  ensure!(cache_metric(&cold_check, "cache_bytes_written")? > 0, "{cold_check}");
  let cold_units = reusable_cache_units(&cold_check)?;
  let warm_units = reusable_cache_units(&warm_check)?;
  let removed_units = cold_units
    .iter()
    .filter(|(identity, _)| !warm_units.contains_key(*identity))
    .collect::<Vec<_>>();
  let added_units = warm_units
    .iter()
    .filter(|(identity, _)| !cold_units.contains_key(*identity))
    .collect::<Vec<_>>();
  ensure!(
    removed_units.is_empty() && added_units.is_empty(),
    "native cache unit identities changed across clean roots:\nremoved={removed_units:#?}\nadded={added_units:#?}"
  );
  let changed_units = cold_units
    .iter()
    .filter_map(|(identity, cold)| {
      let warm = warm_units.get(identity)?;
      ((&cold["action_key"], &cold["unit"]) != (&warm["action_key"], &warm["unit"])).then_some((identity, cold, warm))
    })
    .collect::<Vec<_>>();
  ensure!(
    changed_units.is_empty(),
    "native cache unit inputs or claimed outputs changed across clean roots:\n{changed_units:#?}"
  );
  let check_hits = cache_metric(&warm_check, "hits")?;
  ensure!(check_hits >= 12, "{warm_check}");
  ensure!(cache_metric(&warm_check, "misses")? == 0, "{warm_check}");
  let check_bytes_restored = cache_metric(&warm_check, "bytes_restored")?;
  ensure!(check_bytes_restored > 0, "{warm_check}");
  ensure!(
    cache_metric(&warm_check, "cache_bytes_read")? >= check_bytes_restored,
    "{warm_check}"
  );
  ensure!(cache_metric(&warm_check, "cache_bytes_written")? == 0, "{warm_check}");
  for reason in [
    "build_script_not_graduated",
    "proc_macro_not_graduated",
    "native_linking_not_graduated",
    "binary_not_graduated",
  ] {
    ensure!(warm_check.contains(reason), "missing bypass '{reason}':\n{warm_check}");
  }
  let first_check = compiler_output_evidence(&first, &first.join("target"), &cold_check)?;
  let second_check = compiler_output_evidence(&second, &second.join("target"), &warm_check)?;
  verify_root_bound_outputs_are_bypassed(&first_check, &cold_check, "cold cargo check")?;
  verify_root_bound_outputs_are_bypassed(&second_check, &warm_check, "warm cargo check")?;
  ensure!(first_check.portable.len() >= check_hits as usize * 2);
  verify_portable_hit_outputs(&first_check.portable, &second_check.portable, check_hits, "cargo check")?;
  #[cfg(target_os = "macos")]
  ensure!(
    unique_dependency_output(&first.join("target"), "libserde_derive-", &["dylib", "so"])?
      == unique_dependency_output(&second.join("target"), "libserde_derive-", &["dylib", "so"])?,
    "portable proc-macro execution produced root-bound dylib bytes"
  );
  #[cfg(target_os = "macos")]
  ensure!(
    unique_dependency_output(&first.join("target"), "libserde-", &["rmeta"])?
      == unique_dependency_output(&second.join("target"), "libserde-", &["rmeta"])?,
    "portable proc-macro execution did not stabilize downstream metadata"
  );

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
  fs::remove_dir_all(second.join("target"))?;
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

  let feature_population = run_cargo_rail(&second, "distribution", &check_cache)?;
  ensure!(cache_metric(&feature_population, "misses")? > 0, "{feature_population}");
  let feature_binary = executable(second.join("target/release/fixture-cli"));
  let feature_output = Command::new(&feature_binary).output()?;
  ensure!(feature_output.status.success());
  ensure!(String::from_utf8_lossy(&feature_output.stdout).trim() == "119");

  fs::remove_dir_all(first.join("target"))?;
  let warm_build = run_cargo_rail(&first, "distribution", &check_cache)?;
  let build_hits = cache_metric(&warm_build, "hits")?;
  ensure!(build_hits >= 8, "{warm_build}");
  let build_bytes_restored = cache_metric(&warm_build, "bytes_restored")?;
  ensure!(build_bytes_restored > 0, "{warm_build}");
  ensure!(
    cache_metric(&warm_build, "cache_bytes_read")? >= build_bytes_restored,
    "{warm_build}"
  );
  ensure!(cache_metric(&warm_build, "cache_bytes_written")? == 0, "{warm_build}");

  let binary = executable(first.join("target/release/fixture-cli"));
  ensure!(binary.is_file(), "linked fixture binary was not produced");
  let output = Command::new(binary).output()?;
  ensure!(output.status.success());
  ensure!(String::from_utf8_lossy(&output.stdout).trim() == "119");

  let warm_release = first.join("target/release");
  let populated_release = second.join("target/release");
  let warm_outputs = compiler_output_evidence(&first, &warm_release, &warm_build)?;
  let populated_outputs = compiler_output_evidence(&second, &populated_release, &feature_population)?;
  verify_root_bound_outputs_are_bypassed(&warm_outputs, &warm_build, "warm cargo build")?;
  verify_root_bound_outputs_are_bypassed(&populated_outputs, &feature_population, "cold cargo build")?;
  ensure!(warm_outputs.portable.len() >= build_hits as usize * 2);
  verify_portable_hit_outputs(
    &warm_outputs.portable,
    &populated_outputs.portable,
    build_hits,
    "cargo build",
  )?;
  Ok(())
}
