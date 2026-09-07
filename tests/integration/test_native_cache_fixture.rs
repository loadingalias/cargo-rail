//! Retained real-workspace qualification for transparent native compiler reuse.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context as _, Result, ensure};
use rscrypto::Sha256;

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
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/materialize-native-cache.sh");
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

fn cargo_process(fixture: &Path, cargo_home: Option<&Path>) -> Command {
    let mut command = Command::new("cargo");
    command
        .current_dir(fixture)
        .env_remove("RUSTC_WRAPPER")
        .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER");
    if let Some(cargo_home) = cargo_home {
        command.env("CARGO_HOME", cargo_home);
    }
    command
}

fn cargo_metadata(fixture: &Path, cargo_home: Option<&Path>) -> Result<serde_json::Value> {
    let mut command = cargo_process(fixture, cargo_home);
    command.args([
        "metadata",
        "--locked",
        "--offline",
        "--all-features",
        "--format-version=1",
    ]);
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
        3 => PathBuf::from("3")
            .join(crate_name.get(..1).context("three-byte crate name prefix")?)
            .join(crate_name),
        _ => PathBuf::from(crate_name.get(..2).context("crate name index prefix")?)
            .join(crate_name.get(2..4).context("crate name index suffix")?)
            .join(crate_name),
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

fn cache_status(fixture: &Path, cargo_home: &Path) -> Result<serde_json::Value> {
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
    serde_json::from_slice(&output.stdout).map_err(Into::into)
}

fn cache_usage(fixture: &Path, cargo_home: &Path) -> Result<Usage> {
    let value = cache_status(fixture, cargo_home)?;
    let usage = &value["status"]["installation"]["usage"];
    Ok(Usage {
        hits: usage["hits"].as_u64().context("usage hits")?,
        misses: usage["misses"].as_u64().context("usage misses")?,
        bypasses: usage["bypasses"].as_u64().context("usage bypasses")?,
        failures: usage["failures"].as_u64().context("usage failures")?,
    })
}

fn profile_cache_root(fixture: &Path, cargo_home: &Path) -> Result<PathBuf> {
    let value = cache_status(fixture, cargo_home)?;
    value["status"]["local"]["cache"]["root"]
        .as_str()
        .map(PathBuf::from)
        .context("selected profile cache root")
}

fn run_cargo(
    fixture: &Path,
    cargo_home: &Path,
    workload: &str,
    environment: &[(&str, &str)],
) -> Result<(Output, Usage)> {
    let before = cache_usage(fixture, cargo_home)?;
    let mut command = cargo_command(fixture, cargo_home, workload);
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

fn cargo_command(fixture: &Path, cargo_home: &Path, workload: &str) -> Command {
    let mut command = cargo_process(fixture, Some(cargo_home));
    command.arg(workload);
    if workload == "build" {
        // Pipelined metadata can expose different library candidates in cold and warm builds.
        command.args(["--release", "--jobs", "1"]);
    } else if workload == "test" {
        command.args(["--no-run", "--all-targets"]);
    }
    command
        .args([
            "--workspace",
            "--all-features",
            "--locked",
            "--offline",
            "--message-format=json-render-diagnostics",
        ])
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_TERM_COLOR", "never");
    command
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
    hasher.update(&fs::read(path)?);
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
                    Some("a" | "d" | "dll" | "dylib" | "lib" | "rlib" | "rmeta" | "so")
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

fn native_action_keys(cache_root: &Path) -> Result<BTreeSet<String>> {
    let directory = cache_root.join("native-actions-v2");
    let mut keys = BTreeSet::new();
    if !directory.is_dir() {
        return Ok(keys);
    }
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

fn benchmark_action_keys(directory: &Path, status: &str) -> Result<Vec<String>> {
    let mut keys = Vec::new();
    for event in benchmark_events(directory)? {
        if event["status"] == status {
            keys.push(
                event["action_key"]
                    .as_str()
                    .with_context(|| format!("benchmark {status} action key"))?
                    .to_string(),
            );
        }
    }
    Ok(keys)
}

fn benchmark_action_crates(directory: &Path, status: &str) -> Result<Vec<String>> {
    Ok(benchmark_events(directory)?
        .into_iter()
        .filter(|event| event["status"] == status)
        .filter_map(|event| event["action"]["crate_name"].as_str().map(str::to_string))
        .collect())
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

fn ensure_typed_benchmark_events(directory: &Path) -> Result<()> {
    let events = benchmark_events(directory)?;
    ensure!(!events.is_empty(), "benchmark compiler operation inventory is empty");
    for event in events {
        ensure!(
            event["schema_version"] == 9,
            "benchmark compiler operation has an incompatible schema: {event}"
        );
        let action = event["action"].as_object().context("benchmark compiler operation")?;
        ensure!(
            action.get("schema_version") == Some(&serde_json::json!(3))
                && action.get("action_class").and_then(serde_json::Value::as_str).is_some()
                && action.get("driver").and_then(serde_json::Value::as_str).is_some()
                && action
                    .get("crate_types")
                    .and_then(serde_json::Value::as_array)
                    .is_some()
                && action.get("emit").and_then(serde_json::Value::as_array).is_some(),
            "benchmark compiler operation is incomplete: {event}"
        );
        let identity = event["action_id"]
            .as_str()
            .context("benchmark compiler operation identity")?;
        let digest = identity
            .strip_prefix("coverage-action-v3:sha256:")
            .context("benchmark compiler operation identity prefix")?;
        ensure!(
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "benchmark compiler operation identity is not canonical: {identity}"
        );
    }
    Ok(())
}

#[cfg(windows)]
fn ensure_windows_native_miss_boundary(
    usage: Usage,
    miss_crates: &[String],
    summary: &BTreeMap<String, u64>,
    phase: &str,
) -> Result<()> {
    // COFF linker observation deliberately bypasses this native producer. Its
    // rebuilt artifact changes the exact inputs of every downstream action, so
    // the safe miss boundary is the full dependent closure, including binaries.
    let allowed_native_misses = BTreeSet::from([
        "fixture_native",
        "fixture_native_sys",
        "fixture_service_b",
        "fixture_cli",
        "fixture_worker",
    ]);
    let observed_native_misses = miss_crates.iter().map(String::as_str).collect::<BTreeSet<_>>();
    ensure!(
        observed_native_misses.len() == miss_crates.len()
            && miss_crates.len() as u64 == usage.misses
            && observed_native_misses.is_subset(&allowed_native_misses)
            && (observed_native_misses.is_empty()
                || summary
                    .get("bypassed:dynamic_dependency_execution_observation_unavailable")
                    .is_some_and(|count| *count > 0)),
        "Windows {phase} misses escaped the explicitly ungraduated native-artifact boundary: \
     usage={usage:?}, misses={miss_crates:?}, events={summary:?}"
    );
    Ok(())
}

#[cfg(windows)]
fn ensure_windows_hit_outputs_are_exact(
    cold: &BTreeMap<PathBuf, String>,
    warm: &BTreeMap<PathBuf, String>,
    uncached_crates: &[String],
    phase: &str,
) -> Result<()> {
    ensure!(
        cold.keys().eq(warm.keys()),
        "Windows {phase} changed the reusable output inventory"
    );
    let changed = cold
        .iter()
        .filter(|(path, digest)| warm.get(*path) != Some(*digest))
        .map(|(path, _)| path)
        .collect::<Vec<_>>();
    ensure!(
        changed.iter().all(|path| {
            let normalized = path.to_string_lossy().replace('-', "_");
            uncached_crates.iter().any(|crate_name| normalized.contains(crate_name))
        }),
        "Windows {phase} changed output bytes outside its reported non-hit closure: \
     uncached={uncached_crates:?}, changed={changed:?}"
    );
    Ok(())
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

fn static_archive(directory: &Path) -> PathBuf {
    if cfg!(windows) {
        directory.join("fixture_static.lib")
    } else {
        directory.join("libfixture_static.a")
    }
}

fn dynamic_library(directory: &Path, crate_name: &str) -> PathBuf {
    if cfg!(windows) {
        directory.join(format!("{crate_name}.dll"))
    } else if cfg!(target_os = "macos") {
        directory.join(format!("lib{crate_name}.dylib"))
    } else {
        directory.join(format!("lib{crate_name}.so"))
    }
}

#[cfg(debug_assertions)]
#[test]
fn benchmark_coverage_event_failure_is_explicit() -> Result<()> {
    let root = tempfile::tempdir()?;
    let fixture = root.path().join("fixture");
    let cache = root.path().join("cache");
    let cargo_home = root.path().join("cargo-home");
    let events = root.path().join("events");
    fs::create_dir_all(fixture.join("src"))?;
    fs::create_dir(&cargo_home)?;
    fs::write(
        fixture.join("Cargo.toml"),
        "[package]\nname = \"coverage-failure\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )?;
    fs::write(
        fixture.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"coverage-failure\"\nversion = \"0.1.0\"\n",
    )?;
    fs::write(fixture.join("src/lib.rs"), "pub fn value() -> u32 { 42 }\n")?;
    setup_cache(&fixture, &cargo_home, &cache)?;
    create_private_directory(&events)?;
    let events = fs::canonicalize(events)?;

    let before = cache_usage(&fixture, &cargo_home)?;
    let output = cargo_command(&fixture, &cargo_home, "check")
        .env("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1")
        .env("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY", &events)
        .env("CARGO_RAIL_TEST_BENCH_COVERAGE_FAULT", "miss")
        .output()?;
    let usage = cache_usage(&fixture, &cargo_home)?.difference(before);
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure!(!output.status.success(), "injected coverage failure was ignored");
    ensure!(
        stderr.contains(
            "cargo-rail compiler cache wrapper: failed to record benchmark compiler coverage: coverage event: \
             injected benchmark compiler coverage miss event failure"
        ),
        "wrapper did not report the benchmark evidence failure:\n{stderr}"
    );
    ensure!(
        usage.misses == 1,
        "injected failure did not cross exactly one recorded miss: {usage:?}\n{stderr}"
    );
    ensure!(
        !benchmark_events(&events)?.iter().any(|event| event["status"] == "miss"),
        "faulted miss unexpectedly published benchmark evidence"
    );
    Ok(())
}

#[test]
fn real_cargo_check_reuses_exact_outputs_with_root_bound_authority() -> Result<()> {
    let root = tempfile::tempdir()?;
    let result: Result<()> = (|| {
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
        let first_cache_root = profile_cache_root(&first, &first_cargo_home)?;
        let second_cache_root = profile_cache_root(&second, &second_cargo_home)?;

        let (_, first_cold) = run_cargo(&first, &first_cargo_home, "check", &[])?;
        let (second_cold_output, second_cold) = run_cargo(&second, &second_cargo_home, "check", &[])?;
        ensure!(first_cold.hits == 0 && first_cold.misses >= 12, "{first_cold:?}");
        ensure!(second_cold.hits == 0 && second_cold.misses >= 12, "{second_cold:?}");
        ensure!(first_cold.failures == 0 && second_cold.failures == 0);
        let first_keys = native_action_keys(&first_cache_root)?;
        let second_keys = native_action_keys(&second_cache_root)?;
        ensure!(!first_keys.is_empty() && !second_keys.is_empty());
        ensure!(
            first_keys.is_disjoint(&second_keys),
            "exact actions crossed physical roots"
        );
        let second_outputs = reusable_outputs(&second.join("target"))?;
        let second_diagnostic = current_root_diagnostic(&second_cold_output)?;

        setup_cache(&second, &second_cargo_home, &first_cache)?;
        let reconstructed_cache_root = profile_cache_root(&second, &second_cargo_home)?;
        fs::remove_dir_all(second.join("target"))?;
        let root_bound_events = fs::canonicalize(root.path())?.join("root-bound-events");
        create_private_directory(&root_bound_events)?;
        let root_bound_events_value = root_bound_events.to_string_lossy().into_owned();
        let (root_bound_output, root_bound_cold) = run_cargo(
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
        let root_bound_hits = benchmark_action_keys(&root_bound_events, "hit")?;
        let root_bound_misses = benchmark_action_keys(&root_bound_events, "miss")?;
        ensure_typed_benchmark_events(&root_bound_events)?;
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
            "root-bound reconstruction changed the eligible action count: original={second_cold:?}, reconstructed={root_bound_cold:?}, events={:?}\ncompiler stderr:\n{}",
            benchmark_event_summary(&root_bound_events)?,
            String::from_utf8_lossy(&root_bound_output.stderr)
        );
        let reconstructed_keys = native_action_keys(&reconstructed_cache_root)?;
        ensure!(
            root_bound_misses.len() as u64 == root_bound_cold.misses
                && root_bound_misses.iter().all(|key| reconstructed_keys.contains(key)),
            "root-bound reconstruction did not publish every action missed by the current execution: \
     usage={root_bound_cold:?}, reconstructed={}, misses={root_bound_misses:?}, events={:?}",
            reconstructed_keys.len(),
            benchmark_event_summary(&root_bound_events)?
        );
        let root_bound_outputs = reusable_outputs(&second.join("target"))?;
        #[cfg(not(windows))]
        ensure!(
            root_bound_outputs == second_outputs,
            "root-bound output reconstruction changed reusable outputs"
        );
        // Correct root-bound misses perform a second cold compilation. Windows
        // PE/COFF images and native archives can embed build-time data, so that
        // compilation must reproduce the output inventory, not identical bytes.
        // The same-root warm phase below remains byte-exact outside its explicitly
        // reported native miss closure.
        #[cfg(windows)]
        ensure!(
            root_bound_outputs.keys().eq(second_outputs.keys()),
            "root-bound output reconstruction changed the reusable output inventory"
        );

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
        ensure_typed_benchmark_events(&second_warm_events)?;
        let second_warm_miss_crates = benchmark_action_crates(&second_warm_events, "miss")?;
        #[cfg(not(windows))]
        ensure!(
            second_warm.hits.saturating_add(second_warm.misses) == second_keys.len() as u64,
            "same-root warm reconstruction changed the eligible action count: expected={}, usage={second_warm:?}, \
     misses={second_warm_miss_crates:?}, events={second_warm_summary:?}",
            second_keys.len(),
        );
        #[cfg(not(windows))]
        ensure!(
            second_warm.hits == second_keys.len() as u64 && second_warm.misses == 0,
            "same-root warm restore was not clean: {second_warm:?}"
        );
        #[cfg(windows)]
        ensure!(
            second_warm.hits.saturating_add(second_warm.misses) == second_keys.len() as u64,
            "Windows same-root warm restore changed the previously eligible action count: \
     expected={}, usage={second_warm:?}, misses={second_warm_miss_crates:?}, events={second_warm_summary:?}",
            second_keys.len(),
        );
        #[cfg(windows)]
        let second_warm_uncached_crates = {
            let mut crates = second_warm_miss_crates.clone();
            crates.extend(benchmark_action_crates(&second_warm_events, "bypassed")?);
            crates.sort();
            crates.dedup();
            crates
        };
        #[cfg(windows)]
        ensure_windows_native_miss_boundary(second_warm, &second_warm_miss_crates, &second_warm_summary, "check")?;
        ensure!(
            second_warm.failures == 0,
            "same-root warm restore failed: {second_warm:?}"
        );
        let second_warm_outputs = reusable_outputs(&second.join("target"))?;
        #[cfg(not(windows))]
        ensure!(
            second_warm_outputs == root_bound_outputs,
            "same-root warm restore changed the outputs published by its cold producer"
        );
        #[cfg(windows)]
        ensure_windows_hit_outputs_are_exact(
            &root_bound_outputs,
            &second_warm_outputs,
            &second_warm_uncached_crates,
            "check",
        )?;
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

        Ok(())
    })();
    if result.is_err() {
        eprintln!("retained failed compiler-cache fixture: {}", root.keep().display());
    }
    result
}

#[test]
fn real_cargo_build_reuses_exact_outputs() -> Result<()> {
    let root = tempfile::tempdir()?;
    let result: Result<()> = (|| {
        let first = root.path().join("first");
        let git_source = root.path().join("git-source");
        let first_cargo_home = root.path().join("first-cargo-home");
        let build_cache = root.path().join("build-cache");
        materialize_fixture(&first, &git_source)?;
        add_current_root_diagnostic(&first)?;
        seed_isolated_cargo_home(&first, &first_cargo_home)?;
        setup_cache(&first, &first_cargo_home, &build_cache)?;
        let build_cache_root = profile_cache_root(&first, &first_cargo_home)?;
        ensure!(
            native_action_keys(&build_cache_root)?.is_empty(),
            "release build selected a cache containing native actions"
        );
        let build_cold_events = fs::canonicalize(root.path())?.join("build-cold-events");
        create_private_directory(&build_cold_events)?;
        let build_cold_events_value = build_cold_events.to_string_lossy().into_owned();
        let (_, build_cold) = run_cargo(
            &first,
            &first_cargo_home,
            "build",
            &[
                ("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1"),
                (
                    "CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY",
                    build_cold_events_value.as_str(),
                ),
            ],
        )?;
        ensure_typed_benchmark_events(&build_cold_events)?;
        let build_cold_hits = benchmark_action_crates(&build_cold_events, "hit")?;
        #[cfg(not(windows))]
        let build_cold_miss_keys = benchmark_action_keys(&build_cold_events, "miss")?;
        ensure!(
            build_cold.hits == 0 && build_cold.misses >= 8 && build_cold.failures == 0,
            "release build crossed a non-cold cache boundary: \
         usage={build_cold:?}, hits={build_cold_hits:?}, events={:?}",
            benchmark_event_summary(&build_cold_events)?
        );
        let build_outputs = reusable_outputs(&first.join("target/release"))?;
        let binary = executable(first.join("target/release/fixture-cli"));
        let dylib = dynamic_library(&first.join("target/release"), "fixture_dylib");
        let cdylib = dynamic_library(&first.join("target/release"), "fixture_cdylib");
        ensure!(
            dylib.is_file() && cdylib.is_file(),
            "dynamic library outputs are missing"
        );
        let cold_binary = Command::new(&binary).output()?;
        ensure!(cold_binary.status.success());
        ensure!(String::from_utf8_lossy(&cold_binary.stdout).trim() == "119");

        fs::remove_dir_all(first.join("target"))?;
        let build_warm_events = fs::canonicalize(root.path())?.join("build-warm-events");
        create_private_directory(&build_warm_events)?;
        let build_warm_events_value = build_warm_events.to_string_lossy().into_owned();
        let (_build_warm_output, build_warm) = run_cargo(
            &first,
            &first_cargo_home,
            "build",
            &[
                ("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1"),
                (
                    "CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY",
                    build_warm_events_value.as_str(),
                ),
            ],
        )?;
        #[cfg(windows)]
        let (cold_actions, warm_actions) = (
            build_cold
                .hits
                .saturating_add(build_cold.misses)
                .saturating_add(build_cold.bypasses),
            build_warm
                .hits
                .saturating_add(build_warm.misses)
                .saturating_add(build_warm.bypasses),
        );
        #[cfg(windows)]
        ensure!(
            warm_actions == cold_actions,
            "warm build changed the classified action count: cold={build_cold:?}, warm={build_warm:?}"
        );
        #[cfg(not(windows))]
        {
            let build_warm_hit_keys = benchmark_action_keys(&build_warm_events, "hit")?;
            ensure!(
                build_cold_miss_keys.len() as u64 == build_cold.misses
                    && build_warm_hit_keys.len() as u64 == build_warm.hits,
                "release-build usage and action ledgers disagree: cold={build_cold:?}, warm={build_warm:?}"
            );
            let cold_keys = build_cold_miss_keys.into_iter().collect::<BTreeSet<_>>();
            let warm_keys = build_warm_hit_keys.into_iter().collect::<BTreeSet<_>>();
            let unexpected = warm_keys.difference(&cold_keys).collect::<Vec<_>>();
            // Cargo owns unit scheduling and freshness. Earlier restores can make a
            // cold-equivalent unit fresh before it reaches the wrapper, so the warm
            // invocation census may be smaller. Every invocation that does reach L1
            // must still use authority published by this cold build.
            ensure!(
                !warm_keys.is_empty() && unexpected.is_empty(),
                "warm build used unexpected action authority: unexpected={unexpected:?}, cold={build_cold:?}, \
             warm={build_warm:?}"
            );
            ensure!(
                build_warm.misses == 0,
                "warm build was not clean: {build_warm:?}, misses={:?}, cold={:?}, warm={:?}",
                benchmark_action_crates(&build_warm_events, "miss")?,
                benchmark_event_summary(&build_cold_events)?,
                benchmark_event_summary(&build_warm_events)?
            );
        }
        #[cfg(windows)]
        let build_warm_miss_crates = benchmark_action_crates(&build_warm_events, "miss")?;
        #[cfg(windows)]
        let build_warm_uncached_crates = {
            let mut crates = build_warm_miss_crates.clone();
            crates.extend(benchmark_action_crates(&build_warm_events, "bypassed")?);
            crates.sort();
            crates.dedup();
            crates
        };
        #[cfg(windows)]
        ensure_windows_native_miss_boundary(
            build_warm,
            &build_warm_miss_crates,
            &benchmark_event_summary(&build_warm_events)?,
            "build",
        )?;
        ensure!(build_warm.failures == 0);
        ensure_typed_benchmark_events(&build_warm_events)?;
        #[cfg(not(windows))]
        {
            let hits = benchmark_action_crates(&build_warm_events, "hit")?;
            ensure!(
                hits.iter().any(|name| name == "fixture_dylib"),
                "the ordinary Rust dynamic library was not restored: {hits:?}"
            );
            for (directory, phase) in [(&build_cold_events, "cold"), (&build_warm_events, "warm")] {
                let events = benchmark_events(directory)?;
                let static_reason = if cfg!(any(target_arch = "aarch64", target_arch = "x86_64")) {
                    "compiler_assembly_input_evidence_unavailable"
                } else {
                    "compiler_lto_assembly_evidence_unavailable"
                };
                for (crate_name, reason) in [
                    ("fixture_static", static_reason),
                    ("fixture_cdylib", "compiler_lto_assembly_evidence_unavailable"),
                ] {
                    let actions = events
                        .iter()
                        .filter(|event| event["action"]["crate_name"] == crate_name)
                        .collect::<Vec<_>>();
                    ensure!(
                        actions.len() == 1 && actions[0]["status"] == "bypassed" && actions[0]["reason"] == reason,
                        "{phase} {crate_name} did not retain its declared unsupported-input boundary: {actions:?}"
                    );
                }
            }
        }

        let build_warm_outputs = reusable_outputs(&first.join("target/release"))?;
        #[cfg(not(windows))]
        ensure!(build_warm_outputs == build_outputs);
        #[cfg(windows)]
        ensure_windows_hit_outputs_are_exact(
            &build_outputs,
            &build_warm_outputs,
            &build_warm_uncached_crates,
            "build",
        )?;
        ensure!(
            dylib.is_file() && cdylib.is_file(),
            "restored dynamic library outputs are missing"
        );
        let warm_binary = Command::new(binary).output()?;
        ensure!(warm_binary.status.success());
        ensure!(String::from_utf8_lossy(&warm_binary.stdout).trim() == "119");

        Ok(())
    })();
    if result.is_err() {
        eprintln!("retained failed compiler-cache fixture: {}", root.keep().display());
    }
    result
}

#[test]
fn real_cargo_test_targets_reuse_exact_outputs() -> Result<()> {
    let root = tempfile::tempdir()?;
    let result: Result<()> = (|| {
        let first = root.path().join("first");
        let git_source = root.path().join("git-source");
        let first_cargo_home = root.path().join("first-cargo-home");
        let build_cache = root.path().join("build-cache");
        materialize_fixture(&first, &git_source)?;
        add_current_root_diagnostic(&first)?;
        seed_isolated_cargo_home(&first, &first_cargo_home)?;
        setup_cache(&first, &first_cargo_home, &build_cache)?;
        let test_cold_events = fs::canonicalize(root.path())?.join("test-cold-events");
        create_private_directory(&test_cold_events)?;
        let test_cold_events_value = test_cold_events.to_string_lossy().into_owned();
        let (_, test_cold) = run_cargo(
            &first,
            &first_cargo_home,
            "test",
            &[
                ("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1"),
                (
                    "CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY",
                    test_cold_events_value.as_str(),
                ),
            ],
        )?;
        ensure!(
            test_cold.failures == 0,
            "test-target cold compile failed: {test_cold:?}"
        );
        ensure_typed_benchmark_events(&test_cold_events)?;
        let new_test_targets = ["fixture_cli_bench", "fixture_cli_example", "fixture_cli_smoke"];
        #[cfg(not(windows))]
        {
            let misses = benchmark_action_crates(&test_cold_events, "miss")?;
            for target in new_test_targets {
                ensure!(
                    misses.iter().any(|crate_name| crate_name == target),
                    "test-target cold compile did not publish {target}: {misses:?}"
                );
            }
        }
        #[cfg(windows)]
        {
            let bypasses = benchmark_action_crates(&test_cold_events, "bypassed")?;
            for target in new_test_targets {
                ensure!(
                    bypasses.iter().any(|crate_name| crate_name == target),
                    "COFF test-target compile did not preserve the explicit cold boundary for {target}: {bypasses:?}"
                );
            }
        }

        fs::remove_dir_all(first.join("target"))?;
        let test_warm_events = fs::canonicalize(root.path())?.join("test-warm-events");
        create_private_directory(&test_warm_events)?;
        let test_warm_events_value = test_warm_events.to_string_lossy().into_owned();
        let (_, test_warm) = run_cargo(
            &first,
            &first_cargo_home,
            "test",
            &[
                ("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1"),
                (
                    "CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY",
                    test_warm_events_value.as_str(),
                ),
            ],
        )?;
        ensure!(
            test_warm.failures == 0,
            "test-target warm compile failed: {test_warm:?}"
        );
        ensure_typed_benchmark_events(&test_warm_events)?;
        #[cfg(not(windows))]
        {
            let hits = benchmark_action_crates(&test_warm_events, "hit")?;
            for target in new_test_targets {
                ensure!(
                    hits.iter().any(|crate_name| crate_name == target),
                    "test-target warm compile did not restore {target}: {hits:?}"
                );
            }
        }
        #[cfg(windows)]
        ensure_windows_native_miss_boundary(
            test_warm,
            &benchmark_action_crates(&test_warm_events, "miss")?,
            &benchmark_event_summary(&test_warm_events)?,
            "test targets",
        )?;
        Ok(())
    })();
    if result.is_err() {
        eprintln!("retained failed compiler-cache fixture: {}", root.keep().display());
    }
    result
}

#[test]
fn real_world_native_cache_fixture_exercises_required_compiler_classes() -> Result<()> {
    let root = tempfile::tempdir()?;
    let fixture = root.path().join("fixture");
    let target = fixture.join("target");
    let cargo_home = root.path().join("cargo-home");
    materialize_fixture(&fixture, &root.path().join("git-source"))?;
    seed_isolated_cargo_home(&fixture, &cargo_home)?;

    let metadata = cargo_metadata(&fixture, Some(&cargo_home))?;
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
    for required_kind in ["cdylib", "dylib", "staticlib"] {
        ensure!(packages.iter().any(|package| {
            package["targets"].as_array().into_iter().flatten().any(|target| {
                target["kind"]
                    .as_array()
                    .is_some_and(|kinds| kinds.iter().any(|kind| kind == required_kind))
            })
        }));
    }
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

    let integrated_assembly = fs::read_to_string(fixture.join("crates/fixture-static/src/lib.rs"))?;
    for required in [
        "core::arch::asm!",
        "core::arch::global_asm!(include_str!",
        "core::arch::naked_asm!",
        "target_feature(enable",
    ] {
        ensure!(
            integrated_assembly.contains(required),
            "fixture does not exercise required integrated-assembly shape {required}"
        );
    }
    ensure!(
        fs::metadata(fixture.join("crates/fixture-static/src/integrated_assembly.s"))?.len() > 0,
        "fixture included assembly text is empty"
    );

    let check = cargo_process(&fixture, Some(&cargo_home))
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
    let build = cargo_process(&fixture, Some(&cargo_home))
        .args(["build", "--workspace", "--all-features", "--locked", "--offline"])
        .output()?;
    ensure!(build.status.success(), "fixture build failed");
    let test = cargo_process(&fixture, Some(&cargo_home))
        .args([
            "test",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--no-run",
            "--locked",
            "--offline",
        ])
        .output()?;
    ensure!(test.status.success(), "fixture test-target compile failed");
    ensure!(executable(target.join("debug/fixture-cli")).is_file());
    ensure!(static_archive(&target.join("debug")).is_file());
    ensure!(dynamic_library(&target.join("debug"), "fixture_dylib").is_file());
    ensure!(dynamic_library(&target.join("debug"), "fixture_cdylib").is_file());
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn write_native_input_fixture(root: &Path, name: &str, source: &str) -> Result<PathBuf> {
    let fixture = root.join("fixture");
    fs::create_dir_all(fixture.join("src"))?;
    fs::write(
        fixture.join("Cargo.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
    )?;
    fs::write(
        fixture.join("Cargo.lock"),
        format!("version = 4\n\n[[package]]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
    )?;
    fs::write(fixture.join("src/lib.rs"), source)?;
    Ok(fixture)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn native_input_outputs(target: &Path) -> Result<BTreeMap<PathBuf, (String, u32)>> {
    use std::os::unix::fs::PermissionsExt as _;

    reusable_outputs(target)?
        .into_iter()
        .map(|(path, digest)| {
            let mode = fs::metadata(target.join(&path))?.permissions().mode() & 0o777;
            Ok((path, (digest, mode)))
        })
        .collect()
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn native_input_diagnostics(output: &Output) -> Result<Vec<serde_json::Value>> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .filter_map(|event| match event {
            Ok(event) if event["reason"] == "compiler-message" => Some(Ok(event["message"].clone())),
            Ok(_) => None,
            Err(error) => Some(Err(error.into())),
        })
        .collect()
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn native_assembly_inputs_bypass_after_proven_ordinary_cold_and_warm_reuse() -> Result<()> {
    let root = tempfile::tempdir()?;
    let fixture = write_native_input_fixture(
        root.path(),
        "native-assembly-input",
        "#![no_std]\npub fn value() -> u64 { 41 }\npub fn compiler_fallback_path() -> Option<&'static str> { option_env!(\"DYLD_FALLBACK_LIBRARY_PATH\") }\n",
    )?;
    let target = fixture.join("target");
    let baseline_home = root.path().join("baseline-home");
    let cached_home = root.path().join("cached-home");
    fs::create_dir(&baseline_home)?;
    fs::create_dir(&cached_home)?;
    let baseline = cargo_command(&fixture, &baseline_home, "build").output()?;
    ensure!(baseline.status.success(), "ordinary baseline failed: {baseline:?}");
    let expected = native_input_outputs(&target)?;
    ensure!(!expected.is_empty(), "ordinary baseline produced no native outputs");
    fs::remove_dir_all(&target)?;
    setup_cache(&fixture, &cached_home, &root.path().join("cache"))?;
    let events = root.path().join("ordinary-cold-events");
    create_private_directory(&events)?;
    let events = fs::canonicalize(events)?;
    let (cold, usage) = run_cargo(
        &fixture,
        &cached_home,
        "build",
        &[
            ("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1"),
            (
                "CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY",
                events.to_str().context("event path")?,
            ),
        ],
    )?;
    ensure!(
        usage.misses == 1 && usage.hits == 0 && usage.bypasses == 0 && usage.failures == 0,
        "ordinary cold did not execute a cacheable compiler action: {usage:?}\n{cold:?}\n{:?}",
        benchmark_event_summary(&events)?
    );
    ensure!(
        native_input_outputs(&target)? == expected,
        "ordinary cold outputs or modes differ from rustc"
    );
    ensure!(
        native_input_diagnostics(&cold)? == native_input_diagnostics(&baseline)?,
        "ordinary cold compiler diagnostics differ"
    );
    fs::remove_dir_all(&target)?;
    let (warm, usage) = run_cargo(&fixture, &cached_home, "build", &[])?;
    ensure!(
        usage.hits == 1 && usage.misses == 0 && usage.bypasses == 0 && usage.failures == 0,
        "ordinary warm did not restore a real compiler result: {usage:?}"
    );
    ensure!(
        native_input_outputs(&target)? == expected,
        "ordinary warm output bytes or modes changed"
    );
    ensure!(
        native_input_diagnostics(&warm)? == native_input_diagnostics(&baseline)?,
        "ordinary warm compiler diagnostics differ"
    );

    let blob = root.path().join("external-assembly.bin");
    let section = if cfg!(target_os = "macos") {
        ".section __TEXT,__const"
    } else {
        ".section .rodata"
    };
    fs::write(
        fixture.join("src/lib.rs"),
        format!(
            "#![no_std]\ncore::arch::global_asm!(r#\"{section}\n.incbin \"{}\"\n\"#);\npub fn value() -> u64 {{ 41 }}\n",
            blob.display()
        ),
    )?;
    let mut previous = None;
    for (phase, bytes) in [("initial", b"initial!"), ("mutated", b"changed!")] {
        fs::write(&blob, bytes)?;
        fs::remove_dir_all(&target)?;
        let baseline = cargo_command(&fixture, &baseline_home, "build").output()?;
        ensure!(
            baseline.status.success(),
            "{phase} assembly baseline failed: {baseline:?}"
        );
        let expected = native_input_outputs(&target)?;
        if let Some(previous) = previous {
            ensure!(
                expected != previous,
                "same-size .incbin mutation did not change the ordinary output"
            );
        }
        fs::remove_dir_all(&target)?;
        let events = root.path().join(format!("{phase}-events"));
        create_private_directory(&events)?;
        let events = fs::canonicalize(events)?;
        let events_value = events.to_str().context("assembly event path")?;
        let (cached, usage) = run_cargo(
            &fixture,
            &cached_home,
            "build",
            &[
                ("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1"),
                ("CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY", events_value),
            ],
        )?;
        ensure!(
            usage.bypasses == 1 && usage.hits == 0 && usage.misses == 0 && usage.failures == 0,
            "{phase} assembly was incorrectly reused: {usage:?}"
        );
        ensure!(
            benchmark_events(&events)?
                .iter()
                .any(|event| event["status"] == "bypassed"
                    && event["reason"] == "compiler_assembly_input_evidence_unavailable"),
            "{phase} bypass omitted the actual assembly reason: {:?}",
            benchmark_event_summary(&events)?
        );
        ensure!(
            cached.status.code() == baseline.status.code(),
            "{phase} compiler status changed"
        );
        ensure!(
            native_input_outputs(&target)? == expected,
            "{phase} assembly output bytes or modes differ from ordinary rustc"
        );
        ensure!(
            native_input_diagnostics(&cached)? == native_input_diagnostics(&baseline)?,
            "{phase} compiler diagnostics changed"
        );
        previous = Some(expected);
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn transitive_crate_replacement_cannot_restore_a_result_for_stale_direct_metadata() -> Result<()> {
    let root = tempfile::tempdir()?;
    let source = "#![no_std]\npub fn value() -> u64 { b::value() }\n";
    let fixture = write_native_input_fixture(root.path(), "native-transitive-input", source)?;
    let dependencies = fixture.join("deps");
    let producers = root.path().join("producers");
    let baseline_home = root.path().join("baseline-home");
    let cached_home = root.path().join("cached-home");
    for directory in [&dependencies, &producers, &baseline_home, &cached_home] {
        fs::create_dir(directory)?;
    }
    let sysroot = Command::new("rustc").args(["--print", "sysroot"]).output()?;
    ensure!(
        sysroot.status.success(),
        "selected rustc sysroot query failed: {sysroot:?}"
    );
    let rustc = PathBuf::from(String::from_utf8(sysroot.stdout)?.trim()).join("bin/rustc");
    let a = dependencies.join("liba.rlib");
    let b = dependencies.join("libb.rlib");
    let compile_a = |value: u64| -> Result<()> {
        let input = producers.join("a.rs");
        fs::write(&input, format!("#![no_std]\npub const VALUE: u64 = {value};\n"))?;
        let output = Command::new(&rustc)
            .current_dir(&fixture)
            .args(["--crate-name", "a", "--crate-type", "rlib"])
            .arg(input)
            .arg("-o")
            .arg(&a)
            .env_remove("RUSTC_WRAPPER")
            .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER")
            .output()?;
        ensure!(output.status.success(), "ordinary A producer failed: {output:?}");
        Ok(())
    };
    compile_a(41)?;
    let original_a = fs::read(&a)?;
    let b_source = producers.join("b.rs");
    fs::write(
        &b_source,
        "#![no_std]\nextern crate a;\npub fn value() -> u64 { a::VALUE }\n",
    )?;
    let output = Command::new(&rustc)
        .current_dir(&fixture)
        .args(["--crate-name", "b", "--crate-type", "rlib"])
        .arg(&b_source)
        .arg("--extern")
        .arg(format!("a={}", a.display()))
        .arg("-L")
        .arg(format!("dependency={}", dependencies.display()))
        .arg("-o")
        .arg(&b)
        .env_remove("RUSTC_WRAPPER")
        .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER")
        .output()?;
    ensure!(output.status.success(), "ordinary B producer failed: {output:?}");
    let original_b = fs::read(&b)?;
    let flags = format!(
        "--extern\u{1f}b={}\u{1f}-L\u{1f}dependency={}",
        b.display(),
        dependencies.display()
    );
    let baseline = cargo_command(&fixture, &baseline_home, "check")
        .env("CARGO_ENCODED_RUSTFLAGS", &flags)
        .output()?;
    ensure!(baseline.status.success(), "ordinary C baseline failed: {baseline:?}");
    let target = fixture.join("target");
    let expected = native_input_outputs(&target)?;
    ensure!(!expected.is_empty(), "ordinary C produced no metadata");
    fs::remove_dir_all(&target)?;
    setup_cache(&fixture, &cached_home, &root.path().join("cache"))?;
    let events = root.path().join("ordinary-cold-events");
    create_private_directory(&events)?;
    let events = fs::canonicalize(events)?;
    let (cold, usage) = run_cargo(
        &fixture,
        &cached_home,
        "check",
        &[
            ("CARGO_ENCODED_RUSTFLAGS", &flags),
            ("CARGO_RAIL_CACHE", "__cargo_rail_benchmark_coverage_v1"),
            (
                "CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY",
                events.to_str().context("event path")?,
            ),
        ],
    )?;
    ensure!(
        usage.misses == 1 && usage.hits == 0 && usage.bypasses == 0 && usage.failures == 0,
        "C cold did not publish a real result: {usage:?}\n{cold:?}\n{:?}",
        benchmark_event_summary(&events)?
    );
    ensure!(
        native_input_outputs(&target)? == expected,
        "C cold metadata or modes differ from ordinary rustc"
    );
    ensure!(
        native_input_diagnostics(&cold)? == native_input_diagnostics(&baseline)?,
        "C cold diagnostics differ"
    );
    fs::remove_dir_all(&target)?;
    let (_, usage) = run_cargo(&fixture, &cached_home, "check", &[("CARGO_ENCODED_RUSTFLAGS", &flags)])?;
    ensure!(
        usage.hits == 1 && usage.misses == 0 && usage.bypasses == 0 && usage.failures == 0,
        "C warm did not restore the real result: {usage:?}"
    );
    ensure!(
        native_input_outputs(&target)? == expected,
        "C warm output bytes or modes changed"
    );

    compile_a(42)?;
    let replaced_a = fs::read(&a)?;
    ensure!(
        replaced_a.len() == original_a.len() && replaced_a != original_a,
        "A replacement must be valid changed bytes with exactly the same size"
    );
    ensure!(
        fs::read(&b)? == original_b,
        "B metadata changed during the transitive mutation"
    );
    ensure!(
        fs::read_to_string(fixture.join("src/lib.rs"))? == source,
        "C source changed during the transitive mutation"
    );
    fs::remove_dir_all(&target)?;
    let baseline = cargo_command(&fixture, &baseline_home, "check")
        .env("CARGO_ENCODED_RUSTFLAGS", &flags)
        .output()?;
    ensure!(
        !baseline.status.success(),
        "ordinary rustc accepted deliberately stale B"
    );
    ensure!(
        String::from_utf8_lossy(&baseline.stderr).contains("error[E0460]"),
        "ordinary rustc did not diagnose the changed transitive dependency: {baseline:?}"
    );
    fs::remove_dir_all(&target)?;
    let before = cache_usage(&fixture, &cached_home)?;
    let cached = cargo_command(&fixture, &cached_home, "check")
        .env("CARGO_ENCODED_RUSTFLAGS", &flags)
        .output()?;
    let usage = cache_usage(&fixture, &cached_home)?.difference(before);
    ensure!(
        usage.hits == 0 && usage.misses + usage.bypasses > 0 && usage.failures == 0,
        "stale transitive input reused an old C result: {usage:?}"
    );
    ensure!(
        !cached.status.success() && cached.status.code() == baseline.status.code(),
        "cached C masked the ordinary compiler failure: {cached:?}"
    );
    ensure!(
        cached.stdout == baseline.stdout && cached.stderr == baseline.stderr,
        "cached C changed the ordinary transitive compiler diagnostic: cached={cached:?}; baseline={baseline:?}"
    );
    Ok(())
}
