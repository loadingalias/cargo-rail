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

fn run_cargo_rail(fixture: &Path, action: &str, cache: &Path, cargo_home: &Path) -> Result<String> {
  run_cargo_rail_with_options(fixture, action, cache, cargo_home, true, &[])
}

fn run_cargo_rail_with_environment(
  fixture: &Path,
  action: &str,
  cache: &Path,
  cargo_home: &Path,
  environment: &[(&str, &str)],
) -> Result<String> {
  run_cargo_rail_with_options(fixture, action, cache, cargo_home, true, environment)
}

fn run_cargo_rail_with_options(
  fixture: &Path,
  action: &str,
  cache: &Path,
  cargo_home: &Path,
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
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  ensure!(
    output.status.success(),
    "cargo-rail {action} failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
  );
  Ok(format!("{stdout}\n{stderr}"))
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
  ensure!(
    metadata.is_dir(),
    "fixture cache source is not a directory: {}",
    source.display()
  );
  ensure!(
    !destination.exists(),
    "fixture cache destination already exists: {}",
    destination.display()
  );
  fs::create_dir_all(destination)?;
  let mut entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
  entries.sort_by_key(std::fs::DirEntry::file_name);
  for entry in entries {
    let source = entry.path();
    let destination = destination.join(entry.file_name());
    let metadata = fs::symlink_metadata(&source)?;
    ensure!(
      !metadata.file_type().is_symlink(),
      "fixture cache source contains a symlink: {}",
      source.display()
    );
    if metadata.is_dir() {
      copy_tree(&source, &destination)?;
    } else {
      ensure!(
        metadata.is_file(),
        "fixture cache source is not a regular file: {}",
        source.display()
      );
      fs::copy(&source, &destination)?;
    }
  }
  fs::set_permissions(destination, metadata.permissions())?;
  Ok(())
}

fn copy_file(source: &Path, destination: &Path) -> Result<()> {
  ensure!(
    source.is_file(),
    "fixture cache source is not a file: {}",
    source.display()
  );
  if destination.exists() {
    return Ok(());
  }
  fs::create_dir_all(destination.parent().context("fixture cache file parent")?)?;
  fs::copy(source, destination)?;
  Ok(())
}

fn registry_index_path(crate_name: &str) -> Result<PathBuf> {
  ensure!(
    crate_name.is_ascii() && !crate_name.is_empty(),
    "fixture registry package has an invalid name: {crate_name}"
  );
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
    let manifest = PathBuf::from(
      package["manifest_path"]
        .as_str()
        .context("fixture package manifest path")?,
    );
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
    if !event["unit_identity"].is_string() || !matches!(event["outcome"].as_str(), Some("hit" | "miss")) {
      continue;
    }
    ensure!(event["schema_version"] == 5, "unexpected native-cache event: {event}");
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

fn reusable_output_paths(
  root: &Path,
  target: &Path,
  units: &BTreeMap<String, serde_json::Value>,
  selected: &BTreeSet<String>,
) -> Result<BTreeSet<PathBuf>> {
  units
    .iter()
    .filter(|(identity, _)| selected.contains(*identity))
    .flat_map(|(_, event)| {
      event["unit"]["output_paths"]
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
        .collect::<BTreeSet<_>>()
    })
    .map(|path| {
      root
        .join(path)
        .strip_prefix(target)
        .map(Path::to_path_buf)
        .with_context(|| "native-cache output escaped target directory".to_string())
    })
    .collect()
}

fn unit_by_crate_name<'a>(
  units: &'a BTreeMap<String, serde_json::Value>,
  crate_name: &str,
) -> Result<(&'a str, &'a serde_json::Value)> {
  let matches = units
    .iter()
    .filter(|(_, event)| event["unit"]["descriptor"]["crate_name"] == crate_name)
    .collect::<Vec<_>>();
  ensure!(
    matches.len() == 1,
    "expected exactly one reusable '{crate_name}' unit, found {}: {matches:#?}",
    matches.len()
  );
  Ok((matches[0].0.as_str(), matches[0].1))
}

fn identity_hex(identity: &str) -> Result<&str> {
  let hex = identity
    .rsplit_once("-sha256-")
    .map(|(_, hex)| hex)
    .context("cache identity lacks a SHA-256 suffix")?;
  ensure!(
    hex.len() == 64
      && hex
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
    "cache identity has an invalid SHA-256 suffix: {identity}"
  );
  Ok(hex)
}

fn canonical_result_evidence(cache: &Path, action_key: &str) -> Result<serde_json::Value> {
  let cas = cache.join("cargo-rail/local-cas-v2");
  let state_path = cas
    .join("native-actions-v2")
    .join(format!("{}.json", identity_hex(action_key)?));
  let state: serde_json::Value = serde_json::from_slice(
    &fs::read(&state_path).with_context(|| format!("read native action state {}", state_path.display()))?,
  )?;
  ensure!(
    state["action_key"] == action_key,
    "native action state changed identity: {state}"
  );
  ensure!(
    state["state"]["kind"] == "unique_result",
    "native action is not uniquely reusable: {state}"
  );
  let action_result = state["state"]["action_result"]
    .as_str()
    .context("native action state lacks its immutable result")?;
  let validation_directory = cas
    .join("results")
    .join(identity_hex(action_result)?)
    .join("validations");
  let mut validations = fs::read_dir(&validation_directory)?
    .collect::<Result<Vec<_>, _>>()?
    .into_iter()
    .map(|entry| entry.path())
    .filter(|path| path.extension().is_some_and(|extension| extension == "json"))
    .collect::<Vec<_>>();
  validations.sort_unstable();
  ensure!(
    validations.len() == 1,
    "native result must contain exactly one validation: {validation_directory:?}"
  );
  let validation: serde_json::Value = serde_json::from_slice(&fs::read(&validations[0])?)?;
  ensure!(
    validation["action_key"] == action_key,
    "native validation changed action identity: {validation}"
  );
  Ok(serde_json::json!({
    "result_key": validation["result_key"],
    "outputs": validation["outputs"],
    "stdout_digest": validation["stdout_digest"],
    "stdout_bytes": validation["stdout_bytes"],
    "stderr_digest": validation["stderr_digest"],
    "stderr_bytes": validation["stderr_bytes"],
  }))
}

fn root_spellings(path: &Path) -> BTreeSet<Vec<u8>> {
  let mut spellings = BTreeSet::from([path.to_string_lossy().as_bytes().to_vec()]);
  if let Ok(canonical) = fs::canonicalize(path) {
    spellings.insert(canonical.to_string_lossy().as_bytes().to_vec());
  }
  for spelling in spellings.clone() {
    spellings.insert(String::from_utf8_lossy(&spelling).replace('\\', "/").into_bytes());
  }
  spellings
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

fn current_root_diagnostic(output: &str) -> Result<String> {
  let lines = output.lines().collect::<Vec<_>>();
  let start = lines
    .iter()
    .position(|line| line.contains("unused variable: `cargo_rail_current_root`"))
    .with_context(|| format!("fixture compiler diagnostic was not replayed:\n{output}"))?;
  Ok(
    lines[start..]
      .iter()
      .take_while(|line| !line.trim().is_empty())
      .copied()
      .collect::<Vec<_>>()
      .join("\n"),
  )
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
fn real_cargo_check_and_build_reuse_exact_outputs_with_root_bound_authority() -> Result<()> {
  let root = tempfile::tempdir()?;
  let first = root.path().join("first");
  let second = root.path().join("second");
  let git_source = root.path().join("git-source");
  let check_cache = root.path().join("check-cache");
  let forced_cold_cache = root.path().join("forced-cold-cache");
  let first_cargo_home = root.path().join("first-cargo-home");
  let second_cargo_home = root.path().join("second-cargo-home");
  materialize_fixture(&first, &git_source)?;
  materialize_fixture(&second, &git_source)?;
  add_current_root_diagnostic(&first)?;
  add_current_root_diagnostic(&second)?;
  seed_isolated_cargo_home(&first, &first_cargo_home)?;
  seed_isolated_cargo_home(&second, &second_cargo_home)?;

  let first_cold = run_cargo_rail(&first, "build", &check_cache, &first_cargo_home)?;
  ensure!(cache_metric(&first_cold, "hits")? == 0, "{first_cold}");
  ensure!(cache_metric(&first_cold, "misses")? >= 12, "{first_cold}");
  ensure!(cache_metric(&first_cold, "cache_bytes_written")? > 0, "{first_cold}");
  let first_units = reusable_cache_units(&first_cold)?;

  let second_cold = run_cargo_rail(&second, "build", &forced_cold_cache, &second_cargo_home)?;
  ensure!(cache_metric(&second_cold, "hits")? == 0, "{second_cold}");
  ensure!(cache_metric(&second_cold, "misses")? >= 12, "{second_cold}");
  ensure!(cache_metric(&second_cold, "cache_bytes_written")? > 0, "{second_cold}");
  let second_units = reusable_cache_units(&second_cold)?;
  let overlapping_units = first_units
    .keys()
    .filter(|identity| second_units.contains_key(*identity))
    .cloned()
    .collect::<BTreeSet<_>>();
  ensure!(
    overlapping_units.is_empty(),
    "exact compiler actions crossed isolated physical roots: {overlapping_units:#?}"
  );
  for (source_class, crate_name) in [
    ("workspace", "fixture_service_a"),
    ("registry", "regex_syntax"),
    ("Git", "fixture_git"),
  ] {
    let (first_identity, first_event) = unit_by_crate_name(&first_units, crate_name)?;
    let (second_identity, second_event) = unit_by_crate_name(&second_units, crate_name)?;
    ensure!(
      first_identity != second_identity,
      "{source_class} unit '{crate_name}' retained one action across isolated roots:\n\
       first={first_event:#?}\nsecond={second_event:#?}"
    );
  }
  let second_action_keys = second_units.keys().cloned().collect::<BTreeSet<_>>();
  let exact_paths = reusable_output_paths(&second, &second.join("target"), &second_units, &second_action_keys)?;
  let second_outputs = output_digests_at(&second.join("target"), exact_paths.iter())?;

  fs::remove_dir_all(second.join("target"))?;
  let second_root_cold = run_cargo_rail(&second, "build", &check_cache, &second_cargo_home)?;
  let second_root_cold_units = reusable_cache_units(&second_root_cold)?;
  ensure!(
    cache_metric(&second_root_cold, "hits")? == 0,
    "a first compilation in the second root restored first-root artifacts:\n\
     first cold:\n{first_cold}\nsecond forced cold:\n{second_cold}\nsecond root cold:\n{second_root_cold}"
  );
  ensure!(
    cache_metric(&second_root_cold, "bypasses")? == cache_metric(&second_cold, "bypasses")?,
    "root-bound cold check changed the control bypass set:\ncontrol:\n{second_cold}\nroot-bound:\n{second_root_cold}"
  );
  ensure!(
    cache_metric(&second_root_cold, "misses")? == second_units.len() as u64,
    "{second_root_cold}"
  );
  for (source_class, crate_name) in [
    ("workspace", "fixture_service_a"),
    ("registry", "regex_syntax"),
    ("Git", "fixture_git"),
  ] {
    let (control_identity, control_event) = unit_by_crate_name(&second_units, crate_name)?;
    let (cold_identity, cold_event) = unit_by_crate_name(&second_root_cold_units, crate_name)?;
    ensure!(
      control_identity == cold_identity && control_event["result_key"] == cold_event["result_key"],
      "root-bound {source_class} unit '{crate_name}' changed exact action/result identity:\n\
       control={control_event:#?}\nroot-bound={cold_event:#?}"
    );
    ensure!(
      canonical_result_evidence(&forced_cold_cache, control_identity)?
        == canonical_result_evidence(&check_cache, cold_identity)?,
      "root-bound {source_class} unit '{crate_name}' changed canonical exact outputs"
    );
  }
  ensure!(
    output_digests_at(&second.join("target"), second_outputs.keys())? == second_outputs,
    "a root-bound miss differed from the independent forced-cold outputs"
  );

  fs::remove_dir_all(second.join("target"))?;
  let second_warm = run_cargo_rail(&second, "build", &check_cache, &second_cargo_home)?;
  let warm_units = reusable_cache_units(&second_warm)?;
  ensure!(
    cache_metric(&second_warm, "hits")? == second_units.len() as u64,
    "same-root clean-target reuse restored an unexpected action set:\n{second_warm}"
  );
  ensure!(cache_metric(&second_warm, "misses")? == 0, "{second_warm}");
  for (source_class, crate_name) in [
    ("workspace", "fixture_service_a"),
    ("registry", "regex_syntax"),
    ("Git", "fixture_git"),
  ] {
    let (cold_identity, cold_event) = unit_by_crate_name(&second_units, crate_name)?;
    let (warm_identity, warm_event) = unit_by_crate_name(&warm_units, crate_name)?;
    ensure!(
      cold_identity == warm_identity,
      "warm {source_class} unit '{crate_name}' changed action identity"
    );
    ensure!(
      warm_event["outcome"] == "hit",
      "warm {source_class} unit '{crate_name}' did not hit: {warm_event}"
    );
    ensure!(
      warm_event["result_key"] == cold_event["result_key"],
      "warm {source_class} unit '{crate_name}' restored a different result: {warm_event}"
    );
  }
  let cold_diagnostic = current_root_diagnostic(&second_cold)?;
  let warm_diagnostic = current_root_diagnostic(&second_warm)?;
  ensure!(
    warm_diagnostic == cold_diagnostic,
    "a cache hit changed the current-root compiler diagnostic:\ncold:\n{cold_diagnostic}\nwarm:\n{warm_diagnostic}"
  );
  ensure!(
    warm_diagnostic.contains(
      Path::new("crates")
        .join("fixture-types")
        .join("src")
        .join("lib.rs")
        .to_string_lossy()
        .as_ref()
    ),
    "compiler diagnostic is not rooted at the current workspace: {warm_diagnostic}"
  );
  for stale in root_spellings(&first)
    .into_iter()
    .chain(root_spellings(&first_cargo_home))
  {
    let stale = String::from_utf8_lossy(&stale);
    ensure!(
      !second_warm.contains(stale.as_ref()),
      "same-root reuse leaked the first root into current diagnostics: {stale}"
    );
  }
  ensure!(
    root_spellings(&second)
      .into_iter()
      .any(|current| second_warm.contains(String::from_utf8_lossy(&current).as_ref())),
    "same-root reuse did not report the current workspace root:\n{second_warm}"
  );
  let check_bytes_restored = cache_metric(&second_warm, "bytes_restored")?;
  ensure!(check_bytes_restored > 0, "{second_warm}");
  ensure!(
    cache_metric(&second_warm, "cache_bytes_read")? >= check_bytes_restored,
    "{second_warm}"
  );
  let warm_misses = cache_metric(&second_warm, "misses")?;
  let warm_bytes_written = cache_metric(&second_warm, "cache_bytes_written")?;
  ensure!(
    (warm_misses == 0) == (warm_bytes_written == 0),
    "warm publication bytes do not match the remaining cold action set: {second_warm}"
  );
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
  let sdk_mutation = run_cargo_rail_with_environment(
    &second,
    "build",
    &check_cache,
    &second_cargo_home,
    &[("SDKROOT", sdk_root.as_str())],
  )?;
  ensure!(cache_metric(&sdk_mutation, "hits")? == 0, "{sdk_mutation}");

  fs::remove_dir_all(second.join("target"))?;
  let linker_mutation = run_cargo_rail_with_environment(
    &second,
    "build",
    &check_cache,
    &second_cargo_home,
    &[("LD", "/cargo-rail/not-used-by-graduated-library-units")],
  )?;
  ensure!(cache_metric(&linker_mutation, "hits")? == 0, "{linker_mutation}");

  fs::remove_dir_all(first.join("target"))?;
  let release_default =
    run_cargo_rail_with_options(&first, "distribution", &check_cache, &first_cargo_home, false, &[])?;
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
  let feature_cold = run_cargo_rail(&first, "distribution", &check_cache, &first_cargo_home)?;
  ensure!(cache_metric(&feature_cold, "misses")? >= 8, "{feature_cold}");
  let feature_binary = executable(first.join("target/release/fixture-cli"));
  let feature_output = Command::new(&feature_binary).output()?;
  ensure!(feature_output.status.success());
  ensure!(String::from_utf8_lossy(&feature_output.stdout).trim() == "119");
  let feature_outputs = reusable_output_digests(&first, &first.join("target/release"), &feature_cold)?;

  fs::remove_dir_all(first.join("target"))?;
  let feature_warm = run_cargo_rail(&first, "distribution", &check_cache, &first_cargo_home)?;
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
