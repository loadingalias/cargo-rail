//! Retained real-workspace qualification for transparent native compiler reuse.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context as _, Result, ensure};
use sha2::{Digest as _, Sha256};

#[cfg(windows)]
fn git_bash() -> Result<PathBuf> {
  let output = Command::new("git")
    .arg("--exec-path")
    .output()
    .context("resolve Git installation for native-cache fixture")?;
  ensure!(output.status.success(), "git --exec-path failed");
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

fn cargo_metadata(fixture: &Path, cargo_home: Option<&Path>) -> Result<serde_json::Value> {
  let mut command = Command::new("cargo");
  command
    .current_dir(fixture)
    .args([
      "metadata",
      "--locked",
      "--offline",
      "--all-features",
      "--format-version=1",
    ])
    .env_remove("RUSTC_WRAPPER")
    .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER");
  if let Some(cargo_home) = cargo_home {
    command.env("CARGO_HOME", cargo_home);
  }
  let output = command.output()?;
  ensure!(
    output.status.success(),
    "fixture metadata failed:\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  serde_json::from_slice(&output.stdout).context("decode fixture metadata")
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
  let metadata = fs::symlink_metadata(source)?;
  ensure!(metadata.is_dir(), "fixture cache source is not a directory");
  ensure!(!destination.exists(), "fixture cache destination already exists");
  fs::create_dir_all(destination)?;
  let mut entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
  entries.sort_by_key(std::fs::DirEntry::file_name);
  for entry in entries {
    let source = entry.path();
    let destination = destination.join(entry.file_name());
    let metadata = fs::symlink_metadata(&source)?;
    ensure!(!metadata.file_type().is_symlink(), "fixture cache contains a symlink");
    if metadata.is_dir() {
      copy_tree(&source, &destination)?;
    } else {
      ensure!(metadata.is_file(), "fixture cache source is not a regular file");
      fs::copy(&source, &destination)?;
    }
  }
  fs::set_permissions(destination, metadata.permissions())?;
  Ok(())
}

fn copy_file(source: &Path, destination: &Path) -> Result<()> {
  ensure!(source.is_file(), "fixture cache source is not a file");
  if destination.exists() {
    return Ok(());
  }
  fs::create_dir_all(destination.parent().context("fixture cache file parent")?)?;
  fs::copy(source, destination)?;
  Ok(())
}

fn registry_index_path(crate_name: &str) -> Result<PathBuf> {
  ensure!(crate_name.is_ascii() && !crate_name.is_empty());
  Ok(match crate_name.len() {
    1 => PathBuf::from("1").join(crate_name),
    2 => PathBuf::from("2").join(crate_name),
    3 => PathBuf::from("3").join(&crate_name[..1]).join(crate_name),
    _ => PathBuf::from(&crate_name[..2]).join(&crate_name[2..4]).join(crate_name),
  })
}

fn seed_isolated_cargo_home(fixture: &Path, cargo_home: &Path) -> Result<()> {
  let metadata = cargo_metadata(fixture, None)?;
  let packages = metadata["packages"].as_array().context("fixture metadata packages")?;
  fs::create_dir_all(cargo_home)?;
  for package in packages {
    let Some(source) = package["source"].as_str() else {
      continue;
    };
    let manifest = PathBuf::from(package["manifest_path"].as_str().context("fixture manifest path")?);
    if source.starts_with("registry+") {
      let package_source = manifest.parent().context("fixture registry package root")?;
      let index_name = package_source
        .parent()
        .and_then(Path::file_name)
        .context("fixture registry source index")?;
      let registry_root = package_source
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .context("ambient Cargo registry root")?;
      let package_name = package["name"].as_str().context("fixture registry package name")?;
      let package_version = package["version"]
        .as_str()
        .context("fixture registry package version")?;
      let cache_index = registry_root.join("cache").join(index_name);
      let sparse_index = registry_root.join("index").join(index_name);
      let destination_source_index = cargo_home.join("registry/src").join(index_name);
      fs::create_dir_all(&destination_source_index)?;
      copy_tree(
        package_source,
        &destination_source_index.join(package_source.file_name().context("package source")?),
      )?;
      copy_file(
        &cache_index.join(format!("{package_name}-{package_version}.crate")),
        &cargo_home
          .join("registry/cache")
          .join(index_name)
          .join(format!("{package_name}-{package_version}.crate")),
      )?;
      copy_file(
        &sparse_index.join("config.json"),
        &cargo_home.join("registry/index").join(index_name).join("config.json"),
      )?;
      let index_path = registry_index_path(package_name)?;
      copy_file(
        &sparse_index.join(".cache").join(&index_path),
        &cargo_home
          .join("registry/index")
          .join(index_name)
          .join(".cache")
          .join(index_path),
      )?;
    } else if source.starts_with("git+") {
      let checkout = manifest
        .ancestors()
        .find(|ancestor| ancestor.join(".cargo-ok").is_file())
        .context("ambient Cargo Git checkout root")?;
      let repository = checkout.parent().context("ambient Cargo Git repository checkout")?;
      let repository_name = repository.file_name().context("ambient Cargo Git repository name")?;
      let git_root = repository
        .parent()
        .and_then(Path::parent)
        .context("ambient Cargo Git root")?;
      let destination_checkout = cargo_home.join("git/checkouts").join(repository_name);
      if !destination_checkout.exists() {
        copy_tree(repository, &destination_checkout)?;
      }
      let destination_database = cargo_home.join("git/db").join(repository_name);
      if !destination_database.exists() {
        copy_tree(&git_root.join("db").join(repository_name), &destination_database)?;
      }
    }
  }
  cargo_metadata(fixture, Some(cargo_home))?;
  Ok(())
}

fn setup_cache(fixture: &Path, cargo_home: &Path, cache_base: &Path) -> Result<()> {
  let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(fixture)
    .args(["rail", "cache", "setup", "--local-dir"])
    .arg(cache_base)
    .env("CARGO_HOME", cargo_home)
    .env_remove("RUSTC_WRAPPER")
    .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER")
    .output()?;
  ensure!(
    output.status.success(),
    "transparent setup failed:\n{}",
    String::from_utf8_lossy(&output.stderr)
  );
  Ok(())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Usage {
  hits: u64,
  misses: u64,
  bypasses: u64,
  failures: u64,
}

impl Usage {
  fn difference(self, before: Self) -> Self {
    Self {
      hits: self.hits.saturating_sub(before.hits),
      misses: self.misses.saturating_sub(before.misses),
      bypasses: self.bypasses.saturating_sub(before.bypasses),
      failures: self.failures.saturating_sub(before.failures),
    }
  }
}

fn cache_usage(fixture: &Path, cargo_home: &Path) -> Result<Usage> {
  let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(fixture)
    .args(["rail", "cache", "status", "--scope", "local", "-f", "json"])
    .env("CARGO_HOME", cargo_home)
    .env_remove("RUSTC_WRAPPER")
    .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER")
    .output()?;
  ensure!(output.status.success(), "cache status failed");
  let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
  let usage = &value["status"]["installation"]["usage"];
  Ok(Usage {
    hits: usage["hits"].as_u64().context("usage hits")?,
    misses: usage["misses"].as_u64().context("usage misses")?,
    bypasses: usage["bypasses"].as_u64().context("usage bypasses")?,
    failures: usage["failures"].as_u64().context("usage failures")?,
  })
}

fn run_cargo(
  fixture: &Path,
  cargo_home: &Path,
  workload: &str,
  environment: &[(&str, &str)],
) -> Result<(Output, Usage)> {
  let before = cache_usage(fixture, cargo_home)?;
  let mut command = Command::new("cargo");
  command.current_dir(fixture).arg(workload);
  if workload == "build" {
    command.arg("--release");
  }
  command
    .args([
      "--workspace",
      "--all-features",
      "--locked",
      "--offline",
      "--message-format=json-render-diagnostics",
    ])
    .env("CARGO_HOME", cargo_home)
    .env("CARGO_INCREMENTAL", "0")
    .env("CARGO_TERM_COLOR", "never")
    .env_remove("RUSTC_WRAPPER")
    .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER");
  for (name, value) in environment {
    command.env(name, value);
  }
  let output = command.output()?;
  ensure!(
    output.status.success(),
    "cargo {workload} failed:\nstdout:\n{}\nstderr:\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  let usage = cache_usage(fixture, cargo_home)?.difference(before);
  Ok((output, usage))
}

fn add_current_root_diagnostic(fixture: &Path) -> Result<()> {
  let path = fixture.join("crates/fixture-types/src/lib.rs");
  let mut source = fs::read_to_string(&path)?;
  source.push_str(
    "\n/// Emit a stable compiler diagnostic whose source path must follow the active root.\n\
     pub fn cargo_rail_diagnostic() -> u64 {\n\
       let cargo_rail_current_root = 0_u64;\n\
       0\n\
     }\n",
  );
  fs::write(path, source)?;
  Ok(())
}

fn current_root_diagnostic(output: &Output) -> Result<String> {
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  stdout
    .lines()
    .chain(stderr.lines())
    .find(|line| line.contains("unused variable: `cargo_rail_current_root`"))
    .map(str::to_string)
    .with_context(|| format!("fixture compiler diagnostic was not emitted:\nstdout:\n{stdout}\nstderr:\n{stderr}"))
}

fn digest_file(path: &Path) -> Result<String> {
  let mut hasher = Sha256::new();
  hasher.update(fs::read(path)?);
  Ok(hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

fn reusable_outputs(target: &Path) -> Result<BTreeMap<PathBuf, String>> {
  fn visit(target: &Path, current: &Path, outputs: &mut BTreeMap<PathBuf, String>) -> Result<()> {
    let mut entries = fs::read_dir(current)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
      let path = entry.path();
      let metadata = fs::symlink_metadata(&path)?;
      if metadata.is_dir() {
        visit(target, &path, outputs)?;
      } else if metadata.is_file()
        && matches!(
          path.extension().and_then(|value| value.to_str()),
          Some("d" | "rmeta" | "rlib")
        )
      {
        outputs.insert(path.strip_prefix(target)?.to_path_buf(), digest_file(&path)?);
      }
    }
    Ok(())
  }
  let mut outputs = BTreeMap::new();
  visit(target, target, &mut outputs)?;
  Ok(outputs)
}

fn native_action_keys(cache_base: &Path) -> Result<BTreeSet<String>> {
  let directory = cache_base.join("cargo-rail/local-cas-v2/native-actions-v2");
  let mut keys = BTreeSet::new();
  for entry in fs::read_dir(directory)? {
    let entry = entry?;
    let path = entry.path();
    if path.extension().and_then(|value| value.to_str()) == Some("json") {
      let state: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
      keys.insert(state["action_key"].as_str().context("native action key")?.to_string());
    }
  }
  Ok(keys)
}

fn benchmark_events(directory: &Path) -> Result<Vec<serde_json::Value>> {
  let mut paths = fs::read_dir(directory)?
    .map(|entry| entry.map(|entry| entry.path()))
    .collect::<Result<Vec<_>, _>>()?;
  paths.sort();
  paths
    .into_iter()
    .map(|path| serde_json::from_slice(&fs::read(path)?).map_err(Into::into))
    .collect()
}

fn benchmark_hit_keys(directory: &Path) -> Result<Vec<String>> {
  let mut keys = Vec::new();
  for event in benchmark_events(directory)? {
    if event["status"] == "hit" {
      keys.push(
        event["action_key"]
          .as_str()
          .context("benchmark hit action key")?
          .to_string(),
      );
    }
  }
  Ok(keys)
}

fn benchmark_event_summary(directory: &Path) -> Result<BTreeMap<String, u64>> {
  let mut summary = BTreeMap::new();
  for event in benchmark_events(directory)? {
    let status = event["status"].as_str().context("benchmark event status")?;
    let reason = event["reason"].as_str().context("benchmark event reason")?;
    *summary.entry(format!("{status}:{reason}")).or_default() += 1;
  }
  Ok(summary)
}

fn create_private_directory(path: &Path) -> Result<()> {
  fs::create_dir(path)?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
  }
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
fn real_cargo_check_and_build_reuse_exact_outputs_with_root_bound_authority() -> Result<()> {
  let root = tempfile::tempdir()?;
  let first = root.path().join("first");
  let second = root.path().join("second");
  let git_source = root.path().join("git-source");
  let first_cache = root.path().join("first-cache");
  let second_cache = root.path().join("second-cache");
  let first_cargo_home = root.path().join("first-cargo-home");
  let second_cargo_home = root.path().join("second-cargo-home");
  materialize_fixture(&first, &git_source)?;
  materialize_fixture(&second, &git_source)?;
  add_current_root_diagnostic(&first)?;
  add_current_root_diagnostic(&second)?;
  seed_isolated_cargo_home(&first, &first_cargo_home)?;
  seed_isolated_cargo_home(&second, &second_cargo_home)?;
  setup_cache(&first, &first_cargo_home, &first_cache)?;
  setup_cache(&second, &second_cargo_home, &second_cache)?;

  let (_, first_cold) = run_cargo(&first, &first_cargo_home, "check", &[])?;
  let (second_cold_output, second_cold) = run_cargo(&second, &second_cargo_home, "check", &[])?;
  ensure!(first_cold.hits == 0 && first_cold.misses >= 12, "{first_cold:?}");
  ensure!(second_cold.hits == 0 && second_cold.misses >= 12, "{second_cold:?}");
  ensure!(first_cold.failures == 0 && second_cold.failures == 0);
  let first_keys = native_action_keys(&first_cache)?;
  let second_keys = native_action_keys(&second_cache)?;
  ensure!(!first_keys.is_empty() && !second_keys.is_empty());
  ensure!(
    first_keys.is_disjoint(&second_keys),
    "exact actions crossed physical roots"
  );
  let second_outputs = reusable_outputs(&second.join("target"))?;
  let second_diagnostic = current_root_diagnostic(&second_cold_output)?;

  setup_cache(&second, &second_cargo_home, &first_cache)?;
  fs::remove_dir_all(second.join("target"))?;
  let root_bound_events = fs::canonicalize(root.path())?.join("root-bound-events");
  create_private_directory(&root_bound_events)?;
  let root_bound_events_value = root_bound_events.to_string_lossy().into_owned();
  let (_, root_bound_cold) = run_cargo(
    &second,
    &second_cargo_home,
    "check",
    &[
      ("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1"),
      (
        "CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY",
        root_bound_events_value.as_str(),
      ),
    ],
  )?;
  let root_bound_hits = benchmark_hit_keys(&root_bound_events)?;
  ensure!(
    root_bound_hits.len() as u64 == root_bound_cold.hits,
    "usage and action ledgers disagree: {root_bound_cold:?}, {root_bound_hits:?}"
  );
  ensure!(
    root_bound_hits
      .iter()
      .all(|key| second_keys.contains(key) && !first_keys.contains(key)),
    "a hit did not come from an action published for the current physical root: {root_bound_hits:?}"
  );
  ensure!(
    root_bound_cold.hits.saturating_add(root_bound_cold.misses) == second_cold.misses,
    "root-bound reconstruction changed the eligible action count: {root_bound_cold:?}"
  );
  ensure!(native_action_keys(&first_cache)?.is_superset(&second_keys));
  ensure!(reusable_outputs(&second.join("target"))? == second_outputs);

  fs::remove_dir_all(second.join("target"))?;
  let second_warm_events = fs::canonicalize(root.path())?.join("second-warm-events");
  create_private_directory(&second_warm_events)?;
  let second_warm_events_value = second_warm_events.to_string_lossy().into_owned();
  let (second_warm_output, second_warm) = run_cargo(
    &second,
    &second_cargo_home,
    "check",
    &[
      ("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1"),
      (
        "CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY",
        second_warm_events_value.as_str(),
      ),
    ],
  )?;
  let second_warm_summary = benchmark_event_summary(&second_warm_events)?;
  ensure!(
    second_warm.hits == second_keys.len() as u64,
    "same-root warm restore did not hit every action: expected={}, usage={second_warm:?}, events={second_warm_summary:?}",
    second_keys.len(),
  );
  ensure!(
    second_warm.misses == 0 && second_warm.failures == 0,
    "same-root warm restore was not clean: {second_warm:?}"
  );
  ensure!(reusable_outputs(&second.join("target"))? == second_outputs);
  ensure!(current_root_diagnostic(&second_warm_output)? == second_diagnostic);

  let (_, cargo_l0) = run_cargo(&second, &second_cargo_home, "check", &[])?;
  ensure!(
    cargo_l0 == Usage::default(),
    "Cargo-fresh work contacted L1: {cargo_l0:?}"
  );

  fs::remove_dir_all(second.join("target"))?;
  let (_, sdk_changed) = run_cargo(&second, &second_cargo_home, "check", &[("SDKROOT", "/")])?;
  ensure!(
    sdk_changed.hits == 0 && sdk_changed.misses >= second_cold.misses,
    "SDKROOT change did not invalidate every action: {sdk_changed:?}"
  );

  fs::remove_dir_all(first.join("target"))?;
  let (_, build_cold) = run_cargo(&first, &first_cargo_home, "build", &[])?;
  ensure!(build_cold.hits == 0 && build_cold.misses >= 8, "{build_cold:?}");
  let build_outputs = reusable_outputs(&first.join("target/release"))?;
  let binary = executable(first.join("target/release/fixture-cli"));
  let cold_binary = Command::new(&binary).output()?;
  ensure!(cold_binary.status.success());
  ensure!(String::from_utf8_lossy(&cold_binary.stdout).trim() == "119");

  fs::remove_dir_all(first.join("target"))?;
  let (_, build_warm) = run_cargo(&first, &first_cargo_home, "build", &[])?;
  ensure!(
    build_warm.hits == build_cold.misses && build_warm.misses == 0,
    "{build_warm:?}"
  );
  ensure!(build_warm.failures == 0);
  ensure!(reusable_outputs(&first.join("target/release"))? == build_outputs);
  let warm_binary = Command::new(binary).output()?;
  ensure!(warm_binary.status.success());
  ensure!(String::from_utf8_lossy(&warm_binary.stdout).trim() == "119");
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
  ensure!(packages.iter().any(|package| {
    package["targets"].as_array().into_iter().flatten().any(|target| {
      target["kind"]
        .as_array()
        .is_some_and(|kinds| kinds.iter().any(|kind| kind == "custom-build"))
    })
  }));
  ensure!(packages.iter().any(|package| {
    package["targets"].as_array().into_iter().flatten().any(|target| {
      target["kind"]
        .as_array()
        .is_some_and(|kinds| kinds.iter().any(|kind| kind == "proc-macro"))
    })
  }));

  let check = Command::new("cargo")
    .current_dir(&fixture)
    .args([
      "check",
      "--workspace",
      "--all-targets",
      "--all-features",
      "--locked",
      "--offline",
    ])
    .output()?;
  ensure!(check.status.success(), "fixture check failed");
  let build = Command::new("cargo")
    .current_dir(&fixture)
    .args(["build", "--workspace", "--all-features", "--locked", "--offline"])
    .output()?;
  ensure!(build.status.success(), "fixture build failed");
  ensure!(executable(target.join("debug/fixture-cli")).is_file());
  Ok(())
}
