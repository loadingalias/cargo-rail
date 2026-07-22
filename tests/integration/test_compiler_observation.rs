use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

use crate::helpers::TestWorkspace;

const CACHE_WRAPPER_MARKER: &str = "CARGO_RAIL_COMPILER_CACHE_WRAPPER";

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
    .filter_map(|entry| entry["observations"].as_array())
    .flatten()
    .find(|observation| observation["unit"]["target_name"] == target_name)
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

fn run_unify_without_ambient_wrappers(workspace: &Path, cache: &Path) -> Result<std::process::Output> {
  run_unify_with_environment(workspace, cache, &[])
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

fn native_cache_observation(workspace: &Path) -> Result<serde_json::Value> {
  let cache: serde_json::Value = serde_json::from_slice(&fs::read(
    workspace.join("target/cargo-rail/cache/compiler-diags-v1.json"),
  )?)?;
  local_observation(&cache, "wrapper_app")
}

#[cfg(target_os = "macos")]
fn assert_native_miss(workspace: &Path, output: &std::process::Output) -> Result<serde_json::Value> {
  assert_eq!(output.status.code(), Some(1), "unexpected unify result: {output:?}");
  let observation = native_cache_observation(workspace)?;
  assert_ne!(
    observation["execution"]["cache_wrapper"]["status"], "hit",
    "mutated input must not authorize native reuse: {observation}"
  );
  Ok(observation)
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

#[cfg(target_os = "macos")]
#[test]
fn compiler_observation_records_verified_native_cache_miss_and_hit() -> Result<()> {
  let workspace = wrapper_workspace("disabled-cache-wrapper-observation")?;
  let local_cache = tempfile::tempdir()?;
  let output = run_unify_without_ambient_wrappers(&workspace.path, local_cache.path())?;
  assert_eq!(output.status.code(), Some(1), "unexpected unify result: {output:?}");
  assert!(
    workspace
      .path
      .join("target/cargo-rail/hermetic/local-cas-v1.json")
      .is_file(),
    "native reuse must retain P7.1's validated cleanup reference"
  );

  let cache: serde_json::Value = serde_json::from_slice(&fs::read(
    workspace.path.join("target/cargo-rail/cache/compiler-diags-v1.json"),
  )?)?;
  let observation = local_observation(&cache, "wrapper_app")?;
  assert_eq!(observation["execution"]["cache_wrapper"]["status"], "miss");
  assert_eq!(
    observation["execution"]["cache_wrapper"]["reason"],
    "candidate_not_found;stored_verified_result"
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
  fs::remove_dir_all(&target)?;
  let output = run_unify_without_ambient_wrappers(&workspace.path, local_cache.path())?;
  assert_eq!(
    output.status.code(),
    Some(1),
    "unexpected warm unify result: {output:?}"
  );
  let cache_file: serde_json::Value = serde_json::from_slice(&fs::read(
    workspace.path.join("target/cargo-rail/cache/compiler-diags-v1.json"),
  )?)?;
  let observation = local_observation(&cache_file, "wrapper_app")?;
  assert_eq!(observation["execution"]["cache_wrapper"]["status"], "hit");
  assert_eq!(
    observation["execution"]["cache_wrapper"]["reason"],
    "verified_local_result"
  );
  assert_eq!(compiler_output_files(&target)?, cold_outputs);

  let first_candidate = observation["execution"]["cache_wrapper"]["candidate_key"]
    .as_str()
    .context("first-root candidate key")?
    .to_string();
  let second = wrapper_workspace("native-cache-second-independent-root")?;
  let second_cold = run_unify_without_ambient_wrappers(&second.path, local_cache.path())?;
  assert_eq!(
    second_cold.status.code(),
    Some(1),
    "second-root cold run: {second_cold:?}"
  );
  let second_observation = native_cache_observation(&second.path)?;
  assert_eq!(second_observation["execution"]["cache_wrapper"]["status"], "miss");
  assert_ne!(
    second_observation["execution"]["cache_wrapper"]["candidate_key"], first_candidate,
    "physical roots stay independently authorized"
  );
  let second_target = second.path.join("target");
  let second_cold_outputs = compiler_output_files(&second_target)?;
  fs::remove_dir_all(&second_target)?;
  let second_warm = run_unify_without_ambient_wrappers(&second.path, local_cache.path())?;
  assert_eq!(
    second_warm.status.code(),
    Some(1),
    "second-root warm run: {second_warm:?}"
  );
  let second_observation = native_cache_observation(&second.path)?;
  assert_eq!(second_observation["execution"]["cache_wrapper"]["status"], "hit");
  assert_eq!(compiler_output_files(&second_target)?, second_cold_outputs);

  let cleanup = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(&second.path)
    .args(["rail", "clean", "--cache", "--quiet"])
    .env_remove("RUSTC_WRAPPER")
    .env_remove("RUSTC_WORKSPACE_WRAPPER")
    .env("CARGO_RAIL_CACHE_DIR", local_cache.path())
    .output()?;
  assert!(cleanup.status.success(), "native cache cleanup failed: {cleanup:?}");
  assert!(
    !local_cache.path().join("cargo-rail/local-cas-v1").exists(),
    "validated cleanup must remove the owned native CAS"
  );
  Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn native_cache_mutations_produce_no_false_hits() -> Result<()> {
  let workspace = wrapper_workspace("native-cache-mutation-matrix")?;
  let source = workspace.path.join("crates/wrapper-app/src/lib.rs");
  fs::write(
    &source,
    "pub const BUILD_VALUE: &str = env!(\"P73_VALUE\");\npub const MARKER: u8 = 1;\n",
  )?;
  workspace.commit("Add native cache mutation fixture")?;
  let dependency = workspace.path.join("vendor/wrapper-dep/src/lib.rs");
  let local_cache = tempfile::tempdir()?;
  let target = workspace.path.join("target");

  let cold = run_unify_with_environment(&workspace.path, local_cache.path(), &[("P73_VALUE", "one")])?;
  assert_native_miss(&workspace.path, &cold)?;
  fs::remove_dir_all(&target)?;
  let warm = run_unify_with_environment(&workspace.path, local_cache.path(), &[("P73_VALUE", "one")])?;
  assert_eq!(warm.status.code(), Some(1), "warm run: {warm:?}");
  assert_eq!(
    native_cache_observation(&workspace.path)?["execution"]["cache_wrapper"]["status"],
    "hit"
  );

  fs::remove_dir_all(&target)?;
  fs::write(
    &source,
    "pub const BUILD_VALUE: &str = env!(\"P73_VALUE\");\npub const MARKER: u8 = 2;\n",
  )?;
  let source_mutation = run_unify_with_environment(&workspace.path, local_cache.path(), &[("P73_VALUE", "one")])?;
  assert_native_miss(&workspace.path, &source_mutation)?;

  fs::remove_dir_all(&target)?;
  let environment_mutation = run_unify_with_environment(&workspace.path, local_cache.path(), &[("P73_VALUE", "two")])?;
  assert_native_miss(&workspace.path, &environment_mutation)?;

  fs::remove_dir_all(&target)?;
  let flag_mutation = run_unify_with_environment(
    &workspace.path,
    local_cache.path(),
    &[("P73_VALUE", "two"), ("RUSTFLAGS", "--cfg=p73_mutation")],
  )?;
  assert_native_miss(&workspace.path, &flag_mutation)?;

  fs::remove_dir_all(&target)?;
  let compiler_environment_mutation = run_unify_with_environment(
    &workspace.path,
    local_cache.path(),
    &[("P73_VALUE", "two"), ("RUSTC_BOOTSTRAP", "1")],
  )?;
  assert_native_miss(&workspace.path, &compiler_environment_mutation)?;

  fs::remove_dir_all(&target)?;
  fs::write(&dependency, "pub fn dependency() { let _changed = true; }\n")?;
  let dependency_mutation = run_unify_with_environment(&workspace.path, local_cache.path(), &[("P73_VALUE", "two")])?;
  assert_native_miss(&workspace.path, &dependency_mutation)?;
  Ok(())
}

#[cfg(target_os = "macos")]
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
  assert_native_miss(&workspace.path, &cold)?;
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
  let observation = assert_native_miss(&workspace.path, &fallback)?;
  assert!(
    observation["execution"]["cache_wrapper"]["reason"]
      .as_str()
      .is_some_and(|reason| reason.contains("local_cache_store_failed") || reason.contains("materialization")),
    "corruption must be explicit: {observation}"
  );
  assert_eq!(compiler_output_files(&target)?, cold_outputs);
  Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn filesystem_reading_macro_has_an_explicit_native_bypass() -> Result<()> {
  let workspace = wrapper_workspace("native-cache-filesystem-macro")?;
  fs::write(workspace.path.join("crates/wrapper-app/src/value.txt"), "observed\n")?;
  fs::write(
    workspace.path.join("crates/wrapper-app/src/lib.rs"),
    "pub const VALUE: &str = include_str!(\"value.txt\");\n",
  )?;
  workspace.commit("Add filesystem macro fixture")?;
  let local_cache = tempfile::tempdir()?;
  let output = run_unify_without_ambient_wrappers(&workspace.path, local_cache.path())?;
  assert_eq!(output.status.code(), Some(1), "filesystem macro run: {output:?}");
  let observation = native_cache_observation(&workspace.path)?;
  assert_eq!(observation["execution"]["cache_wrapper"]["status"], "bypassed");
  assert_eq!(
    observation["execution"]["cache_wrapper"]["reason"],
    "filesystem_reading_macro_not_graduated"
  );
  Ok(())
}

#[cfg(not(target_os = "macos"))]
#[test]
fn unsupported_platform_bypasses_native_cache_and_preserves_cold_outputs() -> Result<()> {
  let workspace = wrapper_workspace("native-cache-unsupported-platform")?;
  let local_cache = tempfile::tempdir()?;
  let target = workspace.path.join("target");

  let first = run_unify_without_ambient_wrappers(&workspace.path, local_cache.path())?;
  assert_eq!(first.status.code(), Some(1), "first cold run: {first:?}");
  let first_observation = native_cache_observation(&workspace.path)?;
  assert_eq!(first_observation["execution"]["cache_wrapper"]["status"], "bypassed");
  assert_eq!(
    first_observation["execution"]["cache_wrapper"]["reason"],
    "native_cache_platform_not_graduated"
  );
  let first_outputs = compiler_output_files(&target)?;
  assert!(!first_outputs.is_empty(), "fixture must produce compiler outputs");

  fs::remove_dir_all(&target)?;
  let second = run_unify_without_ambient_wrappers(&workspace.path, local_cache.path())?;
  assert_eq!(second.status.code(), Some(1), "second cold run: {second:?}");
  let second_observation = native_cache_observation(&workspace.path)?;
  assert_eq!(second_observation["execution"]["cache_wrapper"]["status"], "bypassed");
  assert_eq!(
    second_observation["execution"]["cache_wrapper"]["reason"],
    "native_cache_platform_not_graduated"
  );
  assert_eq!(compiler_output_files(&target)?, first_outputs);
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
    .env("WRAPPER_LOG", &wrapper_log)
    .env("WORKSPACE_WRAPPER_LOG", &workspace_wrapper_log)
    .output()?;
  assert_eq!(output.status.code(), Some(1), "unexpected unify result: {output:?}");

  let cache: serde_json::Value = serde_json::from_slice(&fs::read(
    workspace.path.join("target/cargo-rail/cache/compiler-diags-v1.json"),
  )?)?;
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

#[test]
fn recursive_cargo_rail_wrapper_configuration_is_rejected_before_cargo() -> Result<()> {
  let workspace = wrapper_workspace("recursive-cargo-rail-wrapper")?;
  let output = Command::new(env!("CARGO_BIN_EXE_cargo-rail"))
    .current_dir(&workspace.path)
    .args(["rail", "unify", "--check"])
    .env("RUSTC_WRAPPER", env!("CARGO_BIN_EXE_cargo-rail"))
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
  assert!(
    record["compiler_arguments"]
      .as_array()
      .is_some_and(|arguments| arguments.iter().any(|argument| argument
        .as_str()
        .is_some_and(|argument| argument.starts_with("--emit=") && argument.contains("dep-info"))))
  );
  let observed_paths = record["observed_reads"]
    .as_array()
    .context("observed rustdoc reads")?
    .iter()
    .filter_map(|read| read["path"]["path"].as_str())
    .collect::<Vec<_>>();
  assert!(
    observed_paths.contains(&"src/lib.rs"),
    "crate root missing from {record}"
  );
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
      "portable compiler argv must not retain checkout root '{}': {record}",
      root.display()
    );
  }

  Ok(())
}
